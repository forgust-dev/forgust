//! Milestones (M7). Developers manage them and set them on issues and merge
//! requests (as they set labels); everyone who sees the project sees them.

use chrono::NaiveDate;
use forgust_db::milestones::{Milestone, MilestoneFields};
use forgust_db::projects::Project;
use serde::Deserialize;
use serde_json::json;
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct MilestoneBody {
    pub title: Option<String>,
    pub description: Option<String>,
    /// `YYYY-MM-DD`; empty clears it.
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    /// active or closed (or GitLab's `state_event`: activate, close)
    pub state: Option<String>,
    pub state_event: Option<String>,
}

/// A date field: absent = unchanged, empty = cleared.
pub fn parse_date(raw: Option<&str>, current: Option<NaiveDate>) -> ApiResult<Option<NaiveDate>> {
    match raw.map(str::trim) {
        None => Ok(current),
        Some("") => Ok(None),
        Some(d) => {
            NaiveDate::parse_from_str(d, "%Y-%m-%d").map(Some).map_err(|_| ApiError::bad(format!("`{d}` is not a date (YYYY-MM-DD)")))
        }
    }
}

fn state_of(b: &MilestoneBody, current: &str) -> ApiResult<String> {
    let s = match (b.state.as_deref(), b.state_event.as_deref()) {
        (Some(s), _) => s,
        (None, Some("close")) => "closed",
        (None, Some("activate")) => "active",
        (None, Some(other)) => return Err(ApiError::bad(format!("unknown state_event `{other}`"))),
        (None, None) => current,
    };
    match s {
        "active" | "closed" => Ok(s.to_string()),
        _ => Err(ApiError::bad("state must be active or closed")),
    }
}

fn title_of(raw: &str) -> ApiResult<&str> {
    let t = raw.trim();
    if t.is_empty() || t.len() > 255 {
        return Err(ApiError::bad("a title of 1-255 characters is required"));
    }
    Ok(t)
}

pub async fn find(state: &AppState, principal: &Principal, p: &Project, id: i64) -> ApiResult<Milestone> {
    require_project(state, principal, ProjectAction::Read, p).await?;
    forgust_db::milestones::find(&state.db, p.id, id).await?.ok_or(ApiError::NotFound)
}

pub async fn create(state: &AppState, principal: &Principal, p: &Project, b: &MilestoneBody) -> ApiResult<Milestone> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let title = title_of(b.title.as_deref().unwrap_or(""))?;
    let description = b.description.as_deref().unwrap_or("");
    let fields = MilestoneFields {
        title,
        description,
        start_date: parse_date(b.start_date.as_deref(), None)?,
        due_date: parse_date(b.due_date.as_deref(), None)?,
    };
    let st = state_of(b, "active")?;
    let mut conn = state.db.acquire().await?;
    let id = forgust_db::milestones::create(&mut conn, p.id, fields, &st)
        .await?
        .ok_or_else(|| ApiError::conflict(format!("a milestone called `{title}` already exists")))?;
    super::audit(state, principal, "milestone_created", Some(("project", p.id)), json!({ "title": title }), None).await;
    forgust_db::milestones::find(&state.db, p.id, id).await?.ok_or(ApiError::NotFound)
}

pub async fn update(state: &AppState, principal: &Principal, p: &Project, id: i64, b: &MilestoneBody) -> ApiResult<Milestone> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let m = forgust_db::milestones::find(&state.db, p.id, id).await?.ok_or(ApiError::NotFound)?;
    let title = title_of(b.title.as_deref().unwrap_or(&m.title))?;
    let fields = MilestoneFields {
        title,
        description: b.description.as_deref().unwrap_or(&m.description),
        start_date: parse_date(b.start_date.as_deref(), m.start_date)?,
        due_date: parse_date(b.due_date.as_deref(), m.due_date)?,
    };
    if !forgust_db::milestones::update(&state.db, m.id, fields, &state_of(b, &m.state)?).await? {
        return Err(ApiError::conflict(format!("a milestone called `{title}` already exists")));
    }
    forgust_db::milestones::find(&state.db, p.id, id).await?.ok_or(ApiError::NotFound)
}

pub async fn delete(state: &AppState, principal: &Principal, p: &Project, id: i64) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let m = forgust_db::milestones::find(&state.db, p.id, id).await?.ok_or(ApiError::NotFound)?;
    forgust_db::milestones::delete(&state.db, m.id).await?;
    super::audit(state, principal, "milestone_deleted", Some(("project", p.id)), json!({ "title": m.title }), None).await;
    Ok(())
}

