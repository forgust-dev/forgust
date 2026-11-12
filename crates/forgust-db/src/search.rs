//! Searching the instance (M7): projects, issues and merge requests by title.
//!
//! What a viewer may see is decided here, in SQL, before any limit, with the
//! rule `can_project` applies: admins see everything; anyone sees public
//! projects, signed-in people internal ones, members their own. Issues need the
//! project to be seen; merge requests its code (members of a private project:
//! Reporter and up). Counts follow the same rule, so they never tell about
//! hidden things.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

/// Who is searching.
#[derive(Debug, Clone, Copy)]
pub struct Viewer {
    pub user_id: Option<i64>,
    pub admin: bool,
    /// Sees internal projects: a signed-in person, not an external user or a bot.
    pub internal: bool,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProjectHit {
    pub id: i64,
    pub full_path: String,
    pub name: String,
    pub description: String,
    pub visibility: i16,
    pub last_activity_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ItemHit {
    pub project_id: i64,
    pub project_path: String,
    pub iid: i64,
    pub title: String,
    /// opened, closed (or merged)
    pub state: String,
    pub author_username: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// `pattern` is already LIKE-escaped (`like_escape`).
pub async fn projects(e: impl PgExecutor<'_>, v: Viewer, pattern: &str, limit: i64, offset: i64) -> Result<Vec<ProjectHit>> {
    sqlx::query_as!(
        ProjectHit,
        r#"SELECT p.id, r.path AS "full_path!", p.name, p.description, p.visibility, p.last_activity_at
             FROM projects p JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
            WHERE (p.name ILIKE '%' || $3 || '%' ESCAPE '\' OR r.path ILIKE '%' || $3 || '%' ESCAPE '\')
              AND ($2 OR p.visibility = 20 OR (p.visibility = 10 AND $6)
                   OR EXISTS (SELECT 1 FROM listed_project_authorizations a WHERE a.project_id = p.id AND a.user_id = $1))
            ORDER BY p.last_activity_at DESC, p.id DESC LIMIT $4 OFFSET $5"#,
        v.user_id,
        v.admin,
        pattern,
        limit,
        offset,
        v.internal
    )
    .fetch_all(e)
    .await
}

/// `state`: opened, closed or all.
pub async fn issues(e: impl PgExecutor<'_>, v: Viewer, pattern: &str, state: &str, limit: i64, offset: i64) -> Result<Vec<ItemHit>> {
    sqlx::query_as!(
        ItemHit,
        r#"SELECT i.project_id, r.path AS "project_path!", i.iid, i.title, i.state, u.username AS "author_username?", i.updated_at
             FROM issues i JOIN projects p ON p.id = i.project_id
             JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
             LEFT JOIN users u ON u.id = i.author_id
            WHERE i.title ILIKE '%' || $3 || '%' ESCAPE '\' AND ($4 = 'all' OR i.state = $4)
              AND ($2 OR p.visibility = 20 OR (p.visibility = 10 AND $7)
                   OR EXISTS (SELECT 1 FROM listed_project_authorizations a WHERE a.project_id = p.id AND a.user_id = $1))
            ORDER BY i.updated_at DESC, i.id DESC LIMIT $5 OFFSET $6"#,
        v.user_id,
        v.admin,
        pattern,
        state,
        limit,
        offset,
        v.internal
    )
    .fetch_all(e)
    .await
}

/// `state`: opened, closed, merged or all.
pub async fn merge_requests(
    e: impl PgExecutor<'_>,
    v: Viewer,
    pattern: &str,
    state: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<ItemHit>> {
    sqlx::query_as!(
        ItemHit,
        r#"SELECT m.project_id, r.path AS "project_path!", m.iid, m.title, m.state, u.username AS "author_username?", m.updated_at
             FROM merge_requests m JOIN projects p ON p.id = m.project_id
             JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
             LEFT JOIN users u ON u.id = m.author_id
            WHERE m.title ILIKE '%' || $3 || '%' ESCAPE '\' AND ($4 = 'all' OR m.state = $4)
              AND ($2 OR p.visibility = 20 OR (p.visibility = 10 AND $7)
                   OR EXISTS (SELECT 1 FROM listed_project_authorizations a WHERE a.project_id = p.id AND a.user_id = $1 AND a.access_level >= 20))
            ORDER BY m.updated_at DESC, m.id DESC LIMIT $5 OFFSET $6"#,
        v.user_id,
        v.admin,
        pattern,
        state,
        limit,
        offset,
        v.internal
    )
    .fetch_all(e)
    .await
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Counts {
    pub projects: i64,
    pub issues: i64,
    pub merge_requests: i64,
}

/// How many of each the viewer may see (all states).
pub async fn counts(e: impl PgExecutor<'_>, v: Viewer, pattern: &str) -> Result<Counts> {
    let r = sqlx::query!(
        r#"WITH seen AS (
               SELECT p.id, p.name, r.path,
                      ($2 OR p.visibility = 20 OR (p.visibility = 10 AND $4)
                       OR EXISTS (SELECT 1 FROM listed_project_authorizations a WHERE a.project_id = p.id AND a.user_id = $1 AND a.access_level >= 20)) AS code
                 FROM projects p JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
                WHERE $2 OR p.visibility = 20 OR (p.visibility = 10 AND $4)
                   OR EXISTS (SELECT 1 FROM listed_project_authorizations a WHERE a.project_id = p.id AND a.user_id = $1)
           )
           SELECT (SELECT count(*) FROM seen WHERE seen.name ILIKE '%' || $3 || '%' ESCAPE '\' OR seen.path ILIKE '%' || $3 || '%' ESCAPE '\') AS "projects!",
                  (SELECT count(*) FROM issues i JOIN seen ON seen.id = i.project_id WHERE i.title ILIKE '%' || $3 || '%' ESCAPE '\') AS "issues!",
                  (SELECT count(*) FROM merge_requests m JOIN seen ON seen.id = m.project_id AND seen.code
                    WHERE m.title ILIKE '%' || $3 || '%' ESCAPE '\') AS "merge_requests!""#,
        v.user_id,
        v.admin,
        pattern,
        v.internal
    )
    .fetch_one(e)
    .await?;
    Ok(Counts { projects: r.projects, issues: r.issues, merge_requests: r.merge_requests })
}
