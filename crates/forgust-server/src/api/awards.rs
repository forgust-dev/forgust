//! Emoji reactions (M7).

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{SignedIn, Viewer};
use crate::error::ApiResult;
use crate::services::awards::{self as svc, On, Reactions};
use crate::state::AppState;

#[derive(Deserialize, ToSchema)]
pub struct ReactBody {
    /// thumbsup, thumbsdown, laughing, tada, confused, heart, rocket or eyes
    pub name: String,
    /// A comment of it, instead of the merge request or issue.
    pub note_id: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct Reacted {
    /// The reaction is there now.
    pub awarded: bool,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/reactions", responses((status = 200, body = Reactions)), tag = "merge_requests")]
pub async fn mr_reactions(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Reactions>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let all = svc::list(&state, &v, &p, On::MergeRequest { iid, note_id: None }).await?;
    Ok(Json(svc::summarize(&all, v.direct_user().map(|u| u.id))))
}

/// Add a reaction, or take it back when it is there.
#[utoipa::path(post, path = "/api/v1/projects/{id}/merge_requests/{iid}/reactions", request_body = ReactBody, responses((status = 200, body = Reacted)), tag = "merge_requests")]
pub async fn mr_react(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(b): Json<ReactBody>,
) -> ApiResult<Json<Reacted>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let awarded = svc::toggle(&state, &v, &p, On::MergeRequest { iid, note_id: b.note_id }, &b.name).await?;
    Ok(Json(Reacted { awarded }))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/issues/{iid}/reactions", responses((status = 200, body = Reactions)), tag = "issues")]
pub async fn issue_reactions(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Reactions>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let all = svc::list(&state, &v, &p, On::Issue { iid, note_id: None }).await?;
    Ok(Json(svc::summarize(&all, v.direct_user().map(|u| u.id))))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/issues/{iid}/reactions", request_body = ReactBody, responses((status = 200, body = Reacted)), tag = "issues")]
pub async fn issue_react(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(b): Json<ReactBody>,
) -> ApiResult<Json<Reacted>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let awarded = svc::toggle(&state, &v, &p, On::Issue { iid, note_id: b.note_id }, &b.name).await?;
    Ok(Json(Reacted { awarded }))
}
