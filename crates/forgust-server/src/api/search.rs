//! Searching the instance (M7): projects, issues and merge requests by title,
//! only what the viewer may see (`forgust_db::search`).

use axum::Json;
use axum::extract::{Query, State};
use forgust_db::search::{self as sdb, Counts, ItemHit, ProjectHit};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::Viewer;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    /// all (a few of each, default), projects, issues or merge_requests
    scope: Option<String>,
    /// For issues and merge requests: opened, closed, merged or all (default)
    state: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct SearchResults {
    pub counts: Counts,
    pub projects: Vec<ProjectHit>,
    pub issues: Vec<ItemHit>,
    pub merge_requests: Vec<ItemHit>,
}

#[utoipa::path(get, path = "/api/v1/search", responses((status = 200, body = SearchResults)), tag = "projects")]
pub async fn search(State(state): State<AppState>, Viewer(v): Viewer, Query(q): Query<SearchQuery>) -> ApiResult<Json<SearchResults>> {
    let term = q.q.trim();
    if term.is_empty() || term.chars().count() > 200 {
        return Err(ApiError::bad("search for 1-200 characters"));
    }
    let pattern = forgust_db::like_escape(term);
    let viewer = sdb::Viewer { user_id: v.direct_user().map(|u| u.id), admin: v.is_admin(), internal: v.sees_internal() };
    let scope = q.scope.as_deref().unwrap_or("all");
    let st = q.state.as_deref().unwrap_or("all");
    if !["opened", "closed", "merged", "all"].contains(&st) {
        return Err(ApiError::bad("state must be opened, closed, merged or all"));
    }
    let (per, offset) = super::page_window(q.page.unwrap_or(1), q.per_page.unwrap_or(20));
    let (limit, offset) = if scope == "all" { (5, 0) } else { (per, offset) };
    let issue_state = if st == "merged" { "none" } else { st };
    let mut out =
        SearchResults { counts: sdb::counts(&state.db, viewer, &pattern).await?, projects: vec![], issues: vec![], merge_requests: vec![] };
    match scope {
        "all" | "projects" | "issues" | "merge_requests" => {}
        _ => return Err(ApiError::bad("scope must be all, projects, issues or merge_requests")),
    }
    if matches!(scope, "all" | "projects") {
        out.projects = sdb::projects(&state.db, viewer, &pattern, limit, offset).await?;
    }
    if matches!(scope, "all" | "issues") {
        out.issues = sdb::issues(&state.db, viewer, &pattern, issue_state, limit, offset).await?;
    }
    if matches!(scope, "all" | "merge_requests") {
        out.merge_requests = sdb::merge_requests(&state.db, viewer, &pattern, st, limit, offset).await?;
    }
    Ok(Json(out))
}
