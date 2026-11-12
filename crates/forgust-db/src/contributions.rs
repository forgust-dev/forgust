//! Contributions: what a user did, per day and most recent first (M6).
//!
//! Only events in projects the viewer can see count: public ones, internal ones
//! for signed-in viewers (not external users or bots), and the viewer's own
//! projects (merge requests and pushes from Reporter up).

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

pub async fn record_push(e: impl PgExecutor<'_>, user_id: i64, project_id: i64, ref_name: &str, commits: i32) -> Result<()> {
    sqlx::query!(
        "INSERT INTO push_events (user_id, project_id, ref_name, commit_count) VALUES ($1, $2, $3, $4)",
        user_id,
        project_id,
        ref_name,
        commits
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Who is looking: `None` for anonymous visitors.
#[derive(Debug, Clone, Copy)]
pub struct Viewer {
    pub user_id: Option<i64>,
    pub is_admin: bool,
    /// Sees internal projects: signed in, not an external user or a bot.
    pub internal: bool,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Day {
    pub date: NaiveDate,
    pub count: i64,
}

/// Contributions per day since `since`, in the time zone `tz` (a valid IANA name).
pub async fn calendar(e: impl PgExecutor<'_>, user_id: i64, v: Viewer, since: DateTime<Utc>, tz: &str) -> Result<Vec<Day>> {
    sqlx::query_as!(
        Day,
        r#"WITH ev AS (
               SELECT project_id, created_at AS at, true AS code FROM push_events WHERE user_id = $1 AND created_at >= $2
               UNION ALL SELECT project_id, created_at, true FROM merge_requests WHERE author_id = $1 AND created_at >= $2
               UNION ALL SELECT project_id, merged_at, true FROM merge_requests WHERE merged_by = $1 AND merged_at >= $2
               UNION ALL SELECT project_id, created_at, false FROM issues WHERE author_id = $1 AND created_at >= $2
               UNION ALL SELECT project_id, closed_at, false FROM issues WHERE closed_by = $1 AND closed_at >= $2
               UNION ALL SELECT m.project_id, n.created_at, true FROM notes n JOIN merge_requests m ON m.id = n.merge_request_id
                         WHERE n.author_id = $1 AND NOT n.system AND n.created_at >= $2
               UNION ALL SELECT i.project_id, n.created_at, false FROM issue_notes n JOIN issues i ON i.id = n.issue_id
                         WHERE n.author_id = $1 AND NOT n.system AND n.created_at >= $2
               UNION ALL SELECT m.project_id, a.created_at, true FROM approvals a JOIN merge_requests m ON m.id = a.merge_request_id
                         WHERE a.user_id = $1 AND a.created_at >= $2
           )
           SELECT (ev.at AT TIME ZONE $3)::date AS "date!", count(*) AS "count!"
           FROM ev JOIN projects p ON p.id = ev.project_id
           WHERE $5 OR p.visibility = 20 OR (p.visibility = 10 AND $6)
              -- merge requests and pushes are code: a Guest member does not see them
              OR EXISTS (SELECT 1 FROM listed_project_authorizations pa
                         WHERE pa.project_id = p.id AND pa.user_id = $4 AND (NOT ev.code OR pa.access_level >= 20))
           GROUP BY 1 ORDER BY 1"#,
        user_id,
        since,
        tz,
        v.user_id,
        v.is_admin,
        v.internal
    )
    .fetch_all(e)
    .await
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Activity {
    /// pushed, opened_merge_request, merged_merge_request, opened_issue,
    /// closed_issue, commented_merge_request, commented_issue, approved
    pub action: String,
    pub project_path: String,
    /// The MR or issue number (0 for pushes).
    pub iid: i64,
    /// The MR or issue title; the branch for pushes.
    pub title: String,
    /// Commits in a push.
    pub count: i32,
    pub at: DateTime<Utc>,
}

/// The latest contributions, newest first.
pub async fn recent(e: impl PgExecutor<'_>, user_id: i64, v: Viewer, limit: i64) -> Result<Vec<Activity>> {
    sqlx::query_as!(
        Activity,
        r#"WITH ev AS (
               (SELECT 'pushed' AS action, true AS code, project_id, 0::bigint AS iid,
                       regexp_replace(ref_name, '^refs/(heads|tags)/', '') AS title, commit_count AS count, created_at AS at
                FROM push_events WHERE user_id = $1 ORDER BY created_at DESC LIMIT $4)
               UNION ALL (SELECT 'opened_merge_request', true, project_id, iid, title, 0, created_at FROM merge_requests
                          WHERE author_id = $1 ORDER BY created_at DESC LIMIT $4)
               UNION ALL (SELECT 'merged_merge_request', true, project_id, iid, title, 0, merged_at FROM merge_requests
                          WHERE merged_by = $1 AND merged_at IS NOT NULL ORDER BY merged_at DESC LIMIT $4)
               UNION ALL (SELECT 'opened_issue', false, project_id, iid, title, 0, created_at FROM issues
                          WHERE author_id = $1 ORDER BY created_at DESC LIMIT $4)
               UNION ALL (SELECT 'closed_issue', false, project_id, iid, title, 0, closed_at FROM issues
                          WHERE closed_by = $1 AND closed_at IS NOT NULL ORDER BY closed_at DESC LIMIT $4)
               UNION ALL (SELECT 'commented_merge_request', true, m.project_id, m.iid, m.title, 0, n.created_at
                          FROM notes n JOIN merge_requests m ON m.id = n.merge_request_id
                          WHERE n.author_id = $1 AND NOT n.system ORDER BY n.created_at DESC LIMIT $4)
               UNION ALL (SELECT 'commented_issue', false, i.project_id, i.iid, i.title, 0, n.created_at
                          FROM issue_notes n JOIN issues i ON i.id = n.issue_id
                          WHERE n.author_id = $1 AND NOT n.system ORDER BY n.created_at DESC LIMIT $4)
               UNION ALL (SELECT 'approved', true, m.project_id, m.iid, m.title, 0, a.created_at
                          FROM approvals a JOIN merge_requests m ON m.id = a.merge_request_id
                          WHERE a.user_id = $1 ORDER BY a.created_at DESC LIMIT $4)
           )
           SELECT ev.action AS "action!", r.path AS project_path, ev.iid AS "iid!", ev.title AS "title!",
                  ev.count AS "count!", ev.at AS "at!"
           FROM ev JOIN projects p ON p.id = ev.project_id
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
           WHERE $3 OR p.visibility = 20 OR (p.visibility = 10 AND $5)
              OR EXISTS (SELECT 1 FROM listed_project_authorizations pa
                         WHERE pa.project_id = p.id AND pa.user_id = $2 AND (NOT ev.code OR pa.access_level >= 20))
           ORDER BY ev.at DESC LIMIT $4"#,
        user_id,
        v.user_id,
        v.is_admin,
        limit,
        v.internal
    )
    .fetch_all(e)
    .await
}

/// Whether Postgres knows the time zone name.
pub async fn valid_time_zone(e: impl PgExecutor<'_>, tz: &str) -> Result<bool> {
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM pg_timezone_names WHERE name = $1) AS "ok!""#, tz).fetch_one(e).await
}
