//! Code search: what the index holds of each project, and which projects a
//! viewer may search.

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;

use crate::Result;
use crate::search::Viewer;

#[derive(Debug, Clone)]
pub struct IndexState {
    pub generation: String,
    /// `None`: the repository was empty.
    pub indexed_commit: Option<String>,
}

pub async fn state(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<IndexState>> {
    sqlx::query_as!(IndexState, "SELECT generation, indexed_commit FROM code_index_state WHERE project_id = $1", project_id)
        .fetch_optional(e)
        .await
}

/// Record what was indexed. `at` is when the indexing started: a push made
/// while it ran leaves the project due.
pub async fn set_state(
    e: impl PgExecutor<'_>,
    project_id: i64,
    generation: &str,
    commit: Option<&str>,
    branch: Option<&str>,
    at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO code_index_state (project_id, generation, indexed_commit, default_branch, indexed_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (project_id) DO UPDATE SET generation = $2, indexed_commit = $3, default_branch = $4, indexed_at = $5",
        project_id,
        generation,
        commit,
        branch,
        at
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete_state(e: impl PgExecutor<'_>, project_id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM code_index_state WHERE project_id = $1", project_id).execute(e).await?;
    Ok(())
}

/// Forget what was indexed, so every project is due again.
pub async fn clear(e: impl PgExecutor<'_>) -> Result<()> {
    sqlx::query!("DELETE FROM code_index_state").execute(e).await?;
    Ok(())
}

/// Projects to index: never indexed, indexed into another index, active
/// since, or with another default branch. Then the rows of projects that no longer exist, whose files are
/// still in the index.
pub async fn due(e: impl PgExecutor<'_>, generation: &str, limit: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        r#"(SELECT p.id AS "id!" FROM projects p LEFT JOIN code_index_state s ON s.project_id = p.id
             WHERE s.project_id IS NULL OR s.generation <> $1 OR p.last_activity_at > s.indexed_at
                OR s.default_branch IS DISTINCT FROM p.default_branch
             ORDER BY s.indexed_at NULLS FIRST, p.id LIMIT $2)
           UNION ALL
           (SELECT s.project_id AS "id!" FROM code_index_state s
             WHERE NOT EXISTS (SELECT 1 FROM projects p WHERE p.id = s.project_id) LIMIT $2)"#,
        generation,
        limit
    )
    .fetch_all(e)
    .await
}

/// Projects the index of `generation` holds.
pub async fn indexed_projects(e: impl PgExecutor<'_>, generation: &str) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM code_index_state WHERE generation = $1"#, generation).fetch_one(e).await
}

/// A project code was found in.
#[derive(Debug, Clone)]
pub struct SearchedProject {
    pub id: i64,
    pub full_path: String,
    pub default_branch: Option<String>,
    pub indexed_commit: Option<String>,
}

/// The projects whose code the viewer may read, among `only` (one project),
/// under `group_path` (a group and its subgroups), or on the whole instance.
/// The rule is the one for merge requests in `search`: admins everything,
/// anyone public projects, signed-in people internal ones, members from
/// Reporter up. Only projects in the index of `generation` are returned.
pub async fn readable_projects(
    e: impl PgExecutor<'_>,
    v: Viewer,
    generation: &str,
    only: Option<i64>,
    group_path: Option<&str>,
    limit: i64,
) -> Result<Vec<SearchedProject>> {
    sqlx::query_as!(
        SearchedProject,
        r#"SELECT p.id, r.path AS "full_path!", s.default_branch, s.indexed_commit
             FROM projects p JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
             JOIN code_index_state s ON s.project_id = p.id AND s.generation = $4 AND s.indexed_commit IS NOT NULL
            WHERE ($5::bigint IS NULL OR p.id = $5)
              AND ($6::text IS NULL OR lower(r.path) LIKE lower($6) || '/%' ESCAPE '\')
              AND ($2 OR p.visibility = 20 OR (p.visibility = 10 AND $3)
                   OR EXISTS (SELECT 1 FROM listed_project_authorizations a WHERE a.project_id = p.id AND a.user_id = $1 AND a.access_level >= 20))
            ORDER BY p.id LIMIT $7"#,
        v.user_id,
        v.admin,
        v.internal,
        generation,
        only,
        group_path.map(crate::like_escape),
        limit
    )
    .fetch_all(e)
    .await
}
