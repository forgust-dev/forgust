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
    /// Made by an internal comment: listed only while its owner may read those.
    pub internal: bool,
}

/// Add a to-do unless the same one (user, target, action) is still pending. A
/// public one replaces a pending internal one (which someone who lost sight of
/// internal notes would not see), so the public mention is not lost.
pub async fn add(e: impl PgExecutor<'_>, t: NewTodo<'_>) -> Result<()> {
    sqlx::query!(
        "WITH public AS (
             UPDATE todos SET internal = false, body = $7, author_id = $6, updated_at = now()
             WHERE user_id = $1 AND project_id = $2 AND target_type = $3 AND target_iid = $4 AND action = $5
               AND state = 'pending' AND internal AND NOT $8)
         INSERT INTO todos (user_id, project_id, target_type, target_iid, action, author_id, body, internal)
         SELECT $1, $2, $3, $4, $5, $6, $7, $8
         WHERE NOT EXISTS (SELECT 1 FROM todos WHERE user_id = $1 AND project_id = $2 AND target_type = $3
                           AND target_iid = $4 AND action = $5 AND state = 'pending')",
        t.user_id,
        t.project_id,
        t.target_type,
        t.target_iid,
        t.action,
        t.author_id,
        t.body,
        t.internal
    )
    .execute(e)
    .await?;
    Ok(())
}

/// A user's to-dos in `state`, newest first, with what they point at. Only
/// projects the user can still see (an authorization row) are listed, and
/// issues they still may see (a confidential one they lost sight of is not),
/// and internal comments only while they still may read them.
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
           JOIN users me ON me.id = t.user_id
           WHERE t.user_id = $1 AND t.state = $2
             -- merge requests are code: Reporters and above (a Guest, or someone demoted, sees none)
             AND (t.target_type <> 'merge_request' OR a.access_level >= 20)
             AND (i.id IS NULL OR issue_visible(i, me.id, me.is_admin))
             AND (NOT t.internal OR sees_internal(t.project_id, me.id, me.is_admin))
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
           JOIN users me ON me.id = t.user_id
           LEFT JOIN issues i ON t.target_type = 'issue' AND i.project_id = t.project_id AND i.iid = t.target_iid
           WHERE t.user_id = $1 AND t.state = 'pending' AND (t.target_type <> 'merge_request' OR a.access_level >= 20)
             AND (i.id IS NULL OR issue_visible(i, me.id, me.is_admin))
             AND (NOT t.internal OR sees_internal(t.project_id, me.id, me.is_admin))"#,
        user_id
    )
    .fetch_one(e)
    .await
}

/// What marking to-dos done changed: the to-dos done, and the unread notifications
/// about the same things it read (`read_with_todos`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Done {
    pub todos: u64,
    pub read: u64,
}

// What a to-do is for told its person too, in the bell: a mention (`mention`), an
// assignment or a review request (both `assignment`). Handled once (the to-do done),
// it is read there as well, so the two never ask twice; only that kind, and only
// once no other to-do of that kind waits on the same thing. Reading a notification
// does not do its to-do: reading is not handling. (`review` is the author's "approved"
// or "requested changes": no to-do comes with it.)

/// Mark one (`id`) or all of a user's to-dos done, and read what told them of those.
pub async fn mark_done(e: impl PgExecutor<'_>, user_id: i64, id: Option<i64>) -> Result<Done> {
    let r = sqlx::query!(
        r#"WITH done AS (
               UPDATE todos SET state = 'done', updated_at = now()
                WHERE user_id = $1 AND state = 'pending' AND ($2::bigint IS NULL OR id = $2)
            RETURNING id, project_id, target_type, target_iid,
                      CASE action WHEN 'mentioned' THEN 'mention' ELSE 'assignment' END AS kind
           ), seen AS (
               UPDATE notifications n SET read_at = now() FROM done d
                WHERE n.user_id = $1 AND n.read_at IS NULL AND n.kind::text = d.kind
                  AND n.project_id = d.project_id AND n.target_type = d.target_type AND n.target_iid = d.target_iid
                  -- another to-do of that kind still waits there: its notification too
                  AND NOT EXISTS (SELECT 1 FROM todos t
                                  WHERE t.user_id = $1 AND t.state = 'pending' AND t.id NOT IN (SELECT id FROM done)
                                    AND t.project_id = d.project_id AND t.target_type = d.target_type AND t.target_iid = d.target_iid
                                    AND CASE t.action WHEN 'mentioned' THEN 'mention' ELSE 'assignment' END = d.kind)
            RETURNING n.id
           )
           SELECT (SELECT count(*) FROM done) AS "todos!", (SELECT count(*) FROM seen) AS "read!""#,
        user_id,
        id
    )
    .fetch_one(e)
    .await?;
    Ok(Done { todos: r.todos as u64, read: r.read as u64 })
}

/// The user acted on a target (commented, approved, merged...): its to-dos are done,
/// and what told them of it is read.
pub async fn done_for_target(e: impl PgExecutor<'_>, user_id: i64, project_id: i64, target_type: &str, iid: i64) -> Result<Done> {
    let r = sqlx::query!(
        r#"WITH done AS (
               UPDATE todos SET state = 'done', updated_at = now()
                WHERE user_id = $1 AND project_id = $2 AND target_type = $3 AND target_iid = $4 AND state = 'pending'
            RETURNING CASE action WHEN 'mentioned' THEN 'mention' ELSE 'assignment' END AS kind
           ), seen AS (
               -- every to-do there is done: the kinds they came with are read
               UPDATE notifications SET read_at = now()
                WHERE user_id = $1 AND project_id = $2 AND target_type = $3 AND target_iid = $4
                  AND read_at IS NULL AND kind::text IN (SELECT kind FROM done)
            RETURNING id
           )
           SELECT (SELECT count(*) FROM done) AS "todos!", (SELECT count(*) FROM seen) AS "read!""#,
        user_id,
        project_id,
        target_type,
        iid
    )
    .fetch_one(e)
    .await?;
    Ok(Done { todos: r.todos as u64, read: r.read as u64 })
}
