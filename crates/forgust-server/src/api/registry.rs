//! The container registry for people (ADR-0022): a project's images, their
//! tags, deleting them, and the cleanup policy. Clients use `/v2/` instead.

use axum::Json;
use axum::extract::{Path, State};
use forgust_db::registry::{self as rdb, CleanupPolicy, RepositorySummary, Tag};
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services;
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct ImageRepository {
    #[serde(flatten)]
    pub repository: RepositorySummary,
    /// What to pull: `host/group/project[/name]`.
    pub location: String,
}

#[derive(Serialize, ToSchema)]
pub struct RegistryInfo {
    pub enabled: bool,
    /// `CI_REGISTRY`: what `docker login` takes.
    pub host: String,
    /// `CI_REGISTRY_IMAGE`.
    pub image: String,
    pub repositories: Vec<ImageRepository>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/registry", responses((status = 200, body = RegistryInfo)), tag = "registry")]
pub async fn info(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<RegistryInfo>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let image = crate::registry::image_of(&state, &p.full_path);
    let repositories = rdb::repositories(&state.db, p.id)
        .await?
        .into_iter()
        .map(|r| ImageRepository { location: if r.name.is_empty() { image.clone() } else { format!("{image}/{}", r.name) }, repository: r })
        .collect();
    Ok(Json(RegistryInfo { enabled: state.cfg.registry.enabled, host: crate::registry::host(&state), image, repositories }))
}

pub(crate) async fn repo_of(state: &AppState, project_id: i64, repo_id: i64) -> ApiResult<rdb::Repository> {
    rdb::repository_by_id(&state.db, repo_id).await?.filter(|r| r.project_id == project_id).ok_or(ApiError::NotFound)
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/registry/repositories/{repository_id}/tags", responses((status = 200, body = [Tag])), tag = "registry")]
pub async fn tags(State(state): State<AppState>, Viewer(v): Viewer, Path((id, repo_id)): Path<(String, i64)>) -> ApiResult<Json<Vec<Tag>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let repo = repo_of(&state, p.id, repo_id).await?;
    let mut tags = rdb::tags(&state.db, repo.id, "", 10_000).await?;
    tags.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
    Ok(Json(tags))
}

pub async fn delete_tag(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path((id, repo_id, tag)): Path<(String, i64, String)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Push).await?;
    let repo = repo_of(&state, p.id, repo_id).await?;
    if !rdb::delete_tag(&state.db, repo.id, &tag).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(
        &state,
        &principal,
        "registry_tag_deleted",
        Some(("project", p.id)),
        json!({ "repository": repo.name, "tag": tag }),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// Delete an image repository with all its tags (its layers go with the next sweep).
pub async fn delete_repository(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path((id, repo_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    let repo = repo_of(&state, p.id, repo_id).await?;
    rdb::delete_repository(&state.db, repo.id).await?;
    services::audit(&state, &principal, "registry_repository_deleted", Some(("project", p.id)), json!({ "repository": repo.name }), None)
        .await;
    Ok(Json(json!({ "ok": true })))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/registry/cleanup_policy", responses((status = 200, body = CleanupPolicy)), tag = "registry")]
pub async fn cleanup_policy(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<CleanupPolicy>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    Ok(Json(rdb::cleanup_policy(&state.db, p.id).await?))
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/registry/cleanup_policy", request_body = CleanupPolicy, responses((status = 200, body = CleanupPolicy)), tag = "registry")]
pub async fn set_cleanup_policy(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(policy): Json<CleanupPolicy>,
) -> ApiResult<Json<CleanupPolicy>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    for (what, re) in [("tags to remove", &policy.name_regex), ("tags to keep", &policy.name_regex_keep)] {
        if !re.is_empty() && regex::Regex::new(re).is_err() {
            return Err(ApiError::bad(format!("the pattern of {what} is not a valid regular expression")));
        }
    }
    if policy.keep_n.is_some_and(|n| n < 0) || policy.older_than_days.is_some_and(|d| d < 0) {
        return Err(ApiError::bad("counts cannot be negative"));
    }
    rdb::set_cleanup_policy(&state.db, p.id, &policy).await?;
    services::audit(
        &state,
        &principal,
        "registry_cleanup_policy_changed",
        Some(("project", p.id)),
        serde_json::to_value(&policy).unwrap_or_default(),
        None,
    )
    .await;
    Ok(Json(rdb::cleanup_policy(&state.db, p.id).await?))
}

/// Run the cleanup policy now (it also runs daily): how many tags went.
pub async fn run_cleanup(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    let removed = crate::registry::cleanup(&state, p.id).await?;
    Ok(Json(json!({ "removed": removed })))
}
