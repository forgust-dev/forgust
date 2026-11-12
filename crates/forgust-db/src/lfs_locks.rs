//! Git LFS file locks: one lock per path and project.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LfsLock {
    pub id: i64,
    pub project_id: i64,
    pub path: String,
    pub owner_id: i64,
    pub owner_username: String,
    pub owner_name: String,
    pub created_at: DateTime<Utc>,
}

/// Lock `path` for `owner_id`; None when someone already holds it.
pub async fn create(e: impl PgExecutor<'_>, project_id: i64, path: &str, owner_id: i64, ref_name: Option<&str>) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        "INSERT INTO lfs_locks (project_id, path, owner_id, ref_name) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id, path) DO NOTHING RETURNING id",
        project_id,
        path,
        owner_id,
        ref_name
    )
    .fetch_optional(e)
    .await
}

/// Locks of a project, oldest first, with an id above `after`, at most
/// `limit`; only `path` or `id` when given.
pub async fn list(
    e: impl PgExecutor<'_>,
    project_id: i64,
    path: Option<&str>,
    id: Option<i64>,
    after: i64,
    limit: i64,
) -> Result<Vec<LfsLock>> {
    sqlx::query_as!(
        LfsLock,
        "SELECT l.id, l.project_id, l.path, l.owner_id, u.username AS owner_username, u.name AS owner_name, l.created_at
           FROM lfs_locks l JOIN users u ON u.id = l.owner_id
          WHERE l.project_id = $1 AND ($2::text IS NULL OR l.path = $2) AND ($3::bigint IS NULL OR l.id = $3) AND l.id > $4
          ORDER BY l.id LIMIT $5",
        project_id,
        path,
        id,
        after,
        limit
    )
    .fetch_all(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<Option<LfsLock>> {
    Ok(list(e, project_id, None, Some(id), 0, 1).await?.into_iter().next())
}

pub async fn delete(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM lfs_locks WHERE id = $1 AND project_id = $2", id, project_id).execute(e).await?.rows_affected() > 0)
}

/// The locks among `paths` held by someone other than `user_id`: (path, owner's username).
pub async fn held_by_others(e: impl PgExecutor<'_>, project_id: i64, paths: &[String], user_id: i64) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query!(
        "SELECT l.path, u.username FROM lfs_locks l JOIN users u ON u.id = l.owner_id
          WHERE l.project_id = $1 AND l.path = ANY($2) AND l.owner_id <> $3 ORDER BY l.path",
        project_id,
        paths,
        user_id
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.path, r.username)).collect())
}
