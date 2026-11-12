//! To-dos: what someone was asked to look at (M6).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Todo {
    pub id: i64,
    pub project_id: i64,
    pub project_path: String,
    pub project_name: String,
    /// merge_request or issue
    pub target_type: String,
    pub target_iid: i64,
    pub target_title: String,
    pub target_state: String,
    /// assigned, review_requested or mentioned
    pub action: String,
    pub author_username: Option<String>,
    pub author_name: Option<String>,
    pub body: String,
    pub state: String,
    pub created_at: DateTime<Utc>,
}

pub struct NewTodo<'a> {
    pub user_id: i64,
    pub project_id: i64,
    pub target_type: &'a str,
    pub target_iid: i64,
    pub action: &'a str,
    pub author_id: Option<i64>,
    pub body: &'a str,
}

/// Add a to-do unless the same one (user, target, action) is still pending.
pub async fn add(e: impl PgExecutor<'_>, t: NewTodo<'_>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO todos (user_id, project_id, target_type, target_iid, action, author_id, body)
         SELECT $1, $2, $3, $4, $5, $6, $7
         WHERE NOT EXISTS (SELECT 1 FROM todos WHERE user_id = $1 AND project_id = $2 AND target_type = $3
                           AND target_iid = $4 AND action = $5 AND state = 'pending')",
        t.user_id,
        t.project_id,
        t.target_type,
        t.target_iid,
        t.action,
        t.author_id,
        t.body
    )
    .execute(e)
    .await?;
    Ok(())
}

/// A user's to-dos in `state`, newest first, with what they point at. Only
/// projects the user can still see (an authorization row) are listed.
pub async fn list(e: impl PgExecutor<'_>, user_id: i64, state: &str, limit: i64) -> Result<Vec<Todo>> {
    sqlx::query_as!(
        Todo,
        r#"SELECT t.id, t.project_id, r.path AS project_path, p.name AS project_name, t.target_type, t.target_iid,
                  coalesce(m.title, i.title, '') AS "target_title!", coalesce(m.state, i.state, '') AS "target_state!",
                  t.action, u.username AS "author_username?", u.name AS "author_name?", t.body, t.state, t.created_at
           FROM todos t
           JOIN projects p ON p.id = t.project_id
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
           JOIN listed_project_authorizations a ON a.project_id = p.id AND a.user_id = t.user_id
           LEFT JOIN merge_requests m ON t.target_type = 'merge_request' AND m.project_id = t.project_id AND m.iid = t.target_iid
           LEFT JOIN issues i ON t.target_type = 'issue' AND i.project_id = t.project_id AND i.iid = t.target_iid
           LEFT JOIN users u ON u.id = t.author_id
           WHERE t.user_id = $1 AND t.state = $2
             -- merge requests are code: Reporters and above (a Guest, or someone demoted, sees none)
             AND (t.target_type <> 'merge_request' OR a.access_level >= 20)
           ORDER BY t.created_at DESC, t.id DESC LIMIT $3"#,
        user_id,
        state,
        limit
    )
    .fetch_all(e)
    .await
}

pub async fn pending_count(e: impl PgExecutor<'_>, user_id: i64) -> Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM todos t JOIN listed_project_authorizations a ON a.project_id = t.project_id AND a.user_id = t.user_id
           WHERE t.user_id = $1 AND t.state = 'pending' AND (t.target_type <> 'merge_request' OR a.access_level >= 20)"#,
        user_id
    )
    .fetch_one(e)
    .await
}

/// Mark one (`id`) or all of a user's to-dos done; returns how many changed.
pub async fn mark_done(e: impl PgExecutor<'_>, user_id: i64, id: Option<i64>) -> Result<u64> {
    Ok(sqlx::query!(
        "UPDATE todos SET state = 'done', updated_at = now() WHERE user_id = $1 AND state = 'pending' AND ($2::bigint IS NULL OR id = $2)",
        user_id,
        id
    )
    .execute(e)
    .await?
    .rows_affected())
}

/// The user acted on a target (commented, approved, merged...): its to-dos are done.
pub async fn done_for_target(e: impl PgExecutor<'_>, user_id: i64, project_id: i64, target_type: &str, iid: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE todos SET state = 'done', updated_at = now()
         WHERE user_id = $1 AND project_id = $2 AND target_type = $3 AND target_iid = $4 AND state = 'pending'",
        user_id,
        project_id,
        target_type,
        iid
    )
    .execute(e)
    .await?;
    Ok(())
}
