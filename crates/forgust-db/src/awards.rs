//! Emoji reactions (M7) on merge requests, issues and their comments.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

/// GitLab's names for GitHub's eight reactions.
pub const NAMES: &[&str] = &["thumbsup", "thumbsdown", "laughing", "tada", "confused", "heart", "rocket", "eyes"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    MergeRequest(i64),
    Issue(i64),
    /// A comment on a merge request.
    Note(i64),
    IssueNote(i64),
}

impl Target {
    fn columns(self) -> (Option<i64>, Option<i64>, Option<i64>, Option<i64>) {
        match self {
            Target::MergeRequest(id) => (Some(id), None, None, None),
            Target::Issue(id) => (None, Some(id), None, None),
            Target::Note(id) => (None, None, Some(id), None),
            Target::IssueNote(id) => (None, None, None, Some(id)),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Award {
    pub id: i64,
    pub user_id: i64,
    pub username: String,
    pub user_name: String,
    pub name: String,
    pub merge_request_id: Option<i64>,
    pub issue_id: Option<i64>,
    pub note_id: Option<i64>,
    pub issue_note_id: Option<i64>,
    pub created_at: DateTime<Utc>,
}

/// Reactions on a merge request and on all its comments, oldest first.
pub async fn of_merge_request(e: impl PgExecutor<'_>, mr_id: i64) -> Result<Vec<Award>> {
    sqlx::query_as!(
        Award,
        "SELECT a.id, a.user_id, u.username, u.name AS user_name, a.name, a.merge_request_id, a.issue_id, a.note_id, a.issue_note_id, a.created_at
           FROM award_emoji a JOIN users u ON u.id = a.user_id
          WHERE a.id IN (SELECT id FROM award_emoji WHERE merge_request_id = $1
                         UNION ALL
                         SELECT x.id FROM award_emoji x JOIN notes n ON n.id = x.note_id WHERE n.merge_request_id = $1)
          ORDER BY a.id",
        mr_id
    )
    .fetch_all(e)
    .await
}

/// Reactions on an issue and on all its comments, oldest first.
pub async fn of_issue(e: impl PgExecutor<'_>, issue_id: i64) -> Result<Vec<Award>> {
    sqlx::query_as!(
        Award,
        "SELECT a.id, a.user_id, u.username, u.name AS user_name, a.name, a.merge_request_id, a.issue_id, a.note_id, a.issue_note_id, a.created_at
           FROM award_emoji a JOIN users u ON u.id = a.user_id
          WHERE a.id IN (SELECT id FROM award_emoji WHERE issue_id = $1
                         UNION ALL
                         SELECT x.id FROM award_emoji x JOIN issue_notes n ON n.id = x.issue_note_id WHERE n.issue_id = $1)
          ORDER BY a.id",
        issue_id
    )
    .fetch_all(e)
    .await
}

/// `None` when this person already reacted so.
pub async fn add(e: impl PgExecutor<'_>, user_id: i64, name: &str, target: Target) -> Result<Option<i64>> {
    let (mr, issue, note, issue_note) = target.columns();
    sqlx::query_scalar!(
        "INSERT INTO award_emoji (user_id, name, merge_request_id, issue_id, note_id, issue_note_id) VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT DO NOTHING RETURNING id",
        user_id,
        name,
        mr,
        issue,
        note,
        issue_note
    )
    .fetch_optional(e)
    .await
}

/// Whether there was one to remove.
pub async fn remove(e: impl PgExecutor<'_>, user_id: i64, name: &str, target: Target) -> Result<bool> {
    let (mr, issue, note, issue_note) = target.columns();
    let r = sqlx::query!(
        // one target column is set (a CHECK says so): plain equalities, which
        // the per-target unique indexes serve
        "DELETE FROM award_emoji WHERE user_id = $1 AND name = $2
           AND (merge_request_id = $3 OR issue_id = $4 OR note_id = $5 OR issue_note_id = $6)",
        user_id,
        name,
        mr,
        issue,
        note,
        issue_note
    )
    .execute(e)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Delete by id, only someone's own (GitLab's `DELETE .../award_emoji/:id`).
pub async fn delete_own(e: impl PgExecutor<'_>, id: i64, user_id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM award_emoji WHERE id = $1 AND user_id = $2", id, user_id).execute(e).await?.rows_affected() > 0)
}
