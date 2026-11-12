//! Issues (M5): minimal by design (title, body, state, labels, comments), so the
//! importer has somewhere to put the issues it moves.
//!
//! Anyone who can see a project can open issues and comment; the author or a
//! Developer+ can edit, close and relabel; Maintainers can delete any comment.

use forgust_db::issues::{self as idb, Issue, Label, NewIssue, NewIssueNote};
use forgust_db::merge_requests::next_iid;
use forgust_db::projects::Project;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

fn check_title(t: &str) -> ApiResult<&str> {
    let t = t.trim();
    if t.is_empty() || t.len() > 255 {
        return Err(ApiError::bad("a title of 1-255 characters is required"));
    }
    Ok(t)
}

/// Label ids for names; every name must exist in the project.
pub async fn resolve_labels(state: &AppState, project_id: i64, names: &[String]) -> ApiResult<Vec<Label>> {
    let mut names: Vec<String> = names.iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
    names.sort();
    names.dedup();
    let found = idb::labels_by_name(&state.db, project_id, &names).await?;
    if let Some(missing) = names.iter().find(|n| !found.iter().any(|l| &l.name == *n)) {
        return Err(ApiError::bad(format!("no label `{missing}` in this project")));
    }
    Ok(found)
}

/// Colors given to labels created on the fly, in turn.
const PALETTE: [&str; 10] = ["#6366f1", "#0ea5e9", "#14b8a6", "#22c55e", "#eab308", "#f97316", "#ef4444", "#ec4899", "#a855f7", "#64748b"];

/// Create the labels of `names` the project lacks (GitLab does this when an
/// issue or MR names a new label). Setting labels is for Developers.
pub async fn ensure_labels(state: &AppState, principal: &Principal, p: &Project, names: &[String]) -> ApiResult<()> {
    if names.is_empty() {
        return Ok(());
    }
    require_project(state, principal, ProjectAction::Push, p).await.map_err(|_| ApiError::forbidden("only Developers can set labels"))?;
    let project_id = p.id;
    let existing = idb::labels(&state.db, project_id).await?;
    let mut n = existing.len();
    for name in names.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if name.len() > 255 || name.contains(',') || existing.iter().any(|l| l.name == name) {
            continue;
        }
        idb::upsert_label(&state.db, project_id, name, PALETTE[n % PALETTE.len()], "").await?;
        n += 1;
    }
    Ok(())
}

pub struct CreateIssue<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub labels: &'a [String],
}

pub async fn create(state: &AppState, principal: &Principal, p: &Project, req: CreateIssue<'_>) -> ApiResult<Issue> {
    require_project(state, principal, ProjectAction::Read, p).await?;
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let title = check_title(req.title)?;
    // labels are a triage tool: only Developer+ may set them
    let labels = if req.labels.is_empty() {
        Vec::new()
    } else {
        require_project(state, principal, ProjectAction::Push, p).await?;
        resolve_labels(state, p.id, req.labels).await?
    };
    let mut tx = state.db.begin().await?;
    let iid = next_iid(&mut *tx, p.id, "issues").await?;
    let id = idb::insert(
        &mut *tx,
        NewIssue {
            project_id: p.id,
            iid,
            title,
            description: req.description,
            author_id: Some(user.id),
            imported_author: None,
            state: "opened",
            created_at: None,
            updated_at: None,
            closed_at: None,
            milestone_id: None,
            due_date: None,
        },
    )
    .await?;
    idb::set_issue_labels(&mut tx, id, &labels.iter().map(|l| l.id).collect::<Vec<_>>()).await?;
    tx.commit().await?;
    let issue = idb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    super::refs::on_write(state, p, user, issue_target(&issue), &issue.description, None).await;
    super::notify::new_issue(state, p, &issue, user).await;
    Ok(issue)
}

pub(crate) fn issue_target(i: &Issue) -> super::refs::Target<'_> {
    super::refs::Target::Issue { id: i.id, iid: i.iid, title: &i.title }
}

async fn done(state: &AppState, user_id: i64, p: &Project, i: &Issue) {
    if let Err(e) = forgust_db::todos::done_for_target(&state.db, user_id, p.id, "issue", i.iid).await {
        tracing::warn!(error = %e, "to-dos not marked done");
    }
}

/// The author or a Developer+ may edit, close and reopen.
pub async fn may_edit(state: &AppState, principal: &Principal, p: &Project, issue: &Issue) -> ApiResult<()> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    if issue.author_id == Some(user.id) {
        require_project(state, principal, ProjectAction::Read, p).await?;
        return crate::auth::authz::writable(p);
    }
    require_project(state, principal, ProjectAction::Push, p).await
}

#[derive(Default)]
pub struct UpdateIssue<'a> {
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    /// Some(true) reopens, Some(false) closes.
    pub open: Option<bool>,
    /// Replace the labels (names).
    pub labels: Option<&'a [String]>,
}

