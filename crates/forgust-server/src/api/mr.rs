//! Merge request API (M3).

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use std::collections::HashMap;

use crate::services::stack::{StackBadge, StackView};
use forgust_db::issues::{self as idb, Label};
use forgust_db::merge_requests::{self as mrdb, ApprovalRule, MergeRequest, MrDiff, Note};
use forgust_db::people::{Reviewer, UserBrief};
use forgust_db::projects::Project;
use forgust_db::webhooks::{Delivery, Webhook};
use forgust_storage::DiffOptions;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;
use utoipa::ToSchema;

use super::{group_for, project_for};
use crate::auth::authz::{GroupAction, ProjectAction, can_project};
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::events::Audience;
use crate::services::mr::{self, MappedPosition, MergeMethod, MergeStatus, NewPosition};
use crate::state::{AppState, ResponseCache};

async fn load(
    state: &AppState,
    principal: &crate::auth::Principal,
    id: &str,
    iid: i64,
    action: ProjectAction,
) -> ApiResult<(Project, MergeRequest)> {
    let p = project_for(state, principal, id, action).await?;
    let m = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok((p, m))
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "opened")]
    state: String,
    #[serde(default)]
    search: String,
    #[serde(default = "one")]
    page: i64,
    #[serde(default = "per")]
    per_page: i64,
    author_id: Option<i64>,
    assignee_id: Option<i64>,
    reviewer_id: Option<i64>,
    /// A milestone title, `None` (none set) or `Any`.
    milestone: Option<String>,
}

fn opened() -> String {
    "opened".into()
}
fn one() -> i64 {
    1
}
fn per() -> i64 {
    20
}

#[derive(Serialize, ToSchema)]
pub struct MrListItem {
    #[serde(flatten)]
    pub merge_request: MergeRequest,
    pub labels: Vec<Label>,
    pub assignees: Vec<UserBrief>,
    pub reviewers: Vec<Reviewer>,
    pub stack: Option<StackBadge>,
}

/// Assignees and reviewers per MR id.
pub(crate) async fn people_by_mr(state: &AppState, ids: &[i64]) -> ApiResult<(HashMap<i64, Vec<UserBrief>>, HashMap<i64, Vec<Reviewer>>)> {
    let (a, r) = tokio::try_join!(forgust_db::people::mr_assignees(&state.db, ids), forgust_db::people::mr_reviewers(&state.db, ids))?;
    Ok((forgust_db::group_by_id(a), forgust_db::group_by_id(r)))
}

async fn labels_by_mr(state: &AppState, ids: &[i64]) -> ApiResult<HashMap<i64, Vec<Label>>> {
    Ok(forgust_db::group_by_id(idb::labels_of_mrs(&state.db, ids).await?))
}

#[derive(Serialize, ToSchema)]
pub struct MrCount {
    pub opened: i64,
    pub merged: i64,
    pub closed: i64,
}

