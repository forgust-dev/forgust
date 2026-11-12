//! Project milestones, as GitLab's API has them (`glab milestone`, Renovate's
//! `milestone` option).

use axum::Json;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use forgust_db::milestones::Milestone;
use forgust_db::projects::Project;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{PageQuery, json as j, paged};
use crate::api::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{SignedIn, Viewer};
use crate::error::ApiResult;
use crate::services::milestones::{self as svc, MilestoneBody};
use crate::state::AppState;

fn milestone_json(state: &AppState, p: &Project, m: &Milestone) -> Value {
    json!({
        "id": m.id,
        "iid": m.iid,
        "project_id": p.id,
        "title": m.title,
        "description": m.description,
        "state": m.state,
        "start_date": m.start_date,
        "due_date": m.due_date,
        "expired": m.due_date.is_some_and(|d| d < chrono::Utc::now().date_naive()),
        "created_at": j::ts(&m.created_at),
        "updated_at": j::ts(&m.updated_at),
        "web_url": format!("{}/{}/-/milestones/{}", j::base(&state.cfg), p.full_path, m.id),
    })
}

#[derive(Deserialize)]
pub struct MilestonesQuery {
    state: Option<String>,
    title: Option<String>,
    #[serde(default)]
    search: String,
}

pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<MilestonesQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let st = match q.state.as_deref() {
        Some("active") => "active",
        Some("closed") => "closed",
        _ => "all",
    };
    let all: Vec<Milestone> = forgust_db::milestones::list(&state.db, p.id, st, &forgust_db::like_escape(q.search.trim()))
        .await?
        .into_iter()
        .filter(|m| q.title.as_deref().is_none_or(|t| m.title == t))
        .collect();
    let total = all.len() as i64;
    let items: Vec<Value> =
        all.iter().skip(pg.offset() as usize).take(pg.per_page() as usize).map(|m| milestone_json(&state, &p, m)).collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

pub async fn show(State(state): State<AppState>, Viewer(v): Viewer, Path((id, mid)): Path<(String, i64)>) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let m = svc::find(&state, &v, &p, mid).await?;
    Ok(Json(milestone_json(&state, &p, &m)))
}

pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<MilestoneBody>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let m = svc::create(&state, &v, &p, &b).await?;
    Ok((StatusCode::CREATED, Json(milestone_json(&state, &p, &m))).into_response())
}

pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, mid)): Path<(String, i64)>,
    Json(b): Json<MilestoneBody>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let m = svc::update(&state, &v, &p, mid, &b).await?;
    Ok(Json(milestone_json(&state, &p, &m)))
}

pub async fn delete(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, mid)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    svc::delete(&state, &v, &p, mid).await?;
    Ok(StatusCode::NO_CONTENT)
}
