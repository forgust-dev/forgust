//! Environments and deployments: what was deployed where, stopping an
//! environment, and who may deploy (protected environments).

use axum::Json;
use axum::extract::{Path, Query, State};
use forgust_db::environments::{self as edb, Deployment, Environment, EnvironmentSummary, ProtectedEnvironment};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    /// available (default), stopped, or all.
    state: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/environments", responses((status = 200, body = [EnvironmentSummary])), tag = "ci")]
pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<EnvironmentSummary>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let filter = match q.state.as_deref() {
        None | Some("available") => Some("available"),
        Some("stopped") => Some("stopped"),
        Some("all") => None,
        Some(_) => return Err(ApiError::bad("state is available, stopped or all")),
    };
    let mut conn = state.db.acquire().await?;
    Ok(Json(edb::list(&mut conn, p.id, filter).await?))
}

#[derive(Serialize, ToSchema)]
pub struct EnvironmentDetail {
    #[serde(flatten)]
    pub environment: Environment,
    pub deployments: Vec<Deployment>,
    pub protected: bool,
    pub can_stop: bool,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/environments/{env_id}", responses((status = 200, body = EnvironmentDetail)), tag = "ci")]
pub async fn show(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, env_id)): Path<(String, i64)>,
) -> ApiResult<Json<EnvironmentDetail>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let e = edb::find(&state.db, p.id, env_id).await?.ok_or(ApiError::NotFound)?;
    let deployments = edb::deployments(&state.db, e.id, 100).await?;
    let protected = edb::protected(&state.db, p.id).await?.iter().any(|r| r.matches(&e.name));
    let can_stop = crate::auth::authz::can_project(&state, &v, ProjectAction::Push, &p).await?;
    Ok(Json(EnvironmentDetail { environment: e, deployments, protected, can_stop }))
}

/// Stop an environment: its `on_stop` job runs (as you), or it is marked stopped.
pub async fn stop(
    State(state): State<AppState>,
    SignedIn(me, principal): SignedIn,
    Path((id, env_id)): Path<(String, i64)>,
) -> ApiResult<Json<Environment>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Push).await?;
    let e = edb::find(&state.db, p.id, env_id).await?.ok_or(ApiError::NotFound)?;
    // stopping a protected environment is deploying to it
    let rules = edb::protected(&state.db, p.id).await?;
    let level =
        if me.is_admin { forgust_db::access::OWNER } else { forgust_db::authz::project_level(&state.db, me.id, p.id).await?.unwrap_or(0) };
    if rules.iter().any(|r| r.matches(&e.name) && level < r.deploy_access_level) {
        return Err(ApiError::forbidden("this environment is protected: you may not stop it"));
    }
    crate::ci::environments::stop(&state, p.id, e.id, Some(me.id)).await?;
    services::audit(&state, &principal, "environment_stopped", Some(("project", p.id)), json!({ "environment": e.name }), None).await;
    Ok(Json(edb::find(&state.db, p.id, env_id).await?.ok_or(ApiError::NotFound)?))
}

/// Delete a stopped environment and its deployment history.
pub async fn delete(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path((id, env_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    if !edb::delete(&state.db, p.id, env_id).await? {
        return Err(ApiError::conflict("only a stopped environment can be deleted"));
    }
    Ok(Json(json!({ "ok": true })))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/protected_environments", responses((status = 200, body = [ProtectedEnvironment])), tag = "ci")]
pub async fn protected(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<ProtectedEnvironment>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    Ok(Json(edb::protected(&state.db, p.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct ProtectEnvironment {
    /// A name or a glob (`review/*`).
    pub name: String,
    /// developer, maintainer (default) or owner.
    #[serde(default)]
    pub deploy_access_level: Option<String>,
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/protected_environments", request_body = ProtectEnvironment, responses((status = 200, body = [ProtectedEnvironment])), tag = "ci")]
pub async fn protect(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<ProtectEnvironment>,
) -> ApiResult<Json<Vec<ProtectedEnvironment>>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 255 || globset::Glob::new(name).is_err() {
        return Err(ApiError::bad("invalid environment name or pattern"));
    }
    let level = match req.deploy_access_level.as_deref().unwrap_or("maintainer") {
        "developer" => forgust_db::access::DEVELOPER,
        "maintainer" => forgust_db::access::MAINTAINER,
        "owner" => forgust_db::access::OWNER,
        _ => return Err(ApiError::bad("deploy_access_level is developer, maintainer or owner")),
    };
    edb::protect(&state.db, p.id, name, level).await?;
    services::audit(&state, &principal, "environment_protected", Some(("project", p.id)), json!({ "name": name, "level": level }), None)
        .await;
    Ok(Json(edb::protected(&state.db, p.id).await?))
}

pub async fn unprotect(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path((id, pe_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    if !edb::unprotect(&state.db, p.id, pe_id).await? {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}