/// How many merge requests are in each state (cheaper than listing them), with
/// the list's filters (`state`, `page` and `per_page` are ignored).
#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests_count", responses((status = 200, body = MrCount)), tag = "merge_requests")]
pub async fn count(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<MrCount>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let people = mrdb::PeopleFilter {
        author_id: q.author_id,
        assignee_id: q.assignee_id,
        reviewer_id: q.reviewer_id,
        milestone_id: crate::services::milestones::filter(&state, p.id, q.milestone.as_deref()).await?,
    };
    let (opened, merged, closed) = mrdb::count_by_state(&state.db, p.id, &q.search, people).await?;
    Ok(Json(MrCount { opened, merged, closed }))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests", responses((status = 200, body = [MrListItem])), tag = "merge_requests")]
pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<MrListItem>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    if !["opened", "closed", "merged", "all"].contains(&q.state.as_str()) {
        return Err(ApiError::bad("state must be opened, closed, merged or all"));
    }
    let (per, offset) = super::page_window(q.page, q.per_page);
    let people = mrdb::PeopleFilter {
        author_id: q.author_id,
        assignee_id: q.assignee_id,
        reviewer_id: q.reviewer_id,
        milestone_id: crate::services::milestones::filter(&state, p.id, q.milestone.as_deref()).await?,
    };
    let list = mrdb::list(&state.db, p.id, &q.state, &q.search, people, per, offset).await?;
    let ids = list.iter().map(|m| m.id).collect::<Vec<_>>();
    let mut labels = labels_by_mr(&state, &ids).await?;
    let (mut assignees, mut reviewers) = people_by_mr(&state, &ids).await?;
    let mut stacks = crate::services::stack::badges(&state, &list).await?;
    Ok(Json(
        list.into_iter()
            .map(|m| MrListItem {
                stack: stacks.remove(&m.id),
                labels: labels.remove(&m.id).unwrap_or_default(),
                assignees: assignees.remove(&m.id).unwrap_or_default(),
                reviewers: reviewers.remove(&m.id).unwrap_or_default(),
                merge_request: m,
            })
            .collect(),
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct LabelsBody {
    /// Label names; replaces the current labels.
    pub labels: Vec<String>,
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/merge_requests/{iid}/labels", request_body = LabelsBody, responses((status = 200, body = [Label])), tag = "merge_requests")]
pub async fn set_labels(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<LabelsBody>,
) -> ApiResult<Json<Vec<Label>>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    let labels = mr::set_labels(&state, &p0, &p, &m, &req.labels).await?;
    Ok(Json(labels))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateMr {
    pub source_branch: String,
    pub target_branch: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub squash: bool,
    pub assignee_ids: Option<Vec<i64>>,
    pub reviewer_ids: Option<Vec<i64>>,
    /// Stack on this open MR: the target becomes its source branch (ADR-0019).
    pub stack_on_iid: Option<i64>,
    /// Delete the source branch when the MR merges. Off by default, as in
    /// GitLab's API (the web form ticks it).
    pub remove_source_branch: Option<bool>,
    /// A milestone of the project.
    pub milestone_id: Option<i64>,
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/merge_requests", request_body = CreateMr, responses((status = 200, body = MergeRequest)), tag = "merge_requests")]
pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<CreateMr>,
) -> ApiResult<Json<MergeRequest>> {
    // authorize before reading anything about the project (no existence oracle)
    let p = project_for(&state, &p0, &id, ProjectAction::Push).await?;
    let parent_branch = match req.stack_on_iid {
        Some(iid) => {
            Some(mrdb::find(&state.db, p.id, iid).await?.ok_or_else(|| ApiError::bad(format!("no merge request !{iid}")))?.source_branch)
        }
        None => None,
    };
    let target = req
        .target_branch
        .clone()
        .or(parent_branch)
        .or(p.default_branch.clone())
        .ok_or_else(|| ApiError::bad("target_branch is required"))?;
    let m = mr::create(
        &state,
        &p0,
        &p,
        mr::CreateMr {
            source_branch: &req.source_branch,
            target_branch: &target,
            title: &req.title,
            description: &req.description,
            draft: req.draft,
            squash: req.squash,
            stack_on: req.stack_on_iid,
            remove_source_branch: req.remove_source_branch.unwrap_or(false),
        },
    )
    .await?;
    crate::services::people::set_mr(&state, &p0, &p, &m, req.assignee_ids.as_deref(), req.reviewer_ids.as_deref()).await?;
    if req.milestone_id.is_some() {
        crate::services::milestones::set_merge_request(&state, &p0, &p, &m, req.milestone_id).await?;
        return Ok(Json(mrdb::find(&state.db, p.id, m.iid).await?.ok_or(ApiError::NotFound)?));
    }
    Ok(Json(m))
}

#[derive(Serialize, ToSchema)]
pub struct MrDetail {
    #[serde(flatten)]
    pub merge_request: MergeRequest,
    /// Newest first.
    pub versions: Vec<MrDiff>,
    pub approvals: Vec<mrdb::Approval>,
    pub can_edit: bool,
    pub can_approve: bool,
    pub approved_by_me: bool,
    /// Sanitized HTML of the description (Markdown).
    pub description_html: String,
    pub labels: Vec<Label>,
    pub assignees: Vec<UserBrief>,
    pub reviewers: Vec<Reviewer>,
    /// The stack this MR belongs to, bottom first (ADR-0019).
    pub stack: Option<StackView>,
    /// Who set auto-merge, when it is on.
    pub auto_merge_user: Option<UserBrief>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}", responses((status = 200, body = MrDetail)), tag = "merge_requests")]
pub async fn show(State(state): State<AppState>, Viewer(v): Viewer, Path((id, iid)): Path<(String, i64)>) -> ApiResult<Json<MrDetail>> {
    let (p, m) = load(&state, &v, &id, iid, ProjectAction::ReadCode).await?;
    let versions = mrdb::diffs(&state.db, m.id).await?;
    let approvals = mrdb::approvals(&state.db, m.id, p.id).await?;
    let me = v.direct_user().map(|u| u.id);
    let can_edit = mr::may_edit(&state, &v, &p, &m).await.is_ok();
    let level = crate::auth::authz::project_role(&state, &v, &p).await?.level;
    let labels = labels_by_mr(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
    let (mut assignees, mut reviewers) = people_by_mr(&state, &[m.id]).await?;
    let stack = crate::services::stack::view(&state, &p, &m).await?;
    let auto_merge_user = match m.auto_merge_by {
        Some(id) => forgust_db::users::find(&state.db, id).await?.map(|u| UserBrief { id: u.id, username: u.username, name: u.name }),
        None => None,
    };
    Ok(Json(MrDetail {
        auto_merge_user,
        stack,
        labels,
        assignees: assignees.remove(&m.id).unwrap_or_default(),
        reviewers: reviewers.remove(&m.id).unwrap_or_default(),
        approved_by_me: me.is_some_and(|me| approvals.iter().any(|a| a.user_id == me)),
        can_approve: me.is_some() && level >= forgust_db::access::REPORTER && m.author_id != me && m.state == "opened",
        can_edit,
        description_html: crate::services::refs::render(&state, &p, &m.description, &m.target_branch, true).await,
        versions,
        approvals,
        merge_request: m,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateMr {
    pub title: Option<String>,
    pub description: Option<String>,
    pub draft: Option<bool>,
    pub target_branch: Option<String>,
    /// Replace the assignees (user ids).
    pub assignee_ids: Option<Vec<i64>>,
    /// Replace the review requests (user ids); kept reviewers keep their state.
    pub reviewer_ids: Option<Vec<i64>>,
    pub remove_source_branch: Option<bool>,
    /// Put it on a milestone (`0`: take it off).
    pub milestone_id: Option<i64>,
}

pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<UpdateMr>,
) -> ApiResult<Json<MergeRequest>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    mr::may_edit(&state, &p0, &p, &m).await?;
    crate::services::people::set_mr(&state, &p0, &p, &m, req.assignee_ids.as_deref(), req.reviewer_ids.as_deref()).await?;
    if let Some(r) = req.remove_source_branch {
        mrdb::set_remove_source_branch(&state.db, m.id, r).await?;
    }
    if req.milestone_id.is_some() {
        crate::services::milestones::set_merge_request(&state, &p0, &p, &m, req.milestone_id).await?;
    }
    let change = mr::DetailsChange {
        title: req.title.as_deref(),
        description: req.description.as_deref(),
        draft: req.draft,
        target_branch: req.target_branch.as_deref(),
    };
    mr::update_details(&state, &p0, &p, &m, change).await?;
    Ok(Json(mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?))
}

pub async fn close(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<MergeRequest>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    Ok(Json(mr::set_open(&state, &p0, &p, &m, false).await?))
}

pub async fn reopen(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<MergeRequest>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    Ok(Json(mr::set_open(&state, &p0, &p, &m, true).await?))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/merge_status", responses((status = 200, body = MergeStatus)), tag = "merge_requests")]
pub async fn merge_status(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<MergeStatus>> {
    let (p, m) = load(&state, &v, &id, iid, ProjectAction::ReadCode).await?;
    Ok(Json(mr::merge_status(&state, &v, &p, &m).await?))
}

/// Put the merge request on its target's merge train.
#[utoipa::path(post, path = "/api/v1/projects/{id}/merge_requests/{iid}/merge_train", responses((status = 200, body = forgust_db::merge_trains::Car)), tag = "merge_requests")]
pub async fn add_to_train(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    body: Option<Json<crate::services::merge_train::TrainOptions>>,
) -> ApiResult<Json<forgust_db::merge_trains::Car>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    let opts = body.map(|Json(o)| o).unwrap_or_default();
    Ok(Json(crate::services::merge_train::add(&state, &p0, &p, &m, opts).await?))
}

pub async fn remove_from_train(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    crate::services::merge_train::remove(&state, &p0, &p, &m).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct TrainQuery {
    target_branch: Option<String>,
}

/// The project's merge trains, in order (the first car of each merges next).
#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_trains", responses((status = 200, body = [forgust_db::merge_trains::Car])), tag = "merge_requests")]
pub async fn merge_trains(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<TrainQuery>,
) -> ApiResult<Json<Vec<forgust_db::merge_trains::Car>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    Ok(Json(forgust_db::merge_trains::cars(&state.db, p.id, q.target_branch.as_deref()).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct MergeRequestBody {
    pub method: Option<MergeMethod>,
    /// The source head the user reviewed (from merge_status.head_sha).
    pub sha: Option<String>,
    /// Default: the MR's own choice (`remove_source_branch`).
    pub remove_source_branch: Option<bool>,
    pub message: Option<String>,
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/merge_requests/{iid}/merge", request_body = MergeRequestBody, responses((status = 200, body = MergeRequest)), tag = "merge_requests")]
pub async fn merge(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<MergeRequestBody>,
) -> ApiResult<Json<MergeRequest>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    let method = req.method.unwrap_or(if m.squash { MergeMethod::Squash } else { MergeMethod::Merge });
    let opts = mr::MergeOptions {
        method,
        expected_head: req.sha.as_deref(),
        remove_source_branch: req.remove_source_branch.unwrap_or(m.remove_source_branch),
        message: req.message.as_deref(),
        train: None,
    };
    Ok(Json(mr::merge(&state, &p0, &p, &m, opts).await?))
}

pub async fn rebase(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    let sha = mr::rebase_source(&state, &p0, &p, &m).await?;
    Ok(Json(json!({ "head_sha": sha })))
}

pub async fn approve(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    mr::approve(&state, &p0, &p, &m, true).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn unapprove(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    mr::approve(&state, &p0, &p, &m, false).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct VersionQuery {
    version: Option<i32>,
    /// Compare two versions: the diff from this version's head to `version`'s head.
    from_version: Option<i32>,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

fn pick(versions: &[MrDiff], v: Option<i32>) -> ApiResult<&MrDiff> {
    match v {
        Some(n) => versions.iter().find(|d| d.version == n).ok_or(ApiError::NotFound),
        None => versions.first().ok_or(ApiError::NotFound),
    }
}

pub async fn commits(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
    Query(q): Query<VersionQuery>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid, ProjectAction::ReadCode).await?;
    let versions = mrdb::diffs(&state.db, m.id).await?;
    let d = pick(&versions, q.version)?;
    let key = ResponseCache::key(p.id, "mrcommits", &[&d.base_sha, &d.head_sha]);
    cached(&state, key, || async {
        let mut log = state.storage.log_between(p.id, &d.base_sha, &d.head_sha, 500).await?;
        crate::services::people::link_commits(&state, &mut log).await;
        Ok(log)
    })
    .await
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/diff", responses((status = 200, body = forgust_storage::Diff)), tag = "merge_requests")]
pub async fn diff(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
    Query(q): Query<VersionQuery>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid, ProjectAction::ReadCode).await?;
    let versions = mrdb::diffs(&state.db, m.id).await?;
    let to = pick(&versions, q.version)?;
    let from_sha = match q.from_version {
        Some(fv) => pick(&versions, Some(fv))?.head_sha.clone(),
        None => to.base_sha.clone(),
    };
    let max = state.cfg.limits.max_diff_files_per_page;
    let opts = DiffOptions {
        offset: q.offset,
        limit: q.limit.unwrap_or(max).clamp(1, max),
        max_lines_per_file: state.cfg.limits.max_diff_lines_per_file,
        paths: None,
    };
    let key = ResponseCache::key(p.id, "mrdiff", &[&from_sha, &to.head_sha, &opts.offset.to_string(), &opts.limit.to_string()]);
    cached(&state, key, || async {
        let mut d = state.storage.diff(p.id, Some(&from_sha), &to.head_sha, opts).await?;
        super::repo::highlight_diff(&state, p.id, &mut d).await;
        Ok(d)
    })
    .await
}

use super::repo::cached;

#[derive(Serialize, ToSchema)]
pub struct NoteDto {
    #[serde(flatten)]
    pub note: Note,
    /// Version the comment was written on.
    pub version: Option<i32>,
    /// Where the thread sits on the requested version; null when outdated.
    pub position: Option<MappedPosition>,
    pub outdated: bool,
    /// Sanitized HTML of the body (Markdown).
    pub body_html: String,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/notes", responses((status = 200, body = [NoteDto])), tag = "merge_requests")]
pub async fn notes(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
    Query(q): Query<VersionQuery>,
) -> ApiResult<Json<Vec<NoteDto>>> {
    let (p, m) = load(&state, &v, &id, iid, ProjectAction::ReadCode).await?;
    let versions = mrdb::diffs(&state.db, m.id).await?;
    let target = pick(&versions, q.version).ok();
    let mut out = Vec::new();
    let notes = mrdb::notes(&state.db, m.id).await?;
    let bodies: Vec<&str> = notes.iter().map(|n| n.body.as_str()).collect();
    let mut html = crate::services::refs::render_many(&state, &p, &bodies, &m.target_branch, true).await.into_iter();
    for n in notes {
        let version = n.diff_id.and_then(|d| versions.iter().find(|v| v.id == d)).map(|v| v.version);
        let (position, outdated) = match (n.head_sha.is_some() && n.parent_id.is_none(), target) {
            (true, Some(t)) => {
                let pos = mr::map_position(&state, p.id, &n, t).await?;
                let outdated = pos.is_none();
                (pos, outdated)
            }
            _ => (None, false),
        };
        let body_html = html.next().unwrap_or_default();
        out.push(NoteDto { note: n, version, position, outdated, body_html });
    }
    Ok(Json(out))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateNote {
    pub body: String,
    pub parent_id: Option<i64>,
    pub position: Option<NewPosition>,
}

pub async fn create_note(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<CreateNote>,
) -> ApiResult<Json<Note>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    Ok(Json(mr::add_note(&state, &p0, &p, &m, &req.body, req.parent_id, req.position).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct EditNote {
    pub body: String,
}

pub async fn edit_note(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid, note_id)): Path<(String, i64, i64)>,
    Json(req): Json<EditNote>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    mr::edit_note(&state, &p0, &p, &m, note_id, Some(&req.body)).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_note(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid, note_id)): Path<(String, i64, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    mr::edit_note(&state, &p0, &p, &m, note_id, None).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, ToSchema)]
pub struct ResolveBody {
    pub resolved: bool,
}

pub async fn resolve_note(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid, note_id)): Path<(String, i64, i64)>,
    Json(req): Json<ResolveBody>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    mr::resolve_thread(&state, &p0, &p, &m, note_id, req.resolved).await?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ approval rules

pub async fn rules(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<Vec<ApprovalRule>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    Ok(Json(mrdb::rules(&state.db, p.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct RuleBody {
    pub name: String,
    pub approvals_required: i16,
    /// reporter, developer, maintainer or owner
    #[serde(default = "developer")]
    pub min_access_level: String,
}

fn developer() -> String {
    "developer".into()
}

pub async fn upsert_rule(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<RuleBody>,
) -> ApiResult<Json<Vec<ApprovalRule>>> {
    let p = project_for(&state, &p0, &id, ProjectAction::Admin).await?;
    let level = forgust_db::access::parse(&req.min_access_level)
        .filter(|l| *l >= 20)
        .ok_or_else(|| ApiError::bad("min_access_level must be reporter or above"))?;
    if !(1..=20).contains(&req.approvals_required) || req.name.trim().is_empty() {
        return Err(ApiError::bad("a name and 1-20 required approvals are needed"));
    }
    mrdb::upsert_rule(&state.db, p.id, req.name.trim(), req.approvals_required, level).await?;
    crate::services::audit(
        &state,
        &p0,
        "approval_rule_set",
        Some(("project", p.id)),
        json!({ "name": req.name, "required": req.approvals_required }),
        None,
    )
    .await;
    Ok(Json(mrdb::rules(&state.db, p.id).await?))
}

pub async fn delete_rule(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, rule_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &p0, &id, ProjectAction::Admin).await?;
    if !mrdb::delete_rule(&state.db, p.id, rule_id).await? {
        return Err(ApiError::NotFound);
    }
    crate::services::audit(&state, &p0, "approval_rule_removed", Some(("project", p.id)), json!({ "id": rule_id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ webhooks

#[derive(Deserialize, ToSchema)]
pub struct HookBody {
    pub url: String,
    pub secret: Option<String>,
    /// push, merge_request, note, release, pipeline, build
    pub events: Vec<String>,
    /// `generic` (default: the JSON payload) or a chat app: `slack`,
    /// `mattermost`, `discord`, `matrix`, `teams`.
    pub format: Option<String>,
}

async fn hook_owner(state: &AppState, principal: &crate::auth::Principal, kind: &str, id: &str) -> ApiResult<(Option<i64>, Option<i64>)> {
    Ok(match kind {
        "projects" => (Some(project_for(state, principal, id, ProjectAction::Admin).await?.id), None),
        "groups" => (None, Some(group_for(state, principal, id, GroupAction::Admin).await?.id)),
        _ => return Err(ApiError::NotFound),
    })
}

pub async fn list_hooks(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((kind, id)): Path<(String, String)>,
) -> ApiResult<Json<Vec<Webhook>>> {
    let (pid, nid) = hook_owner(&state, &p0, &kind, &id).await?;
    Ok(Json(forgust_db::webhooks::list(&state.db, pid, nid).await?))
}

pub async fn create_hook(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((kind, id)): Path<(String, String)>,
    Json(req): Json<HookBody>,
) -> ApiResult<Json<Vec<Webhook>>> {
    let (pid, nid) = hook_owner(&state, &p0, &kind, &id).await?;
    crate::services::webhooks::validate_url(&req.url)?;
    if req.events.is_empty() || req.events.iter().any(|e| !crate::services::webhooks::EVENTS.contains(&e.as_str())) {
        return Err(ApiError::bad(format!("events must be among {}", crate::services::webhooks::EVENTS.join(", "))));
    }
    let format = req.format.as_deref().filter(|f| !f.is_empty()).unwrap_or("generic");
    if !crate::services::chat::FORMATS.contains(&format) {
        return Err(ApiError::bad(format!("format must be one of {}", crate::services::chat::FORMATS.join(", "))));
    }
    let sealed = match req.secret.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(state.secrets.encrypt(s.as_bytes())?),
        None => None,
    };
    forgust_db::webhooks::create(&state.db, pid, nid, &req.url, sealed.as_deref(), &req.events, format).await?;
    let target = pid.map(|i| ("project", i)).or(nid.map(|i| ("namespace", i)));
    crate::services::audit(&state, &p0, "webhook_added", target, json!({ "url": req.url, "events": req.events, "format": format }), None)
        .await;
    Ok(Json(forgust_db::webhooks::list(&state.db, pid, nid).await?))
}

pub async fn delete_hook(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((kind, id, hook_id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let (pid, nid) = hook_owner(&state, &p0, &kind, &id).await?;
    if !forgust_db::webhooks::delete(&state.db, hook_id, pid, nid).await? {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

pub async fn hook_deliveries(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((kind, id, hook_id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Vec<Delivery>>> {
    let (pid, nid) = hook_owner(&state, &p0, &kind, &id).await?;
    let hook = forgust_db::webhooks::find(&state.db, hook_id).await?.ok_or(ApiError::NotFound)?;
    if hook.project_id != pid || hook.namespace_id != nid {
        return Err(ApiError::NotFound);
    }
    Ok(Json(forgust_db::webhooks::deliveries(&state.db, hook_id, 50).await?))
}

// ------------------------------------------------------------------ live updates

#[derive(Deserialize)]
pub struct EventsQuery {
    /// Projects this tab watches (comma-separated IDs; none: only the
    /// caller's own events, their inbox).
    #[serde(default)]
    projects: String,
}

/// How long a live stream lives before the browser reconnects (and is
/// authenticated again: a revoked session ends there).
const EVENTS_LIFETIME: Duration = Duration::from_secs(600);

/// One SSE stream per browser tab (architecture: Frontend). The tab lists the
/// projects it shows; events of other projects, or of projects the viewer cannot
/// read, are never sent. Topics change by reconnecting with a new list.
pub async fn events(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let mut allowed = Vec::new();
    for id in q.projects.split(',').filter(|s| !s.is_empty()).take(20) {
        let pid: i64 = id.parse().map_err(|_| ApiError::bad("projects must be numeric IDs"))?;
        if let Some(p) = forgust_db::projects::find(&state.db, pid).await?
            && can_project(&state, &v, ProjectAction::ReadCode, &p).await?
        {
            allowed.push(pid);
        }
    }
    // access is looked at again (once a minute by default) when an event comes: a member
    // removed from a project stops hearing about it
    let me = v.direct_user().map(|u| u.id);
    let checked = std::sync::Arc::new(tokio::sync::Mutex::new((allowed, std::time::Instant::now())));
    let stream = futures::StreamExt::filter_map(BroadcastStream::new(state.events.subscribe()), move |ev| {
        let (state, v, checked) = (state.clone(), v.clone(), checked.clone());
        async move {
            let ev = ev.ok()?;
            // a person's own events (their inbox changed): whatever the projects watched
            let project_id = match ev.audience() {
                Audience::User(id) => return (Some(id) == me).then(|| Ok(SseEvent::default().json_data(&ev).unwrap_or_default())),
                Audience::Project(id) => id,
            };
            let mut guard = checked.lock().await;
            if !guard.0.contains(&project_id) {
                return None;
            }
            if guard.1.elapsed() >= Duration::from_secs(state.cfg.limits.events_recheck_secs) {
                let mut still = Vec::new();
                for pid in guard.0.iter().copied() {
                    if let Ok(Some(p)) = forgust_db::projects::find(&state.db, pid).await
                        && can_project(&state, &v, ProjectAction::ReadCode, &p).await.unwrap_or(false)
                    {
                        still.push(pid);
                    }
                }
                *guard = (still, std::time::Instant::now());
                if !guard.0.contains(&project_id) {
                    return None;
                }
            }
            Some(Ok(SseEvent::default().json_data(&ev).unwrap_or_default()))
        }
    });
    let stream = futures::StreamExt::take_until(stream, tokio::time::sleep(EVENTS_LIFETIME));
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(20))))
}

/// Ask a reviewer to look again.
pub async fn rerequest_review(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid, user_id)): Path<(String, i64, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    crate::services::people::rerequest(&state, &p0, &p, &m, user_id).await?;
    Ok(Json(json!({})))
}

/// Block merging until the caller approves or is asked to review again.
pub async fn request_changes(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    body: Option<Json<RequestChangesBody>>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    let comment = body.and_then(|Json(b)| b.body);
    crate::services::people::request_changes(&state, &p0, &p, &m, comment.as_deref()).await?;
    Ok(Json(json!({})))
}

#[derive(Deserialize, ToSchema)]
pub struct RequestChangesBody {
    /// What to change: posted as a comment that carries the verdict.
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct StackOnBody {
    /// The open MR to stack on; this MR must target its source branch.
    pub on_iid: i64,
}

/// Put this MR on top of another (ADR-0019).
#[utoipa::path(put, path = "/api/v1/projects/{id}/merge_requests/{iid}/stack", request_body = StackOnBody, responses((status = 200, body = MrDetail)), tag = "merge_requests")]
pub async fn stack_on(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<StackOnBody>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    crate::services::stack::stack_on(&state, &p0, &p, &m, req.on_iid).await?;
    Ok(Json(json!({})))
}

/// Take this MR, and the layers above it, out of its stack.
pub async fn leave_stack(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    crate::services::stack::leave(&state, &p0, &p, &m).await?;
    Ok(Json(json!({})))
}

/// Rebase every open layer of the stack onto the branch below it.
pub async fn rebase_stack(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    let moved = crate::services::stack::rebase(&state, &p0, &p, &m).await?;
    Ok(Json(json!({ "rebased": moved })))
}

#[derive(Deserialize, ToSchema)]
pub struct AutoMergeBody {
    pub method: Option<MergeMethod>,
    /// The source head the user reviewed (from merge_status.head_sha).
    pub sha: Option<String>,
    /// Default: the MR's own choice.
    pub remove_source_branch: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct AutoMergeResult {
    pub outcome: crate::services::auto_merge::Outcome,
    pub merge_request: MergeRequest,
}

/// Merge when every check passes (at once when they already do).
#[utoipa::path(post, path = "/api/v1/projects/{id}/merge_requests/{iid}/auto_merge", request_body = AutoMergeBody, responses((status = 200, body = AutoMergeResult)), tag = "merge_requests")]
pub async fn auto_merge(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<AutoMergeBody>,
) -> ApiResult<Json<AutoMergeResult>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    let method = req.method.unwrap_or(if m.squash { MergeMethod::Squash } else { MergeMethod::Merge });
    let outcome = crate::services::auto_merge::enable(
        &state,
        &p0,
        &p,
        &m,
        method,
        req.sha.as_deref(),
        req.remove_source_branch.unwrap_or(m.remove_source_branch),
    )
    .await?;
    let merge_request = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(AutoMergeResult { outcome, merge_request }))
}

pub async fn cancel_auto_merge(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<MergeRequest>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    crate::services::auto_merge::disable(&state, &p0, &p, &m).await?;
    Ok(Json(mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?))
}

/// Commit a comment's suggested change to the source branch.
pub async fn apply_suggestion(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid, note_id)): Path<(String, i64, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &p0, &id, iid, ProjectAction::ReadCode).await?;
    let sha = crate::services::suggestions::apply(&state, &p0, &p, &m, note_id).await?;
    Ok(Json(json!({ "head_sha": sha })))
}
