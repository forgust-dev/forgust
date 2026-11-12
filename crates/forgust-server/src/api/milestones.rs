//! Milestones (M7).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use forgust_db::milestones::Milestone;
use serde::Deserialize;

use super::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::milestones::{self as svc, MilestoneBody};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct MilestonesQuery {
    /// active (default), closed or all
    state: Option<String>,
    #[serde(default)]
    search: String,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/milestones", responses((status = 200, body = Vec<Milestone>)), tag = "issues")]
pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<MilestonesQuery>,
) -> ApiResult<Json<Vec<Milestone>>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let st = q.state.as_deref().unwrap_or("active");
    if !["active", "closed", "all"].contains(&st) {
        return Err(ApiError::bad("state must be active, closed or all"));
    }
    Ok(Json(forgust_db::milestones::list(&state.db, p.id, st, &forgust_db::like_escape(q.search.trim())).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/milestones/{milestone_id}", responses((status = 200, body = Milestone)), tag = "issues")]
pub async fn show(State(state): State<AppState>, Viewer(v): Viewer, Path((id, mid)): Path<(String, i64)>) -> ApiResult<Json<Milestone>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    Ok(Json(svc::find(&state, &v, &p, mid).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/milestones", request_body = MilestoneBody, responses((status = 201, body = Milestone)), tag = "issues")]
pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<MilestoneBody>,
) -> ApiResult<(StatusCode, Json<Milestone>)> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    Ok((StatusCode::CREATED, Json(svc::create(&state, &v, &p, &b).await?)))
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/milestones/{milestone_id}", request_body = MilestoneBody, responses((status = 200, body = Milestone)), tag = "issues")]
pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, mid)): Path<(String, i64)>,
    Json(b): Json<MilestoneBody>,
) -> ApiResult<Json<Milestone>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    Ok(Json(svc::update(&state, &v, &p, mid, &b).await?))
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/milestones/{milestone_id}", responses((status = 204)), tag = "issues")]
pub async fn delete(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, mid)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    svc::delete(&state, &v, &p, mid).await?;
    Ok(StatusCode::NO_CONTENT)
}
