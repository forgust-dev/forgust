//! Assignees and reviewers (M6).
//!
//! Anyone with at least the Reporter role on the project can be assigned or asked
//! to review. On a merge request the author or a Developer+ sets them; on an issue
//! any Reporter+ (triage) or the author. A reviewer's state follows what they do:
//! a comment marks them `reviewed`, an approval `approved`.

use forgust_db::access::REPORTER;
use forgust_db::issues::Issue;
use forgust_db::merge_requests::{self as mrdb, MergeRequest, Position};
use forgust_db::people::{self as pdb, Change};
use forgust_db::projects::Project;
use forgust_db::users::User;

use crate::auth::Principal;
use crate::auth::authz::project_role;
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::services::notify::{About, Kind};
use crate::state::AppState;

const MAX: usize = 20;

/// The users behind `ids`, each active with Reporter+ on the project.
pub async fn check(state: &AppState, p: &Project, ids: &[i64]) -> ApiResult<Vec<User>> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() > MAX {
        return Err(ApiError::bad(format!("at most {MAX} people")));
    }
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let u = forgust_db::users::find(&state.db, id).await?.filter(|u| u.is_active());
        let level = forgust_db::authz::project_level(&state.db, id, p.id).await?.unwrap_or(0);
        match u {
            Some(u) if level >= REPORTER => out.push(u),
            _ => return Err(ApiError::bad(format!("user {id} cannot be assigned: they need at least the Reporter role here"))),
        }
    }
    Ok(out)
}

fn mentions(users: &[User], ids: &[i64]) -> String {
    let names: Vec<String> = ids.iter().filter_map(|id| users.iter().find(|u| u.id == *id)).map(|u| format!("@{}", u.username)).collect();
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        n => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

/// The users behind `ids`, read on `conn` (inside the caller's transaction:
/// no second connection, and a failure is an error, not a missing person).
async fn users_of(conn: &mut sqlx::PgConnection, ids: &[i64]) -> ApiResult<Vec<User>> {
    let mut out = Vec::new();
    for id in ids {
        if let Some(u) = forgust_db::users::find(&mut *conn, *id).await? {
            out.push(u);
        }
    }
    Ok(out)
}

/// "assigned to @a and unassigned @b", or None.
fn assign_note(c: &Change, users: &[User]) -> Option<String> {
    match (c.added.is_empty(), c.removed.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("assigned to {}", mentions(users, &c.added))),
        (true, false) => Some(format!("unassigned {}", mentions(users, &c.removed))),
        (false, false) => Some(format!("assigned to {} and unassigned {}", mentions(users, &c.added), mentions(users, &c.removed))),
    }
}

fn review_note(c: &Change, users: &[User]) -> Option<String> {
    match (c.added.is_empty(), c.removed.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("requested review from {}", mentions(users, &c.added))),
        (true, false) => Some(format!("removed review request for {}", mentions(users, &c.removed))),
        (false, false) => Some(format!(
            "requested review from {} and removed review request for {}",
            mentions(users, &c.added),
            mentions(users, &c.removed)
        )),
    }
}

/// Replace an MR's assignees and/or reviewers (`None` leaves that list alone).
pub async fn set_mr(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    m: &MergeRequest,
    assignees: Option<&[i64]>,
    reviewers: Option<&[i64]>,
) -> ApiResult<()> {
    if assignees.is_none() && reviewers.is_none() {
        return Ok(());
    }
    super::mr::may_edit(state, principal, p, m).await?;
    let actor = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    let new_assignees = match assignees {
        Some(ids) => Some(check(state, p, ids).await?),
        None => None,
    };
    let new_reviewers = match reviewers {
        Some(ids) => Some(check(state, p, ids).await?),
        None => None,
    };
    let mut tx = state.db.begin().await?;
    let mut assigned = Change::default();
    let mut requested = Change::default();
    if let Some(list) = &new_assignees {
        assigned = pdb::set_mr_assignees(&mut tx, m.id, &list.iter().map(|u| u.id).collect::<Vec<_>>()).await?;
    }
    if let Some(list) = &new_reviewers {
        requested = pdb::set_mr_reviewers(&mut tx, m.id, &list.iter().map(|u| u.id).collect::<Vec<_>>()).await?;
    }
    let everyone = users_of(&mut tx, &[&assigned.added[..], &assigned.removed, &requested.added, &requested.removed].concat()).await?;
    for text in [assign_note(&assigned, &everyone), review_note(&requested, &everyone)].into_iter().flatten() {
        mrdb::add_note(&mut *tx, m.id, Some(actor.id), None, &text, true, Position::default()).await?;
    }
    let notify = |ids: &[i64]| everyone.iter().filter(|u| ids.contains(&u.id) && u.id != actor.id).cloned().collect::<Vec<_>>();
    todo(&mut tx, p, "merge_request", m.iid, "assigned", actor.id, &notify(&assigned.added)).await?;
    todo(&mut tx, p, "merge_request", m.iid, "review_requested", actor.id, &notify(&requested.added)).await?;
    mrdb::touch(&mut *tx, m.id).await?;
    tx.commit().await?;
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: m.iid });
    let what = format!("merge request !{} \"{}\"", m.iid, m.title);
    let url = state.cfg.mr_url(&p.full_path, m.iid);
    let subject = format!("{} !{}: {}", p.full_path, m.iid, m.title);
    let direct = super::notify::Reason::Direct;
    super::notify::tell(
        state,
        p,
        actor.id,
        &notify(&assigned.added),
        direct,
        super::notify::mr_thread(m.id),
        About::mr(Kind::Assignment, m, "assigned you"),
        &subject,
        &format!("{} assigned you to {what}.\n\n{url}\n", actor.username),
    )
    .await;
    super::notify::tell(
        state,
        p,
        actor.id,
        &notify(&requested.added),
        direct,
        super::notify::mr_thread(m.id),
        About::mr(Kind::Assignment, m, "asked you to review"),
        &subject,
        &format!("{} asked you to review {what}.\n\n{url}\n", actor.username),
    )
    .await;
    Ok(())
}

