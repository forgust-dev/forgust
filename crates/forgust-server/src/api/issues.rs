//! Issues and labels (M5).

use axum::Json;
use axum::extract::{Path, Query, State};
use forgust_db::issues::{self as idb, Issue, IssueNote, Label, LabelWithCounts};
use forgust_db::projects::Project;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::{ProjectAction, can_project, require_project};
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::issues::{self, CreateIssue, UpdateIssue};
use crate::state::AppState;

async fn load(state: &AppState, principal: &crate::auth::Principal, id: &str, iid: i64) -> ApiResult<(Project, Issue)> {
    let p = project_for(state, principal, id, ProjectAction::Read).await?;
    let i = idb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok((p, i))
}

/// Markdown of an issue for `viewer` (who may be a Guest: no merge request titles).
async fn render_for(state: &AppState, viewer: &crate::auth::Principal, p: &Project, texts: &[&str]) -> Vec<String> {
    let code = crate::services::releases::sees_code(state, viewer, p).await;
    crate::services::refs::render_many(state, p, texts, p.default_branch.as_deref().unwrap_or("HEAD"), code).await
}

#[derive(Serialize, ToSchema)]
pub struct IssueDto {
    #[serde(flatten)]
    pub issue: Issue,
    pub labels: Vec<Label>,
    pub assignees: Vec<forgust_db::people::UserBrief>,
}

async fn with_labels(state: &AppState, list: Vec<Issue>) -> ApiResult<Vec<IssueDto>> {
    let ids: Vec<i64> = list.iter().map(|i| i.id).collect();
    let (labels, people) = tokio::try_join!(idb::labels_of_issues(&state.db, &ids), forgust_db::people::issue_assignees(&state.db, &ids))?;
    let (mut by_issue, mut assignees) = (forgust_db::group_by_id(labels), forgust_db::group_by_id(people));
    Ok(list
        .into_iter()
        .map(|i| IssueDto {
            labels: by_issue.remove(&i.id).unwrap_or_default(),
            assignees: assignees.remove(&i.id).unwrap_or_default(),
            issue: i,
        })
        .collect())
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "opened")]
    state: String,
    #[serde(default)]
    search: String,
    #[serde(default)]
    label: String,
    #[serde(default = "one")]
    page: i64,
    #[serde(default = "per")]
    per_page: i64,
    assignee_id: Option<i64>,
    author_id: Option<i64>,
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
pub struct IssueList {
    pub issues: Vec<IssueDto>,
    pub opened: i64,
    pub closed: i64,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/issues", responses((status = 200, body = IssueList)), tag = "issues")]
pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<IssueList>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    if !["opened", "closed", "all"].contains(&q.state.as_str()) {
        return Err(ApiError::bad("state must be opened, closed or all"));
    }
    let (per, offset) = super::page_window(q.page, q.per_page);
    let filter = idb::ListFilter {
        state: &q.state,
        search: q.search.trim(),
        label: &q.label,
        assignee_id: q.assignee_id,
        author_id: q.author_id,
        milestone_id: crate::services::milestones::filter(&state, p.id, q.milestone.as_deref()).await?,
        limit: per,
        offset,
    };
    let issues = with_labels(&state, idb::list(&state.db, p.id, filter).await?).await?;
    let (opened, closed) = idb::counts(&state.db, p.id).await?;
    Ok(Json(IssueList { issues, opened, closed }))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateIssueBody {
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Label names; they must exist in the project.
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignee_ids: Vec<i64>,
    /// A milestone of the project (Developers).
    pub milestone_id: Option<i64>,
    /// `YYYY-MM-DD`.
    pub due_date: Option<String>,
}

/// Milestone (`0` removes it) and due date (`""` clears it), when given; the
/// issue as it is after.
/// What `plan` will refuse, checked before anything is created.
pub(crate) async fn check_plan(
    state: &AppState,
    p0: &crate::auth::Principal,
    p: &Project,
    milestone_id: Option<i64>,
    due_date: Option<&str>,
) -> ApiResult<()> {
    if milestone_id.is_some_and(|m| m != 0) {
        crate::services::milestones::check_target(state, p0, p, milestone_id).await?;
    }
    crate::services::milestones::parse_date(due_date, None)?;
    Ok(())
}

pub(crate) async fn plan(
    state: &AppState,
    p0: &crate::auth::Principal,
    p: &Project,
    i: Issue,
    milestone_id: Option<i64>,
    due_date: Option<&str>,
) -> ApiResult<Issue> {
    if milestone_id.is_none() && due_date.is_none() {
        return Ok(i);
    }
    if let Some(m) = milestone_id {
        crate::services::milestones::set_issue(state, p0, p, &i, Some(m)).await?;
    }
    if due_date.is_some() {
        let due = crate::services::milestones::parse_date(due_date, i.due_date)?;
        crate::services::milestones::set_issue_due_date(state, p0, p, &i, due).await?;
    }
    idb::find(&state.db, p.id, i.iid).await?.ok_or(ApiError::NotFound)
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/issues", request_body = CreateIssueBody, responses((status = 200, body = IssueDto)), tag = "issues")]
pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<CreateIssueBody>,
) -> ApiResult<Json<IssueDto>> {
    let p = project_for(&state, &p0, &id, ProjectAction::Read).await?;
    check_plan(&state, &p0, &p, req.milestone_id, req.due_date.as_deref()).await?;
    let i = issues::create(&state, &p0, &p, CreateIssue { title: &req.title, description: &req.description, labels: &req.labels }).await?;
    if !req.assignee_ids.is_empty() {
        crate::services::people::set_issue(&state, &p0, &p, &i, &req.assignee_ids).await?;
    }
    let i = plan(&state, &p0, &p, i, req.milestone_id, req.due_date.as_deref()).await?;
    Ok(Json(with_labels(&state, vec![i]).await?.remove(0)))
}

#[derive(Serialize, ToSchema)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: IssueDto,
    /// Sanitized HTML of the description (Markdown).
    pub description_html: String,
    pub can_edit: bool,
    /// Developer+: may set labels.
    pub can_label: bool,
    /// Reporter+ or the author: may set assignees.
    pub can_assign: bool,
    /// Merge requests that reference this issue (for readers of the code).
    pub merge_requests: Vec<RelatedMr>,
}

