//! `award_emoji` on issues and merge requests, as GitLab's API has them.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use forgust_db::awards::Award;
use serde::Deserialize;
use serde_json::{Value, json};

use super::json as j;
use crate::api::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::awards::{self as svc, On};
use crate::state::AppState;

fn award_json(state: &AppState, a: &Award, iid: i64, kind: &str) -> Value {
    json!({
        "id": a.id,
        "name": a.name,
        "user": j::person(&state.cfg, Some(a.user_id), Some(&a.username), Some(&a.user_name), None),
        "created_at": j::ts(&a.created_at),
        "updated_at": j::ts(&a.created_at),
        "awardable_id": a.merge_request_id.or(a.issue_id),
        "awardable_type": kind,
        "awardable_iid": iid,
    })
}

fn on(kind: &str, iid: i64) -> On {
    if kind == "merge_requests" { On::MergeRequest { iid, note_id: None } } else { On::Issue { iid, note_id: None } }
}

fn type_name(kind: &str) -> &'static str {
    if kind == "merge_requests" { "MergeRequest" } else { "Issue" }
}

fn check_kind(kind: &str) -> ApiResult<()> {
    if kind == "merge_requests" || kind == "issues" { Ok(()) } else { Err(ApiError::NotFound) }
}

pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, kind, iid)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    check_kind(&kind)?;
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    crate::auth::authz::writable(&p)?;
    let all = svc::list(&state, &v, &p, on(&kind, iid)).await?;
    let mine: Vec<Value> = all
        .iter()
        .filter(|a| a.merge_request_id.is_some() || a.issue_id.is_some())
        .map(|a| award_json(&state, a, iid, type_name(&kind)))
        .collect();
    Ok(Json(json!(mine)))
}

#[derive(Deserialize)]
pub struct AwardBody {
    name: String,
}

pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, kind, iid)): Path<(String, String, i64)>,
    Json(b): Json<AwardBody>,
) -> ApiResult<Response> {
    check_kind(&kind)?;
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    // GitLab refuses the same award twice
    let award_id =
        svc::add(&state, &v, &p, on(&kind, iid), &b.name).await?.ok_or_else(|| ApiError::bad("Award Emoji Name has already been taken"))?;
    let all = svc::list(&state, &v, &p, on(&kind, iid)).await?;
    let a = all.iter().find(|a| a.id == award_id).ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(award_json(&state, a, iid, type_name(&kind)))).into_response())
}

pub async fn delete(
    State(state): State<AppState>,
    SignedIn(u, v): SignedIn,
    Path((id, kind, iid, award)): Path<(String, String, i64, i64)>,
) -> ApiResult<StatusCode> {
    check_kind(&kind)?;
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let all = svc::list(&state, &v, &p, on(&kind, iid)).await?;
    if !all.iter().any(|a| a.id == award && (a.merge_request_id.is_some() || a.issue_id.is_some())) {
        return Err(ApiError::NotFound);
    }
    if !forgust_db::awards::delete_own(&state.db, award, u.id).await? {
        return Err(ApiError::forbidden("only your own reactions can be removed"));
    }
    Ok(StatusCode::NO_CONTENT)
}
