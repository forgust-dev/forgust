//! Emoji reactions (M7): anyone signed in who can see a merge request or issue
//! reacts to it or to its comments, with one of eight emoji.

use std::collections::BTreeMap;

use forgust_db::awards::{Award, NAMES, Target};
use forgust_db::projects::Project;
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Who reacted so, at most this many named.
const NAMED: usize = 10;

#[derive(Debug, Serialize, ToSchema)]
pub struct Reaction {
    pub name: String,
    pub count: usize,
    /// The viewer is one of them.
    pub me: bool,
    /// Usernames, the first few.
    pub users: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Reactions {
    /// On the merge request or issue itself.
    pub target: Vec<Reaction>,
    /// Per comment id.
    pub notes: BTreeMap<i64, Vec<Reaction>>,
}

fn group(list: &[&Award], viewer: Option<i64>) -> Vec<Reaction> {
    NAMES
        .iter()
        .filter_map(|n| {
            let of: Vec<&&Award> = list.iter().filter(|a| a.name == *n).collect();
            (!of.is_empty()).then(|| Reaction {
                name: n.to_string(),
                count: of.len(),
                me: viewer.is_some_and(|v| of.iter().any(|a| a.user_id == v)),
                users: of.iter().take(NAMED).map(|a| a.username.clone()).collect(),
            })
        })
        .collect()
}

pub fn summarize(all: &[Award], viewer: Option<i64>) -> Reactions {
    let target: Vec<&Award> = all.iter().filter(|a| a.merge_request_id.is_some() || a.issue_id.is_some()).collect();
    let mut by_note: BTreeMap<i64, Vec<&Award>> = BTreeMap::new();
    for a in all {
        if let Some(n) = a.note_id.or(a.issue_note_id) {
            by_note.entry(n).or_default().push(a);
        }
    }
    Reactions { target: group(&target, viewer), notes: by_note.into_iter().map(|(n, l)| (n, group(&l, viewer))).collect() }
}

/// What is reacted to: an MR or issue (by iid), or one of its comments.
#[derive(Debug, Clone, Copy)]
pub enum On {
    MergeRequest { iid: i64, note_id: Option<i64> },
    Issue { iid: i64, note_id: Option<i64> },
}

/// The row to react to, checked: the viewer may see it, and the comment is on it.
async fn resolve(state: &AppState, principal: &Principal, p: &Project, on: On) -> ApiResult<Target> {
    match on {
        On::MergeRequest { iid, note_id } => {
            require_project(state, principal, ProjectAction::ReadCode, p).await?;
            let m = forgust_db::merge_requests::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
            match note_id {
                None => Ok(Target::MergeRequest(m.id)),
                Some(n) => {
                    let note = forgust_db::merge_requests::note(&state.db, m.id, n).await?.ok_or(ApiError::NotFound)?;
                    Ok(Target::Note(note.id))
                }
            }
        }
        On::Issue { iid, note_id } => {
            require_project(state, principal, ProjectAction::Read, p).await?;
            let i = forgust_db::issues::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
            match note_id {
                None => Ok(Target::Issue(i.id)),
                Some(n) => {
                    let note = forgust_db::issues::note(&state.db, i.id, n).await?.ok_or(ApiError::NotFound)?;
                    Ok(Target::IssueNote(note.id))
                }
            }
        }
    }
}

fn check_name(name: &str) -> ApiResult<&str> {
    NAMES.iter().find(|n| **n == name).copied().ok_or_else(|| ApiError::bad(format!("reactions are {}", NAMES.join(", "))))
}

/// Add or take back a reaction; whether it is there after.
pub async fn toggle(state: &AppState, principal: &Principal, p: &Project, on: On, name: &str) -> ApiResult<bool> {
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let name = check_name(name)?;
    let target = resolve(state, principal, p, on).await?;
    if forgust_db::awards::remove(&state.db, user.id, name, target).await? {
        return Ok(false);
    }
    forgust_db::awards::add(&state.db, user.id, name, target).await?;
    Ok(true)
}

/// Add one (GitLab's POST); `None` when it was already there.
pub async fn add(state: &AppState, principal: &Principal, p: &Project, on: On, name: &str) -> ApiResult<Option<i64>> {
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let name = check_name(name)?;
    let target = resolve(state, principal, p, on).await?;
    Ok(forgust_db::awards::add(&state.db, user.id, name, target).await?)
}

/// All reactions on the MR or issue and its comments, as the viewer sees them.
pub async fn list(state: &AppState, principal: &Principal, p: &Project, on: On) -> ApiResult<Vec<Award>> {
    let target = resolve(state, principal, p, on).await?;
    Ok(match target {
        Target::MergeRequest(id) => forgust_db::awards::of_merge_request(&state.db, id).await?,
        Target::Issue(id) => forgust_db::awards::of_issue(&state.db, id).await?,
        _ => return Err(ApiError::bad("reactions are listed per merge request or issue")),
    })
}
