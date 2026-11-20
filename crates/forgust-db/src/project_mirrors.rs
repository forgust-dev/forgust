//! Mirrors of a project's repository: pulled from another forge (the
//! repository is then read-only here), or pushed to one after each change.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Mirror {
    pub id: i64,
    pub project_id: i64,
    /// `pull` or `push`.
    pub direction: String,
    pub url: String,
    pub username: String,
    #[serde(skip)]
    pub token_enc: Option<String>,
    pub enabled: bool,
    /// Push: only protected branches, nothing deleted on the remote.
    pub only_protected: bool,
    /// Pull: run pipelines for what arrives.
    pub trigger_pipelines: bool,
    /// Pull: minutes between updates.
    pub interval_minutes: i32,
    pub created_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub last_update_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

impl Mirror {
    pub fn has_token(&self) -> bool {
        self.token_enc.is_some()
    }
}

pub struct NewMirror<'a> {
    pub project_id: i64,
    pub direction: &'a str,
    pub url: &'a str,
    pub username: &'a str,
    pub token_enc: Option<&'a str>,
    pub only_protected: bool,
    pub trigger_pipelines: bool,
    pub interval_minutes: i32,
    pub created_by: Option<i64>,
}

pub async fn insert(e: impl PgExecutor<'_>, m: NewMirror<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO project_mirrors (project_id, direction, url, username, token_enc, only_protected, trigger_pipelines, interval_minutes, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        m.project_id,
        m.direction,
        m.url,
        m.username,
        m.token_enc,
        m.only_protected,
        m.trigger_pipelines,
        m.interval_minutes,
        m.created_by
    )
    .fetch_one(e)
    .await
}

pub async fn list(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<Mirror>> {
    sqlx::query_as!(
        Mirror,
        "SELECT id, project_id, direction, url, username, token_enc, enabled, only_protected, trigger_pipelines, interval_minutes,
                created_by, created_at, last_update_at, last_success_at, last_error
         FROM project_mirrors WHERE project_id = $1 ORDER BY direction, id",
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Mirror>> {
    sqlx::query_as!(
        Mirror,
        "SELECT id, project_id, direction, url, username, token_enc, enabled, only_protected, trigger_pipelines, interval_minutes,
                created_by, created_at, last_update_at, last_success_at, last_error
         FROM project_mirrors WHERE id = $1",
        id
    )
    .fetch_optional(e)
    .await
}

/// The address a project's repository is pulled from, when it is a pull mirror.
pub async fn pulled_from(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar!("SELECT url FROM project_mirrors WHERE project_id = $1 AND direction = 'pull'", project_id).fetch_optional(e).await
}

/// Enabled push mirrors of a project.
pub async fn push_targets(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!("SELECT id FROM project_mirrors WHERE project_id = $1 AND direction = 'push' AND enabled ORDER BY id", project_id)
        .fetch_all(e)
        .await
}

/// Enabled pull mirrors whose interval has passed since their last attempt
/// (never-tried ones first).
pub async fn due_pulls(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM project_mirrors
         WHERE direction = 'pull' AND enabled
           AND (last_update_at IS NULL OR last_update_at < now() - make_interval(mins => interval_minutes))
         ORDER BY last_update_at NULLS FIRST LIMIT $1",
        limit
    )
    .fetch_all(e)
    .await
}

pub struct MirrorChange {
    pub enabled: bool,
    pub only_protected: bool,
    pub trigger_pipelines: bool,
    pub interval_minutes: i32,
}

pub async fn update(e: impl PgExecutor<'_>, id: i64, c: &MirrorChange) -> Result<()> {
    sqlx::query!(
        "UPDATE project_mirrors SET enabled = $2, only_protected = $3, trigger_pipelines = $4, interval_minutes = $5 WHERE id = $1",
        id,
        c.enabled,
        c.only_protected,
        c.trigger_pipelines,
        c.interval_minutes
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Replace the credentials (the address stays).
pub async fn set_credentials(e: impl PgExecutor<'_>, id: i64, username: &str, token_enc: Option<&str>) -> Result<()> {
    sqlx::query!("UPDATE project_mirrors SET username = $2, token_enc = $3 WHERE id = $1", id, username, token_enc).execute(e).await?;
    Ok(())
}

/// An attempt is starting: the sweep does not pick it again meanwhile.
pub async fn mark_attempt(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE project_mirrors SET last_update_at = now() WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// How the last attempt went: `None` when it worked.
pub async fn set_result(e: impl PgExecutor<'_>, id: i64, error: Option<&str>) -> Result<()> {
    sqlx::query!(
        "UPDATE project_mirrors SET last_update_at = now(), last_error = $2,
                last_success_at = CASE WHEN $2::text IS NULL THEN now() ELSE last_success_at END
         WHERE id = $1",
        id,
        error
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete(e: impl PgExecutor<'_>, id: i64, project_id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM project_mirrors WHERE id = $1 AND project_id = $2", id, project_id).execute(e).await?.rows_affected() > 0)
}
