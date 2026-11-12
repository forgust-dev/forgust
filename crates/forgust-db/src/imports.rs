//! Imports from other forges (ADR-0018): the import, its projects, what has been
//! imported already, and the source's users.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Import {
    pub id: i64,
    pub user_id: i64,
    pub source: String,
    pub base_url: String,
    pub target_namespace_id: i64,
    pub target_path: String,
    pub status: String,
    #[schema(value_type = Object)]
    pub options: Value,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// The token is still stored (the import has not ended).
    pub has_token: bool,
}

pub struct NewImport<'a> {
    pub user_id: i64,
    pub source: &'a str,
    pub base_url: &'a str,
    pub token_enc: &'a str,
    pub target_namespace_id: i64,
    pub options: &'a Value,
}

pub async fn insert(e: impl PgExecutor<'_>, i: NewImport<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO imports (user_id, source, base_url, token_enc, target_namespace_id, options)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        i.user_id,
        i.source,
        i.base_url,
        i.token_enc,
        i.target_namespace_id,
        i.options
    )
    .fetch_one(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Import>> {
    sqlx::query_as!(
        Import,
        r#"SELECT i.id, i.user_id, i.source, i.base_url, i.target_namespace_id, r.path AS "target_path!", i.status, i.options,
                  i.created_at, i.finished_at, (i.token_enc IS NOT NULL) AS "has_token!"
           FROM imports i JOIN routes r ON r.source_type = 'namespace' AND r.source_id = i.target_namespace_id
           WHERE i.id = $1"#,
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn list_for_user(e: impl PgExecutor<'_>, user_id: i64, limit: i64) -> Result<Vec<Import>> {
    sqlx::query_as!(
        Import,
        r#"SELECT i.id, i.user_id, i.source, i.base_url, i.target_namespace_id, r.path AS "target_path!", i.status, i.options,
                  i.created_at, i.finished_at, (i.token_enc IS NOT NULL) AS "has_token!"
           FROM imports i JOIN routes r ON r.source_type = 'namespace' AND r.source_id = i.target_namespace_id
           WHERE i.user_id = $1 ORDER BY i.id DESC LIMIT $2"#,
        user_id,
        limit
    )
    .fetch_all(e)
    .await
}

pub async fn token(e: impl PgExecutor<'_>, id: i64) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!("SELECT token_enc FROM imports WHERE id = $1", id).fetch_optional(e).await?.flatten())
}

/// Set the status; ending statuses erase the token and stamp the end.
pub async fn set_status(e: impl PgExecutor<'_>, id: i64, status: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE imports SET status = $2,
                token_enc = CASE WHEN $2 IN ('finished', 'failed', 'canceled') THEN NULL ELSE token_enc END,
                finished_at = CASE WHEN $2 IN ('finished', 'failed', 'canceled') THEN now() ELSE NULL END
         WHERE id = $1",
        id,
        status
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_token(e: impl PgExecutor<'_>, id: i64, token_enc: &str) -> Result<()> {
    sqlx::query!("UPDATE imports SET token_enc = $2 WHERE id = $1", id, token_enc).execute(e).await?;
    Ok(())
}

// ------------------------------------------------------------------ projects

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ImportProject {
    pub id: i64,
    pub import_id: i64,
    pub source_id: String,
    pub source_path: String,
    pub target_path: String,
    pub project_id: Option<i64>,
    pub status: String,
    pub stage: Option<String>,
    #[schema(value_type = Object)]
    pub progress: Value,
    #[schema(value_type = Object)]
    pub report: Value,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub async fn add_project(e: impl PgExecutor<'_>, import_id: i64, source_id: &str, source_path: &str, target_path: &str) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO import_projects (import_id, source_id, source_path, target_path) VALUES ($1, $2, $3, $4) RETURNING id",
        import_id,
        source_id,
        source_path,
        target_path
    )
    .fetch_one(e)
    .await
}

pub async fn projects(e: impl PgExecutor<'_>, import_id: i64) -> Result<Vec<ImportProject>> {
    sqlx::query_as!(
        ImportProject,
        "SELECT id, import_id, source_id, source_path, target_path, project_id, status, stage, progress, report, error, started_at, finished_at
         FROM import_projects WHERE import_id = $1 ORDER BY source_path",
        import_id
    )
    .fetch_all(e)
    .await
}

pub async fn project(e: impl PgExecutor<'_>, id: i64) -> Result<Option<ImportProject>> {
    sqlx::query_as!(
        ImportProject,
        "SELECT id, import_id, source_id, source_path, target_path, project_id, status, stage, progress, report, error, started_at, finished_at
         FROM import_projects WHERE id = $1",
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn set_project_status(e: impl PgExecutor<'_>, id: i64, status: &str, error: Option<&str>) -> Result<()> {
    sqlx::query!(
        "UPDATE import_projects SET status = $2, error = $3,
                started_at = CASE WHEN $2 IN ('planning', 'running') THEN coalesce(started_at, now()) ELSE started_at END,
                finished_at = CASE WHEN $2 IN ('planned', 'finished', 'failed', 'skipped', 'canceled') THEN now() ELSE NULL END,
                stage = CASE WHEN $2 IN ('planned', 'finished', 'failed', 'skipped', 'canceled') THEN NULL ELSE stage END
         WHERE id = $1",
        id,
        status,
        error
    )
    .execute(e)
    .await?;
    Ok(())
}

/// The line shown under a project in flight (why it waits); leaves a project
/// that has ended (or was canceled) alone.
pub async fn set_project_note(e: impl PgExecutor<'_>, id: i64, note: Option<&str>) -> Result<()> {
    sqlx::query!("UPDATE import_projects SET error = $2 WHERE id = $1 AND status IN ('planning', 'running')", id, note).execute(e).await?;
    Ok(())
}

pub async fn set_stage(e: impl PgExecutor<'_>, id: i64, stage: &str) -> Result<()> {
    sqlx::query!("UPDATE import_projects SET stage = $2 WHERE id = $1", id, stage).execute(e).await?;
    Ok(())
}

/// Record `done` of `total` for one kind (issues, merge_requests, ...).
pub async fn set_progress(e: impl PgExecutor<'_>, id: i64, kind: &str, done: i64, total: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE import_projects SET progress = jsonb_set(progress, ARRAY[$2], jsonb_build_object('done', $3::bigint, 'total', $4::bigint))
         WHERE id = $1",
        id,
        kind,
        done,
        total
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_report(e: impl PgExecutor<'_>, id: i64, report: &Value) -> Result<()> {
    sqlx::query!("UPDATE import_projects SET report = $2 WHERE id = $1", id, report).execute(e).await?;
    Ok(())
}

pub async fn set_target_path(e: impl PgExecutor<'_>, id: i64, target_path: &str) -> Result<()> {
    sqlx::query!("UPDATE import_projects SET target_path = $2 WHERE id = $1", id, target_path).execute(e).await?;
    Ok(())
}

pub async fn set_local_project(e: impl PgExecutor<'_>, id: i64, project_id: i64) -> Result<()> {
    sqlx::query!("UPDATE import_projects SET project_id = $2 WHERE id = $1", id, project_id).execute(e).await?;
    Ok(())
}

/// How many projects of the import are in each status.
pub async fn status_counts(e: impl PgExecutor<'_>, import_id: i64) -> Result<Vec<(String, i64)>> {
    let rows = sqlx::query!(r#"SELECT status, count(*) AS "n!" FROM import_projects WHERE import_id = $1 GROUP BY status"#, import_id)
        .fetch_all(e)
        .await?;
    Ok(rows.into_iter().map(|r| (r.status, r.n)).collect())
}

// ------------------------------------------------------------------ imported objects

pub async fn imported(e: impl PgExecutor<'_>, project_id: i64, kind: &str, external_id: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        "SELECT local_id FROM imported_objects WHERE project_id = $1 AND kind = $2 AND external_id = $3",
        project_id,
        kind,
        external_id
    )
    .fetch_optional(e)
    .await
}

pub async fn record(e: impl PgExecutor<'_>, project_id: i64, kind: &str, external_id: &str, local_id: i64) -> Result<()> {
    sqlx::query!(
        "INSERT INTO imported_objects (project_id, kind, external_id, local_id) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        project_id,
        kind,
        external_id,
        local_id
    )
    .execute(e)
    .await?;
    Ok(())
}

// ------------------------------------------------------------------ users

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ImportUser {
    pub source_user_id: String,
    pub username: String,
    pub name: String,
    pub email: Option<String>,
    pub user_id: Option<i64>,
    pub local_username: Option<String>,
}

pub async fn user(e: impl PgExecutor<'_>, import_id: i64, source_user_id: &str) -> Result<Option<ImportUser>> {
    sqlx::query_as!(
        ImportUser,
        r#"SELECT iu.source_user_id, iu.username, iu.name, iu.email, iu.user_id, u.username AS "local_username?"
           FROM import_users iu LEFT JOIN users u ON u.id = iu.user_id
           WHERE iu.import_id = $1 AND iu.source_user_id = $2"#,
        import_id,
        source_user_id
    )
    .fetch_optional(e)
    .await
}

pub async fn users(e: impl PgExecutor<'_>, import_id: i64) -> Result<Vec<ImportUser>> {
    sqlx::query_as!(
        ImportUser,
        r#"SELECT iu.source_user_id, iu.username, iu.name, iu.email, iu.user_id, u.username AS "local_username?"
           FROM import_users iu LEFT JOIN users u ON u.id = iu.user_id
           WHERE iu.import_id = $1 ORDER BY iu.username"#,
        import_id
    )
    .fetch_all(e)
    .await
}

/// Remember a source user and match them to an account here by email
/// (case-insensitive; only confirmed, active accounts).
pub async fn upsert_user(
    e: impl PgExecutor<'_>,
    import_id: i64,
    source_user_id: &str,
    username: &str,
    name: &str,
    email: Option<&str>,
) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        r#"INSERT INTO import_users (import_id, source_user_id, username, name, email, user_id)
           VALUES ($1, $2, $3, $4, $5,
                   (SELECT id FROM users WHERE $5::text IS NOT NULL AND lower(email) = lower($5) AND state = 'active' LIMIT 1))
           ON CONFLICT (import_id, source_user_id) DO UPDATE
               SET username = EXCLUDED.username, name = EXCLUDED.name,
                   email = coalesce(EXCLUDED.email, import_users.email),
                   user_id = coalesce(EXCLUDED.user_id, import_users.user_id)
           RETURNING user_id"#,
        import_id,
        source_user_id,
        username,
        name,
        email
    )
    .fetch_one(e)
    .await
}

/// External ids of what is already imported, for one kind.
pub async fn imported_ids(e: impl PgExecutor<'_>, project_id: i64, kind: &str) -> Result<std::collections::HashSet<String>> {
    let rows = sqlx::query_scalar!("SELECT external_id FROM imported_objects WHERE project_id = $1 AND kind = $2", project_id, kind)
        .fetch_all(e)
        .await?;
    Ok(rows.into_iter().collect())
}