/// Ask a reviewer to look again: their state goes back to `unreviewed`.
pub async fn rerequest(state: &AppState, principal: &Principal, p: &Project, m: &MergeRequest, user_id: i64) -> ApiResult<()> {
    super::mr::may_edit(state, principal, p, m).await?;
    let actor = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    if !pdb::mr_reviewers(&state.db, &[m.id]).await?.iter().any(|(_, r)| r.user.id == user_id) {
        return Err(ApiError::NotFound);
    }
    let mut tx = state.db.begin().await?;
    pdb::set_review_state(&mut *tx, m.id, user_id, "unreviewed", None).await?;
    let users = users_of(&mut tx, &[user_id]).await?;
    let text = format!("requested review from {}", mentions(&users, &[user_id]));
    mrdb::add_note(&mut *tx, m.id, Some(actor.id), None, &text, true, Position::default()).await?;
    mrdb::touch(&mut *tx, m.id).await?;
    let to: Vec<User> = users.into_iter().filter(|u| u.id != actor.id).collect();
    todo(&mut tx, p, "merge_request", m.iid, "review_requested", actor.id, &to).await?;
    tx.commit().await?;
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: m.iid });
    let url = state.cfg.mr_url(&p.full_path, m.iid);
    super::notify::tell(
        state,
        p,
        actor.id,
        &to,
        super::notify::Reason::Direct,
        super::notify::mr_thread(m.id),
        About::mr(Kind::Assignment, m, "asked you to review again"),
        &format!("{} !{}: {}", p.full_path, m.iid, m.title),
        &format!("{} asked you to review merge request !{} \"{}\" again.\n\n{url}\n", actor.username, m.iid, m.title),
    )
    .await;
    Ok(())
}

/// A reviewer acted: record it (no-op for people who are not reviewers).
pub async fn reviewed(state: &AppState, m: &MergeRequest, user_id: i64, action: ReviewAction) {
    let r = match action {
        ReviewAction::Commented => pdb::set_review_state(&state.db, m.id, user_id, "reviewed", Some("unreviewed")).await,
        ReviewAction::Approved => pdb::set_review_state(&state.db, m.id, user_id, "approved", None).await,
        ReviewAction::Unapproved => pdb::set_review_state(&state.db, m.id, user_id, "reviewed", Some("approved")).await,
        ReviewAction::RequestedChanges => pdb::set_review_state(&state.db, m.id, user_id, "requested_changes", None).await,
    };
    if let Err(e) = r {
        tracing::warn!(mr = m.id, user_id, error = %e, "review state not recorded");
    }
}

