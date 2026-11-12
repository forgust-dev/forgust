//! Assignees of merge requests and issues, and review requests (M6).

use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

/// A user as lists show them.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct UserBrief {
    pub id: i64,
    pub username: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Reviewer {
    #[serde(flatten)]
    pub user: UserBrief,
    /// unreviewed, reviewed (commented), requested_changes or approved
    pub state: String,
    /// The comment changes were requested with (when `state` is requested_changes).
    pub review_note_id: Option<i64>,
}

/// What a replace changed: users added and users removed.
#[derive(Debug, Default)]
pub struct Change {
    pub added: Vec<i64>,
    pub removed: Vec<i64>,
}

fn change(before: &[i64], after: &[i64]) -> Change {
    Change {
        added: after.iter().filter(|u| !before.contains(u)).copied().collect(),
        removed: before.iter().filter(|u| !after.contains(u)).copied().collect(),
    }
}

pub async fn mr_assignees(e: impl PgExecutor<'_>, mr_ids: &[i64]) -> Result<Vec<(i64, UserBrief)>> {
    let rows = sqlx::query!(
        "SELECT a.merge_request_id, u.id, u.username, u.name FROM merge_request_assignees a JOIN users u ON u.id = a.user_id
         WHERE a.merge_request_id = ANY($1) ORDER BY a.created_at, u.username",
        mr_ids
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.merge_request_id, UserBrief { id: r.id, username: r.username, name: r.name })).collect())
}

pub async fn mr_reviewers(e: impl PgExecutor<'_>, mr_ids: &[i64]) -> Result<Vec<(i64, Reviewer)>> {
    let rows = sqlx::query!(
        "SELECT r.merge_request_id, r.state, r.review_note_id, u.id, u.username, u.name FROM merge_request_reviewers r JOIN users u ON u.id = r.user_id
         WHERE r.merge_request_id = ANY($1) ORDER BY r.created_at, u.username",
        mr_ids
    )
    .fetch_all(e)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.merge_request_id,
                Reviewer {
                    user: UserBrief { id: r.id, username: r.username, name: r.name },
                    review_note_id: if r.state == "requested_changes" { r.review_note_id } else { None },
                    state: r.state,
                },
            )
        })
        .collect())
}

pub async fn issue_assignees(e: impl PgExecutor<'_>, issue_ids: &[i64]) -> Result<Vec<(i64, UserBrief)>> {
    let rows = sqlx::query!(
        "SELECT a.issue_id, u.id, u.username, u.name FROM issue_assignees a JOIN users u ON u.id = a.user_id
         WHERE a.issue_id = ANY($1) ORDER BY a.created_at, u.username",
        issue_ids
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.issue_id, UserBrief { id: r.id, username: r.username, name: r.name })).collect())
}

