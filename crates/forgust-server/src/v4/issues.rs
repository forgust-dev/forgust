//! Issues (Renovate's dependency dashboard, `glab issue`).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use forgust_db::issues::{self as idb, Issue};
use forgust_db::projects::Project;
use serde::Deserialize;
use serde_json::{Value, json};

use super::json as j;
use super::{Ids, Labels, PageQuery, paged, relabel};
use crate::api::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::issues::{self as svc, CreateIssue, UpdateIssue};
use crate::state::AppState;

async fn labels_of(state: &AppState, ids: &[i64]) -> ApiResult<HashMap<i64, Vec<String>>> {
    Ok(forgust_db::group_by_id(idb::labels_of_issues(&state.db, ids).await?.into_iter().map(|(id, l)| (id, l.name)).collect()))
}

async fn render(state: &AppState, p: &Project, i: &Issue) -> ApiResult<Value> {
    let labels = labels_of(state, &[i.id]).await?.remove(&i.id).unwrap_or_default();
    let assignees: Vec<_> = forgust_db::people::issue_assignees(&state.db, &[i.id]).await?.into_iter().map(|(_, u)| u).collect();
    Ok(j::issue(&state.cfg, p, i, labels, &assignees))
}

#[derive(Deserialize)]
pub struct ListQuery {
    state: Option<String>,
    labels: Option<Labels>,
    #[serde(default)]
    search: String,
    scope: Option<String>,
    author_id: Option<i64>,
    assignee_id: Option<i64>,
    assignee_username: Option<String>,
    milestone: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let st = match q.state.as_deref() {
        None | Some("all") => "all",
        Some("opened") => "opened",
        Some("closed") => "closed",
        Some(_) => return Err(ApiError::bad("state must be opened, closed or all")),
    };
    let author = match q.scope.as_deref() {
        Some("created_by_me") => Some(v.direct_user().ok_or(ApiError::Unauthorized)?.id),
        _ => q.author_id,
    };
    let assignee = match (q.scope.as_deref(), &q.assignee_username) {
        (Some("assigned_to_me"), _) => Some(v.direct_user().ok_or(ApiError::Unauthorized)?.id),
        (_, Some(name)) => Some(super::user_filter(&state, name).await?),
        _ => q.assignee_id,
    };
    let wanted = q.labels.as_ref().map(Labels::names).unwrap_or_default();
    // the first label narrows the query; further labels (and then the page) are applied here
    let filtered = wanted.len() > 1;
    let (limit, offset) = if filtered { (5000, 0) } else { (pg.probe(), pg.offset()) };
    let list = idb::list(
        &state.db,
        p.id,
        idb::ListFilter {
            state: st,
            search: q.search.trim(),
            label: wanted.first().map(String::as_str).unwrap_or(""),
            assignee_id: assignee,
            author_id: author,
            milestone_id: crate::services::milestones::filter(&state, p.id, q.milestone.as_deref()).await?,
            limit,
            offset,
        },
    )
    .await?;
    let mut labels = labels_of(&state, &list.iter().map(|i| i.id).collect::<Vec<_>>()).await?;
    let mut picked: Vec<(Issue, Vec<String>)> = list
        .into_iter()
        .map(|i| {
            let l = labels.remove(&i.id).unwrap_or_default();
            (i, l)
        })
        .filter(|(_, l)| wanted.iter().all(|w| l.contains(w)))
        .collect();
    if filtered {
        picked = picked.into_iter().skip(pg.offset() as usize).take(pg.probe() as usize).collect();
    }
    let mut people = forgust_db::group_by_id(
        forgust_db::people::issue_assignees(&state.db, &picked.iter().map(|(i, _)| i.id).collect::<Vec<_>>()).await?,
    );
    let items: Vec<Value> =
        picked.into_iter().map(|(i, l)| j::issue(&state.cfg, &p, &i, l, &people.remove(&i.id).unwrap_or_default())).collect();
    Ok(paged(&state, &uri, &pg, items, None))
}

pub async fn show(State(state): State<AppState>, Viewer(v): Viewer, Path((id, iid)): Path<(String, i64)>) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let i = idb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(render(&state, &p, &i).await?))
}