/// To-dos for `users`, in the caller's transaction.
async fn todo(
    conn: &mut sqlx::PgConnection,
    p: &Project,
    kind: &str,
    iid: i64,
    action: &str,
    author: i64,
    users: &[User],
) -> ApiResult<()> {
    for u in users {
        let t = forgust_db::todos::NewTodo {
            user_id: u.id,
            project_id: p.id,
            target_type: kind,
            target_iid: iid,
            action,
            author_id: Some(author),
            body: "",
        };
        forgust_db::todos::add(&mut *conn, t).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum ReviewAction {
    Commented,
    Approved,
    Unapproved,
    RequestedChanges,
}

/// Replace an issue's assignees: Reporter+ or the author.
pub async fn set_issue(state: &AppState, principal: &Principal, p: &Project, i: &Issue, assignees: &[i64]) -> ApiResult<()> {
    crate::auth::authz::writable(p)?;
    let actor = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    let level = project_role(state, principal, p).await?.level;
    if level < REPORTER && i.author_id != Some(actor.id) {
        return Err(ApiError::forbidden("assigning issues needs at least the Reporter role"));
    }
    let list = check(state, p, assignees).await?;
    let mut tx = state.db.begin().await?;
    let c = forgust_db::people::set_issue_assignees(&mut tx, i.id, &list.iter().map(|u| u.id).collect::<Vec<_>>()).await?;
    let everyone = users_of(&mut tx, &[&c.added[..], &c.removed].concat()).await?;
    if let Some(text) = assign_note(&c, &everyone) {
        forgust_db::issues::add_note(
            &mut *tx,
            forgust_db::issues::NewIssueNote {
                issue_id: i.id,
                author_id: Some(actor.id),
                imported_author: None,
                body: &text,
                system: true,
                created_at: None,
            },
        )
        .await?;
        forgust_db::issues::touch(&mut *tx, i.id).await?;
    }
    let to: Vec<User> = everyone.into_iter().filter(|u| c.added.contains(&u.id) && u.id != actor.id).collect();
    todo(&mut tx, p, "issue", i.iid, "assigned", actor.id, &to).await?;
    tx.commit().await?;
    let url = state.cfg.issue_url(&p.full_path, i.iid);
    super::notify::tell(
        state,
        p,
        actor.id,
        &to,
        super::notify::Reason::Direct,
        super::notify::issue_thread(i.id),
        About::issue(Kind::Assignment, i, "assigned you"),
        &format!("{} #{}: {}", p.full_path, i.iid, i.title),
        &format!("{} assigned you to issue #{} \"{}\".\n\n{url}\n", actor.username, i.iid, i.title),
    )
    .await;
    Ok(())
}

/// A reviewer asks for changes: merging is blocked until they approve or are
/// asked to review again. Anyone who may approve can do it (they become a
/// reviewer). The comment, when there is one, says what to change: it is posted
/// like any comment and carries the verdict.
pub async fn request_changes(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    m: &MergeRequest,
    comment: Option<&str>,
) -> ApiResult<()> {
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    if project_role(state, principal, p).await?.level < REPORTER {
        return Err(ApiError::forbidden("reviewing needs at least the Reporter role"));
    }
    if m.author_id == Some(user.id) {
        return Err(ApiError::forbidden("authors cannot request changes on their own merge request"));
    }
    if m.state != "opened" {
        return Err(ApiError::conflict("the merge request is not open"));
    }
    let comment = comment.map(str::trim).filter(|c| !c.is_empty());
    let note = match comment {
        Some(text) => Some(super::mr::add_note(state, principal, p, m, text, None, None).await?.id),
        None => None,
    };
    let mut tx = state.db.begin().await?;
    mrdb::unapprove(&mut *tx, m.id, user.id).await?;
    pdb::upsert_review(&mut *tx, m.id, user.id, "requested_changes", note).await?;
    match note {
        Some(id) => mrdb::set_note_review(&mut *tx, id, "requested_changes").await?,
        None => {
            mrdb::add_note(&mut *tx, m.id, Some(user.id), None, "requested changes", true, Position::default()).await?;
        }
    }
    mrdb::touch(&mut *tx, m.id).await?;
    tx.commit().await?;
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: m.iid });
    if let Some(author) = match m.author_id {
        Some(id) => forgust_db::users::find(&state.db, id).await?,
        None => None,
    } {
        let url = state.cfg.mr_url(&p.full_path, m.iid);
        super::notify::tell(
            state,
            p,
            user.id,
            &[author],
            super::notify::Reason::Direct,
            super::notify::mr_thread(m.id),
            About::mr(Kind::Review, m, "requested changes"),
            &format!("{} !{}: {}", p.full_path, m.iid, m.title),
            &match comment {
                Some(text) => {
                    format!("{} requested changes on merge request !{} \"{}\":\n\n{text}\n\n{url}\n", user.username, m.iid, m.title)
                }
                None => format!("{} requested changes on merge request !{} \"{}\".\n\n{url}\n", user.username, m.iid, m.title),
            },
        )
        .await;
    }
    Ok(())
}

/// Link the authors and committers of `commits` to the accounts that have
/// their email (verified), so pages can show who it is. Best effort.
pub async fn link_commits<'a>(state: &AppState, commits: impl IntoIterator<Item = &'a mut forgust_storage::CommitInfo>) {
    let people: Vec<&mut forgust_storage::Person> = commits.into_iter().flat_map(|c| [&mut c.author, &mut c.committer]).collect();
    let mut emails: Vec<String> = people.iter().map(|p| p.email.to_lowercase()).collect();
    emails.sort();
    emails.dedup();
    if emails.is_empty() {
        return;
    }
    let found: std::collections::HashMap<String, String> = match pdb::usernames_by_email(&state.db, &emails).await {
        Ok(rows) => rows.into_iter().collect(),
        Err(e) => {
            tracing::warn!(error = %e, "commit authors not linked");
            return;
        }
    };
    for p in people {
        p.username = found.get(&p.email.to_lowercase()).cloned();
    }
}