#[derive(Serialize, ToSchema)]
pub struct RelatedMr {
    pub iid: i64,
    pub title: String,
    /// opened, merged, closed or locked
    pub state: String,
    /// Its description closes this issue when it merges (`Closes #12`).
    pub closes: bool,
}

/// Merge requests whose title or description references the issue, or that
/// mentioned it in a comment ("mentioned in merge request !4" notes).
async fn related_mrs(state: &AppState, p: &Project, i: &Issue) -> ApiResult<Vec<RelatedMr>> {
    let mentioned: Vec<i64> = idb::notes(&state.db, i.id)
        .await?
        .iter()
        .filter(|n| n.system)
        .filter_map(|n| n.body.strip_prefix("mentioned in merge request !")?.parse().ok())
        .collect();
    let rows = forgust_db::merge_requests::referencing_issue(&state.db, p.id, i.iid, &mentioned).await?;
    Ok(rows
        .into_iter()
        .filter_map(|(iid, title, state, description)| {
            let closes = crate::services::refs::closing_issues(&description).contains(&i.iid);
            let refs = crate::render::refs_in_prose(&format!("{title}\n\n{description}"));
            (closes || mentioned.contains(&iid) || refs.issues.contains(&i.iid)).then_some(RelatedMr { iid, title, state, closes })
        })
        .collect())
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/issues/{iid}", responses((status = 200, body = IssueDetail)), tag = "issues")]
pub async fn show(State(state): State<AppState>, Viewer(v): Viewer, Path((id, iid)): Path<(String, i64)>) -> ApiResult<Json<IssueDetail>> {
    let (p, i) = load(&state, &v, &id, iid).await?;
    let can_edit = v.direct_user().is_some() && issues::may_edit(&state, &v, &p, &i).await.is_ok();
    let can_label = v.direct_user().is_some() && can_project(&state, &v, ProjectAction::Push, &p).await?;
    let can_assign = v.direct_user().is_some_and(|u| i.author_id == Some(u.id))
        || (v.direct_user().is_some() && crate::auth::authz::project_role(&state, &v, &p).await?.level >= forgust_db::access::REPORTER);
    let description_html = render_for(&state, &v, &p, &[&i.description]).await.pop().unwrap_or_default();
    // MR titles are for readers of the code (a Guest sees none)
    let merge_requests =
        if can_project(&state, &v, ProjectAction::ReadCode, &p).await? { related_mrs(&state, &p, &i).await? } else { Vec::new() };
    let issue = with_labels(&state, vec![i]).await?.remove(0);
    Ok(Json(IssueDetail { issue, description_html, can_edit, can_label, can_assign, merge_requests }))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateIssueBody {
    pub title: Option<String>,
    pub description: Option<String>,
    /// `close` or `reopen`.
    pub state_event: Option<String>,
    /// Replace the labels (names).
    pub labels: Option<Vec<String>>,
    /// Replace the assignees (user ids).
    pub assignee_ids: Option<Vec<i64>>,
    /// Put it on a milestone (`0`: take it off).
    pub milestone_id: Option<i64>,
    /// `YYYY-MM-DD`; empty clears it.
    pub due_date: Option<String>,
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/issues/{iid}", request_body = UpdateIssueBody, responses((status = 200, body = IssueDto)), tag = "issues")]
pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<UpdateIssueBody>,
) -> ApiResult<Json<IssueDto>> {
    let (p, i) = load(&state, &p0, &id, iid).await?;
    let open = match req.state_event.as_deref() {
        None => None,
        Some("close") => Some(false),
        Some("reopen") => Some(true),
        Some(_) => return Err(ApiError::bad("state_event must be close or reopen")),
    };
    let i = issues::update(
        &state,
        &p0,
        &p,
        &i,
        UpdateIssue { title: req.title.as_deref(), description: req.description.as_deref(), open, labels: req.labels.as_deref() },
    )
    .await?;
    if let Some(ids) = &req.assignee_ids {
        crate::services::people::set_issue(&state, &p0, &p, &i, ids).await?;
    }
    let i = plan(&state, &p0, &p, i, req.milestone_id, req.due_date.as_deref()).await?;
    Ok(Json(with_labels(&state, vec![i]).await?.remove(0)))
}

#[derive(Serialize, ToSchema)]
pub struct IssueNoteDto {
    #[serde(flatten)]
    pub note: IssueNote,
    /// Sanitized HTML of the body (Markdown).
    pub body_html: String,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/issues/{iid}/notes", responses((status = 200, body = [IssueNoteDto])), tag = "issues")]
pub async fn notes(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Vec<IssueNoteDto>>> {
    let (p, i) = load(&state, &v, &id, iid).await?;
    let notes = idb::notes(&state.db, i.id).await?;
    let bodies: Vec<&str> = notes.iter().map(|n| n.body.as_str()).collect();
    let html = render_for(&state, &v, &p, &bodies).await;
    let out = notes.into_iter().zip(html).map(|(n, body_html)| IssueNoteDto { body_html, note: n }).collect();
    Ok(Json(out))
}

#[derive(Deserialize, ToSchema)]
pub struct NoteBody {
    pub body: String,
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/issues/{iid}/notes", request_body = NoteBody, responses((status = 200)), tag = "issues")]
pub async fn create_note(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<NoteBody>,
) -> ApiResult<Json<Value>> {
    let (p, i) = load(&state, &p0, &id, iid).await?;
    let note_id = issues::add_note(&state, &p0, &p, &i, &req.body).await?;
    Ok(Json(json!({ "id": note_id })))
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/issues/{iid}/notes/{note_id}", request_body = NoteBody, responses((status = 200)), tag = "issues")]
pub async fn edit_note(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid, note_id)): Path<(String, i64, i64)>,
    Json(req): Json<NoteBody>,
) -> ApiResult<Json<Value>> {
    let (p, i) = load(&state, &p0, &id, iid).await?;
    issues::edit_note(&state, &p0, &p, &i, note_id, Some(&req.body)).await?;
    Ok(Json(json!({ "ok": true })))
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/issues/{iid}/notes/{note_id}", responses((status = 200)), tag = "issues")]
pub async fn delete_note(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, iid, note_id)): Path<(String, i64, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, i) = load(&state, &p0, &id, iid).await?;
    issues::edit_note(&state, &p0, &p, &i, note_id, None).await?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ labels

#[utoipa::path(get, path = "/api/v1/projects/{id}/labels", responses((status = 200, body = [LabelWithCounts])), tag = "issues")]
pub async fn labels(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<Vec<LabelWithCounts>>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    Ok(Json(idb::labels(&state.db, p.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct LabelBody {
    pub name: String,
    /// `#rrggbb`; a color is picked when left out.
    pub color: Option<String>,
    #[serde(default)]
    pub description: String,
}

/// Colors offered to new labels, in turn.
const PALETTE: [&str; 10] = ["#6366f1", "#0ea5e9", "#14b8a6", "#22c55e", "#eab308", "#f97316", "#ef4444", "#ec4899", "#a855f7", "#64748b"];

fn check_label(body: &LabelBody, fallback_index: usize) -> ApiResult<(String, String)> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 255 || name.contains(',') {
        return Err(ApiError::bad("a label name of 1-255 characters, without commas, is required"));
    }
    let color = match body.color.as_deref().map(str::trim) {
        Some(c) if c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit()) => c.to_ascii_lowercase(),
        Some(c) if !c.is_empty() => return Err(ApiError::bad("color must look like #1f75cb")),
        _ => PALETTE[fallback_index % PALETTE.len()].to_string(),
    };
    Ok((name.to_string(), color))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/labels", request_body = LabelBody, responses((status = 200, body = Label)), tag = "issues")]
pub async fn create_label(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<LabelBody>,
) -> ApiResult<Json<Label>> {
    let p = project_for(&state, &p0, &id, ProjectAction::Read).await?;
    require_project(&state, &p0, ProjectAction::Push, &p).await?;
    let existing = idb::labels(&state.db, p.id).await?;
    let (name, color) = check_label(&req, existing.len())?;
    if existing.iter().any(|l| l.name == name) {
        return Err(ApiError::conflict(format!("label `{name}` already exists")));
    }
    Ok(Json(idb::upsert_label(&state.db, p.id, &name, &color, req.description.trim()).await?))
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/labels/{label_id}", request_body = LabelBody, responses((status = 200, body = Label)), tag = "issues")]
pub async fn update_label(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, label_id)): Path<(String, i64)>,
    Json(req): Json<LabelBody>,
) -> ApiResult<Json<Label>> {
    let p = project_for(&state, &p0, &id, ProjectAction::Read).await?;
    require_project(&state, &p0, ProjectAction::Push, &p).await?;
    let current = idb::label(&state.db, p.id, label_id).await?.ok_or(ApiError::NotFound)?;
    let description = req.description.trim().to_string();
    let (name, color) = check_label(&LabelBody { color: req.color.clone().or(Some(current.color)), ..req }, 0)?;
    match idb::update_label(&state.db, label_id, &name, &color, &description).await {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(ApiError::conflict(format!("label `{name}` already exists"))),
        r => Ok(Json(r?)),
    }
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/labels/{label_id}", responses((status = 200)), tag = "issues")]
pub async fn delete_label(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, label_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &p0, &id, ProjectAction::Read).await?;
    require_project(&state, &p0, ProjectAction::Push, &p).await?;
    idb::label(&state.db, p.id, label_id).await?.ok_or(ApiError::NotFound)?;
    idb::delete_label(&state.db, label_id).await?;
    Ok(Json(json!({ "ok": true })))
}