/// Forgust has no confidential issues: asking for one is refused, not quietly
/// published (a security report filed as confidential must not go public).
fn no_confidential(asked: Option<bool>) -> ApiResult<()> {
    if asked == Some(true) {
        return Err(ApiError::bad("confidential issues are not supported on Forgust: everyone who can see the project sees its issues"));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct CreateBody {
    title: String,
    confidential: Option<bool>,
    #[serde(default)]
    description: Option<String>,
    labels: Option<Labels>,
    assignee_ids: Option<Ids>,
    assignee_id: Option<Ids>,
    milestone_id: Option<i64>,
    due_date: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<CreateBody>,
) -> ApiResult<Response> {
    no_confidential(req.confidential)?;
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let names = req.labels.as_ref().map(Labels::names).unwrap_or_default();
    svc::ensure_labels(&state, &v, &p, &names).await?;
    crate::api::issues::check_plan(&state, &v, &p, req.milestone_id, req.due_date.as_deref()).await?;
    let i = svc::create(
        &state,
        &v,
        &p,
        CreateIssue { title: &req.title, description: req.description.as_deref().unwrap_or(""), labels: &names },
    )
    .await?;
    if let Some(ids) = req.assignee_ids.as_ref().or(req.assignee_id.as_ref()).map(Ids::ids) {
        crate::services::people::set_issue(&state, &v, &p, &i, &ids).await?;
    }
    let i = crate::api::issues::plan(&state, &v, &p, i, req.milestone_id, req.due_date.as_deref()).await?;
    Ok((StatusCode::CREATED, Json(render(&state, &p, &i).await?)).into_response())
}

#[derive(Deserialize)]
pub struct UpdateBody {
    title: Option<String>,
    confidential: Option<bool>,
    description: Option<String>,
    state_event: Option<String>,
    labels: Option<Labels>,
    add_labels: Option<Labels>,
    remove_labels: Option<Labels>,
    assignee_ids: Option<Ids>,
    assignee_id: Option<Ids>,
    #[serde(default, deserialize_with = "super::nullable")]
    milestone_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "super::nullable")]
    due_date: Option<Option<String>>,
}

pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    no_confidential(req.confidential)?;
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let i = idb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let open = match req.state_event.as_deref() {
        None => None,
        Some("close") => Some(false),
        Some("reopen") => Some(true),
        Some(_) => return Err(ApiError::bad("state_event must be close or reopen")),
    };
    let current = labels_of(&state, &[i.id]).await?.remove(&i.id).unwrap_or_default();
    let names = relabel(&current, req.labels.as_ref(), req.add_labels.as_ref(), req.remove_labels.as_ref());
    if let Some(n) = &names {
        svc::ensure_labels(&state, &v, &p, n).await?;
    }
    let i = svc::update(
        &state,
        &v,
        &p,
        &i,
        UpdateIssue { title: req.title.as_deref(), description: req.description.as_deref(), open, labels: names.as_deref() },
    )
    .await?;
    if let Some(ids) = req.assignee_ids.as_ref().or(req.assignee_id.as_ref()).map(Ids::ids) {
        crate::services::people::set_issue(&state, &v, &p, &i, &ids).await?;
    }
    // null clears: milestone 0, due date ""
    let milestone = req.milestone_id.map(|m| m.unwrap_or(0));
    let due = req.due_date.as_ref().map(|d| d.as_deref().unwrap_or(""));
    let i = crate::api::issues::plan(&state, &v, &p, i, milestone, due).await?;
    Ok(Json(render(&state, &p, &i).await?))
}

pub async fn notes(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path((id, iid)): Path<(String, i64)>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let i = idb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let all = idb::notes(&state.db, i.id).await?;
    let total = all.len() as i64;
    let items: Vec<Value> =
        all.iter().skip(pg.offset() as usize).take(pg.per_page() as usize).map(|n| j::issue_note(&state.cfg, n, i.iid)).collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

#[derive(Deserialize)]
pub struct NoteBody {
    body: String,
}

pub async fn create_note(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<NoteBody>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let i = idb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let note_id = svc::add_note(&state, &v, &p, &i, &req.body).await?;
    let n = idb::note(&state.db, i.id, note_id).await?.ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(j::issue_note(&state.cfg, &n, i.iid))).into_response())
}

/// `GET /projects/:id/labels`, with how many open issues and MRs use each.
pub async fn labels(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let all = forgust_db::issues::labels(&state.db, p.id).await?;
    let total = all.len() as i64;
    let items: Vec<Value> = all
        .iter()
        .skip(pg.offset() as usize)
        .take(pg.per_page() as usize)
        .map(|l| {
            json!({
                "id": l.id, "name": l.name, "color": l.color, "text_color": "#FFFFFF", "description": l.description,
                "description_html": l.description, "open_issues_count": l.open_issues, "closed_issues_count": 0,
                "open_merge_requests_count": l.open_merge_requests, "subscribed": false, "priority": null, "is_project_label": true,
            })
        })
        .collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}