/// One "who is on it" list replaced (assignees, reviewers): the rows of
/// those no longer there go, the new ones are added, the kept ones stay as they
/// are (a kept reviewer keeps their review state). The SQL is spelled out per
/// table (compile-checked); the body is written once.
macro_rules! set_people {
    ($(#[$doc:meta])* $name:ident, $select:literal, $delete:literal, $insert:literal) => {
        $(#[$doc])*
        pub async fn $name(conn: &mut PgConnection, id: i64, users: &[i64]) -> Result<Change> {
            let before = sqlx::query_scalar!($select, id).fetch_all(&mut *conn).await?;
            let c = change(&before, users);
            sqlx::query!($delete, id, &c.removed).execute(&mut *conn).await?;
            sqlx::query!($insert, id, &c.added).execute(&mut *conn).await?;
            Ok(c)
        }
    };
}

set_people!(
    set_mr_assignees,
    "SELECT user_id FROM merge_request_assignees WHERE merge_request_id = $1 FOR UPDATE",
    "DELETE FROM merge_request_assignees WHERE merge_request_id = $1 AND user_id = ANY($2)",
    "INSERT INTO merge_request_assignees (merge_request_id, user_id) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING"
);

set_people!(
    /// Replace the review requests; kept reviewers keep their state.
    set_mr_reviewers,
    "SELECT user_id FROM merge_request_reviewers WHERE merge_request_id = $1 FOR UPDATE",
    "DELETE FROM merge_request_reviewers WHERE merge_request_id = $1 AND user_id = ANY($2)",
    "INSERT INTO merge_request_reviewers (merge_request_id, user_id) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING"
);

set_people!(
    set_issue_assignees,
    "SELECT user_id FROM issue_assignees WHERE issue_id = $1 FOR UPDATE",
    "DELETE FROM issue_assignees WHERE issue_id = $1 AND user_id = ANY($2)",
    "INSERT INTO issue_assignees (issue_id, user_id) SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING"
);

/// Record what a reviewer did. `only_from` limits the change to rows in that state
/// (a comment marks `unreviewed` as `reviewed` but leaves an approval alone).
/// Returns whether the user is a reviewer whose state changed.
pub async fn set_review_state(e: impl PgExecutor<'_>, mr_id: i64, user_id: i64, state: &str, only_from: Option<&str>) -> Result<bool> {
    let n = sqlx::query!(
        "UPDATE merge_request_reviewers SET state = $3, updated_at = now()
         WHERE merge_request_id = $1 AND user_id = $2 AND state <> $3 AND ($4::text IS NULL OR state = $4)",
        mr_id,
        user_id,
        state,
        only_from
    )
    .execute(e)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Active users with at least `min_level` on the project, matching `search`
/// (username or name), for assignee pickers.
pub async fn project_users(e: impl PgExecutor<'_>, project_id: i64, min_level: i16, search: &str, limit: i64) -> Result<Vec<UserBrief>> {
    sqlx::query_as!(
        UserBrief,
        "SELECT u.id, u.username, u.name FROM project_authorizations a JOIN users u ON u.id = a.user_id
         WHERE a.project_id = $1 AND a.access_level >= $2 AND u.state = 'active' AND u.user_type = 'human'
           AND ($3 = '' OR u.username ILIKE $3 || '%' OR u.name ILIKE '%' || $3 || '%')
         ORDER BY (u.username ILIKE $3 || '%') DESC, u.username LIMIT $4",
        project_id,
        min_level,
        search,
        limit
    )
    .fetch_all(e)
    .await
}
/// A review request in `state`, with the comment it came with (if any).
pub async fn upsert_review(e: impl PgExecutor<'_>, mr_id: i64, user_id: i64, state: &str, note_id: Option<i64>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO merge_request_reviewers (merge_request_id, user_id, state, review_note_id) VALUES ($1, $2, $3, $4)
         ON CONFLICT (merge_request_id, user_id) DO UPDATE SET state = EXCLUDED.state, review_note_id = EXCLUDED.review_note_id,
                                                              updated_at = now()",
        mr_id,
        user_id,
        state,
        note_id
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Active human users by username (case-insensitive), for `@mentions`.
pub async fn by_usernames(e: impl PgExecutor<'_>, names: &[String]) -> Result<Vec<UserBrief>> {
    sqlx::query_as!(
        UserBrief,
        "SELECT id, username, name FROM users WHERE lower(username) = ANY($1) AND state = 'active' AND user_type = 'human'",
        names
    )
    .fetch_all(e)
    .await
}

/// Usernames of active people (and Renovate bots) by verified email, lowercased:
/// commit authors linked to their accounts.
pub async fn usernames_by_email(e: impl PgExecutor<'_>, emails: &[String]) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query!(
        r#"SELECT lower(email) AS "email!", username FROM users
           WHERE lower(email) = ANY($1) AND email_verified_at IS NOT NULL AND state = 'active'
             AND user_type IN ('human', 'renovate_bot')"#,
        emails
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.email, r.username)).collect())
}

/// An open MR in someone's "your work" lists, across projects.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct WorkMr {
    pub project_id: i64,
    pub project_path: String,
    pub project_name: String,
    pub iid: i64,
    pub title: String,
    pub draft: bool,
    pub source_branch: String,
    pub target_branch: String,
    pub author_username: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// The viewer's review state, when they are a reviewer.
    pub review_state: Option<String>,
}

/// Open MRs where `user_id` is a reviewer (`review`), an assignee (`assigned`)
/// or the author (`authored`), in projects they can still see; newest activity first.
pub async fn work_mrs(e: impl PgExecutor<'_>, user_id: i64, kind: &str, limit: i64) -> Result<Vec<WorkMr>> {
    sqlx::query_as!(
        WorkMr,
        r#"SELECT m.project_id, r.path AS project_path, p.name AS project_name, m.iid, m.title, m.draft, m.source_branch,
                  m.target_branch, u.username AS "author_username?", m.updated_at,
                  (SELECT rv.state FROM merge_request_reviewers rv WHERE rv.merge_request_id = m.id AND rv.user_id = $1) AS review_state
           FROM merge_requests m
           JOIN projects p ON p.id = m.project_id
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
           JOIN listed_project_authorizations a ON a.project_id = p.id AND a.user_id = $1 AND a.access_level >= 20
           LEFT JOIN users u ON u.id = m.author_id
           WHERE m.state = 'opened' AND CASE $2
                 WHEN 'review' THEN EXISTS (SELECT 1 FROM merge_request_reviewers rv WHERE rv.merge_request_id = m.id AND rv.user_id = $1)
                 WHEN 'assigned' THEN EXISTS (SELECT 1 FROM merge_request_assignees x WHERE x.merge_request_id = m.id AND x.user_id = $1)
                 ELSE m.author_id = $1 END
           ORDER BY m.updated_at DESC LIMIT $3"#,
        user_id,
        kind,
        limit
    )
    .fetch_all(e)
    .await
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct WorkIssue {
    pub project_id: i64,
    pub project_path: String,
    pub project_name: String,
    pub iid: i64,
    pub title: String,
    pub author_username: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Open issues assigned to `user_id`, in projects they can still see.
pub async fn work_issues(e: impl PgExecutor<'_>, user_id: i64, limit: i64) -> Result<Vec<WorkIssue>> {
    sqlx::query_as!(
        WorkIssue,
        r#"SELECT i.project_id, r.path AS project_path, p.name AS project_name, i.iid, i.title,
                  u.username AS "author_username?", i.updated_at
           FROM issues i
           JOIN issue_assignees x ON x.issue_id = i.id AND x.user_id = $1
           JOIN projects p ON p.id = i.project_id
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
           JOIN listed_project_authorizations a ON a.project_id = p.id AND a.user_id = $1
           LEFT JOIN users u ON u.id = i.author_id
           WHERE i.state = 'opened'
           ORDER BY i.updated_at DESC LIMIT $2"#,
        user_id,
        limit
    )
    .fetch_all(e)
    .await
}