pub async fn update(state: &AppState, principal: &Principal, p: &Project, issue: &Issue, req: UpdateIssue<'_>) -> ApiResult<Issue> {
    may_edit(state, principal, p, issue).await?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let new_labels = match req.labels {
        Some(names) => {
            require_project(state, principal, ProjectAction::Push, p).await?;
            Some(resolve_labels(state, p.id, names).await?)
        }
        None => None,
    };
    let mut tx = state.db.begin().await?;
    if req.title.is_some() || req.description.is_some() {
        let title = check_title(req.title.unwrap_or(&issue.title))?;
        idb::update_details(&mut *tx, issue.id, title, req.description.unwrap_or(&issue.description)).await?;
    }
    let system = |body: &str| SystemNote { issue_id: issue.id, author_id: Some(user.id), body: body.to_string() };
    if let Some(open) = req.open
        && open != (issue.state == "opened")
    {
        idb::set_state(&mut *tx, issue.id, open, Some(user.id)).await?;
        let n = system(if open { "reopened" } else { "closed" });
        idb::add_note(&mut *tx, n.as_new()).await?;
    }
    if let Some(labels) = new_labels {
        let before: Vec<Label> = idb::labels_of_issues(&mut *tx, &[issue.id]).await?.into_iter().map(|(_, l)| l).collect();
        let added: Vec<&str> = labels.iter().filter(|l| !before.iter().any(|b| b.id == l.id)).map(|l| l.name.as_str()).collect();
        let removed: Vec<&str> = before.iter().filter(|b| !labels.iter().any(|l| l.id == b.id)).map(|l| l.name.as_str()).collect();
        idb::set_issue_labels(&mut tx, issue.id, &labels.iter().map(|l| l.id).collect::<Vec<_>>()).await?;
        if let Some(text) = label_change(&added, &removed) {
            idb::add_note(&mut *tx, system(&text).as_new()).await?;
        }
        idb::touch(&mut *tx, issue.id).await?;
    }
    tx.commit().await?;
    let now = idb::find(&state.db, p.id, issue.iid).await?.ok_or(ApiError::NotFound)?;
    if now.description != issue.description {
        super::refs::on_write(state, p, user, issue_target(&now), &now.description, Some(&issue.description)).await;
        // a ticked box in the Dependency Dashboard
        super::renovate::on_description_edit(state, p.id, issue.author_id, user, &issue.description, &now.description).await;
    }
    if req.open.is_some() {
        done(state, user.id, p, issue).await;
    }
    if now.state != issue.state {
        super::notify::issue_state(state, p, &now, now.state == "opened", user).await;
    }
    Ok(now)
}

/// "added ~bug ~ui and removed ~triage", or None when nothing changed.
pub fn label_change(added: &[&str], removed: &[&str]) -> Option<String> {
    let list = |v: &[&str]| v.iter().map(|n| format!("~\"{n}\"")).collect::<Vec<_>>().join(" ");
    match (added.is_empty(), removed.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("added {}", list(added))),
        (true, false) => Some(format!("removed {}", list(removed))),
        (false, false) => Some(format!("added {} and removed {}", list(added), list(removed))),
    }
}

pub async fn add_note(state: &AppState, principal: &Principal, p: &Project, issue: &Issue, body: &str) -> ApiResult<i64> {
    require_project(state, principal, ProjectAction::Read, p).await?;
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    if body.trim().is_empty() {
        return Err(ApiError::bad("the comment is empty"));
    }
    let mut tx = state.db.begin().await?;
    let id = idb::add_note(
        &mut *tx,
        NewIssueNote { issue_id: issue.id, author_id: Some(user.id), imported_author: None, body, system: false, created_at: None },
    )
    .await?;
    idb::touch(&mut *tx, issue.id).await?;
    tx.commit().await?;
    let mentioned = super::refs::on_write(state, p, user, issue_target(issue), body, None).await;
    super::notify::issue_note(state, p, issue, user, body, &mentioned).await;
    done(state, user.id, p, issue).await;
    Ok(id)
}

/// Authors edit their own comments; Maintainers can delete any.
pub async fn edit_note(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    issue: &Issue,
    note_id: i64,
    body: Option<&str>,
) -> ApiResult<()> {
    let note = idb::note(&state.db, issue.id, note_id).await?.ok_or(ApiError::NotFound)?;
    super::check_note_change(state, principal, p, note.author_id, note.system, body).await?;
    match body {
        Some(b) => {
            idb::edit_note(&state.db, note.id, b).await?;
            if let Some(user) = principal.direct_user() {
                super::refs::on_write(state, p, user, issue_target(issue), b, Some(&note.body)).await;
            }
        }
        None => idb::delete_note(&state.db, note.id).await?,
    }
    Ok(())
}

/// A system note ("closed", "added ~bug") with an owned body.
struct SystemNote {
    issue_id: i64,
    author_id: Option<i64>,
    body: String,
}

impl SystemNote {
    fn as_new(&self) -> NewIssueNote<'_> {
        NewIssueNote {
            issue_id: self.issue_id,
            author_id: self.author_id,
            imported_author: None,
            body: &self.body,
            system: true,
            created_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn label_change_text() {
        assert_eq!(super::label_change(&[], &[]), None);
        assert_eq!(super::label_change(&["bug"], &[]).as_deref(), Some("added ~\"bug\""));
        assert_eq!(super::label_change(&["a", "b"], &["c"]).as_deref(), Some("added ~\"a\" ~\"b\" and removed ~\"c\""));
    }
}
