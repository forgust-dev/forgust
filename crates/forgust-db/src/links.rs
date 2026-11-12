//! Sharing projects and groups with other groups (ADR-0006).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ShareLink {
    pub id: i64,
    pub group_id: i64,
    pub group_full_path: String,
    pub max_access_level: i16,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn share_project(
    e: impl PgExecutor<'_>,
    project_id: i64,
    group_id: i64,
    max: i16,
    expires_at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO project_group_links (project_id, group_id, max_access_level, expires_at) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id, group_id) DO UPDATE SET max_access_level = EXCLUDED.max_access_level, expires_at = EXCLUDED.expires_at",
        project_id,
        group_id,
        max,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn unshare_project(e: impl PgExecutor<'_>, project_id: i64, group_id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM project_group_links WHERE project_id = $1 AND group_id = $2", project_id, group_id)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

pub async fn project_shares(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<ShareLink>> {
    sqlx::query_as!(
        ShareLink,
        "SELECT l.id, l.group_id, r.path AS group_full_path, l.max_access_level, l.expires_at
         FROM project_group_links l JOIN routes r ON r.source_type = 'namespace' AND r.source_id = l.group_id
         WHERE l.project_id = $1 ORDER BY r.path",
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn share_group(
    e: impl PgExecutor<'_>,
    shared_group: i64,
    with_group: i64,
    max: i16,
    expires_at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO group_group_links (shared_group_id, shared_with_group_id, max_access_level, expires_at) VALUES ($1, $2, $3, $4)
         ON CONFLICT (shared_group_id, shared_with_group_id) DO UPDATE SET max_access_level = EXCLUDED.max_access_level, expires_at = EXCLUDED.expires_at",
        shared_group,
        with_group,
        max,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn unshare_group(e: impl PgExecutor<'_>, shared_group: i64, with_group: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM group_group_links WHERE shared_group_id = $1 AND shared_with_group_id = $2", shared_group, with_group)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

pub async fn group_shares(e: impl PgExecutor<'_>, shared_group: i64) -> Result<Vec<ShareLink>> {
    sqlx::query_as!(
        ShareLink,
        "SELECT l.id, l.shared_with_group_id AS group_id, r.path AS group_full_path, l.max_access_level, l.expires_at
         FROM group_group_links l JOIN routes r ON r.source_type = 'namespace' AND r.source_id = l.shared_with_group_id
         WHERE l.shared_group_id = $1 ORDER BY r.path",
        shared_group
    )
    .fetch_all(e)
    .await
}