/// A list's `milestone=` filter: a title, `None` (none set) or `Any`/empty (no filter).
/// An unknown title matches nothing.
pub async fn filter(state: &AppState, project_id: i64, title: Option<&str>) -> ApiResult<Option<i64>> {
    match title.map(str::trim) {
        None | Some("") | Some("Any") => Ok(None),
        Some("None") => Ok(Some(0)),
        Some(t) => Ok(Some(
            forgust_db::milestones::ids_by_title(&state.db, project_id)
                .await?
                .into_iter()
                .find(|(n, _)| n == t)
                .map(|(_, id)| id)
                .unwrap_or(-1),
        )),
    }
}

/// The milestone an issue or merge request may be put on: one of the project's
/// (`0` or `None` clears it). Developers only, as labels.
pub async fn check_target(state: &AppState, principal: &Principal, p: &Project, milestone_id: Option<i64>) -> ApiResult<Option<Milestone>> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    match milestone_id.filter(|id| *id != 0) {
        None => Ok(None),
        Some(id) => Ok(Some(
            forgust_db::milestones::find(&state.db, p.id, id).await?.ok_or_else(|| ApiError::bad("no such milestone in this project"))?,
        )),
    }
}

fn issue_note<'a>(issue_id: i64, author: i64, body: &'a str) -> forgust_db::issues::NewIssueNote<'a> {
    forgust_db::issues::NewIssueNote { issue_id, author_id: Some(author), imported_author: None, body, system: true, created_at: None }
}

/// The system note of a milestone change (`%"title"` references it).
fn milestone_note(new: Option<&Milestone>, old_title: Option<&str>) -> String {
    match new {
        Some(m) => format!("changed milestone to %\"{}\"", m.title),
        None => format!("removed milestone %\"{}\"", old_title.unwrap_or("")),
    }
}

/// Put an issue on a milestone (`None`/`0`: off it), with a system note.
pub async fn set_issue(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    i: &forgust_db::issues::Issue,
    milestone_id: Option<i64>,
) -> ApiResult<()> {
    let m = check_target(state, principal, p, milestone_id).await?;
    if m.as_ref().map(|m| m.id) == i.milestone_id {
        return Ok(());
    }
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let text = milestone_note(m.as_ref(), i.milestone_title.as_deref());
    let mut tx = state.db.begin().await?;
    forgust_db::milestones::set_issue(&mut *tx, i.id, m.map(|m| m.id)).await?;
    forgust_db::issues::add_note(&mut *tx, issue_note(i.id, user.id, &text)).await?;
    tx.commit().await?;
    Ok(())
}

/// Set or clear an issue's due date (Developers, or the author), with a system note.
pub async fn set_issue_due_date(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    i: &forgust_db::issues::Issue,
    due: Option<NaiveDate>,
) -> ApiResult<()> {
    super::issues::may_edit(state, principal, p, i).await?;
    if due == i.due_date {
        return Ok(());
    }
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let text = match due {
        Some(d) => format!("changed due date to {}", d.format("%B %-d, %Y")),
        None => "removed due date".to_string(),
    };
    let mut tx = state.db.begin().await?;
    forgust_db::milestones::set_issue_due_date(&mut *tx, i.id, due).await?;
    forgust_db::issues::add_note(&mut *tx, issue_note(i.id, user.id, &text)).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_merge_request(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    mr: &forgust_db::merge_requests::MergeRequest,
    milestone_id: Option<i64>,
) -> ApiResult<()> {
    let m = check_target(state, principal, p, milestone_id).await?;
    if m.as_ref().map(|m| m.id) != mr.milestone_id {
        let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
        let text = milestone_note(m.as_ref(), mr.milestone_title.as_deref());
        let mut tx = state.db.begin().await?;
        forgust_db::milestones::set_merge_request(&mut *tx, mr.id, m.map(|m| m.id)).await?;
        forgust_db::merge_requests::add_note(&mut *tx, mr.id, Some(user.id), None, &text, true, Default::default()).await?;
        tx.commit().await?;
        state.events.publish(crate::events::Event::MergeRequest { project_id: p.id, iid: mr.iid });
    }
    Ok(())
}
