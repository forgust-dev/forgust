//! Built-in Renovate (ADR-0023): per-project settings and run
//! bookkeeping.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Settings {
    pub project_id: i64,
    pub enabled: bool,
    pub interval_hours: i32,
    pub onboarding: bool,
    pub bot_user_id: Option<i64>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_pipeline_id: Option<i64>,
}

pub async fn get(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<Settings>> {
    sqlx::query_as!(
        Settings,
        "SELECT project_id, enabled, interval_hours, onboarding, bot_user_id, next_run_at, last_pipeline_id
           FROM renovate_settings WHERE project_id = $1",
        project_id
    )
    .fetch_optional(e)
    .await
}

/// Save the project's choice; the next run is set by the caller.
pub async fn save(
    e: impl PgExecutor<'_>,
    project_id: i64,
    enabled: bool,
    interval_hours: i32,
    onboarding: bool,
    bot_user_id: Option<i64>,
    next_run_at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO renovate_settings (project_id, enabled, interval_hours, onboarding, bot_user_id, next_run_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (project_id) DO UPDATE SET enabled = $2, interval_hours = $3, onboarding = $4, bot_user_id = $5,
                                                next_run_at = $6, updated_at = now()",
        project_id,
        enabled,
        interval_hours,
        onboarding,
        bot_user_id,
        next_run_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// A run started: remember its pipeline and when the next one is due.
pub async fn started(e: impl PgExecutor<'_>, project_id: i64, pipeline_id: Option<i64>, next_run_at: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        "UPDATE renovate_settings SET last_pipeline_id = COALESCE($2, last_pipeline_id), next_run_at = $3, updated_at = now()
          WHERE project_id = $1",
        project_id,
        pipeline_id,
        next_run_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Bring the next run forward to `at` at the latest (a push, a ticked box),
/// but not sooner than `min_gap_secs` after the last run started, so a busy
/// project does not hold the instance's run slots.
pub async fn bring_forward(e: impl PgExecutor<'_>, project_id: i64, at: DateTime<Utc>, min_gap_secs: f64) -> Result<bool> {
    Ok(sqlx::query!(
        "UPDATE renovate_settings s
            SET next_run_at = LEAST(COALESCE(s.next_run_at, $2),
                                    GREATEST($2, COALESCE((SELECT p.created_at FROM ci_pipelines p WHERE p.id = s.last_pipeline_id), $2)
                                                 + make_interval(secs => $3)))
          WHERE s.project_id = $1 AND s.enabled",
        project_id,
        at,
        min_gap_secs
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}

/// Projects whose run is due, longest waiting first.
pub async fn due(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT s.project_id FROM renovate_settings s JOIN projects p ON p.id = s.project_id
          WHERE s.enabled AND s.next_run_at <= now() AND p.archived_at IS NULL ORDER BY s.next_run_at LIMIT $1",
        limit
    )
    .fetch_all(e)
    .await
}

/// Renovate pipelines not finished yet: of one project, or of the instance.
pub async fn active_runs(e: impl PgExecutor<'_>, project_id: Option<i64>) -> Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM ci_pipelines
            WHERE source = 'renovate' AND ($1::bigint IS NULL OR project_id = $1) AND status = ANY($2)"#,
        project_id,
        crate::ci::ACTIVE as &[&str]
    )
    .fetch_one(e)
    .await
}

/// Revoke every live token of a user (a Renovate bot's run tokens).
pub async fn revoke_tokens(e: impl PgExecutor<'_>, user_id: i64) -> Result<u64> {
    Ok(sqlx::query!("UPDATE personal_access_tokens SET revoked = true WHERE user_id = $1 AND NOT revoked", user_id)
        .execute(e)
        .await?
        .rows_affected())
}

/// Renovate pipelines still unfinished after `secs` (no runner took them):
/// they would hold the project's and the instance's run slots for good.
pub async fn stale_runs(e: impl PgExecutor<'_>, secs: f64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM ci_pipelines
          WHERE source = 'renovate' AND created_at < now() - make_interval(secs => $1) AND status = ANY($2)",
        secs,
        crate::ci::ACTIVE as &[&str]
    )
    .fetch_all(e)
    .await
}

/// A project's Renovate pipelines not finished yet.
pub async fn active_pipelines(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM ci_pipelines
          WHERE source = 'renovate' AND project_id = $1 AND status = ANY($2)",
        project_id,
        crate::ci::ACTIVE as &[&str]
    )
    .fetch_all(e)
    .await
}

// ------------------------------------------------------------ instance

/// The administrator's changes over the config file: (fields set, sealed github.com token).
pub async fn instance(e: impl PgExecutor<'_>) -> Result<(serde_json::Value, Option<String>)> {
    Ok(sqlx::query!("SELECT overrides, github_com_token_enc FROM renovate_instance WHERE id")
        .fetch_optional(e)
        .await?
        .map(|r| (r.overrides, r.github_com_token_enc))
        .unwrap_or((serde_json::json!({}), None)))
}

/// Save the overrides; `token`: None keeps the stored token, Some(None) clears it.
pub async fn set_instance(e: impl PgExecutor<'_>, overrides: &serde_json::Value, token: Option<Option<&str>>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO renovate_instance (id, overrides, github_com_token_enc) VALUES (true, $1, $3)
         ON CONFLICT (id) DO UPDATE SET overrides = $1,
             github_com_token_enc = CASE WHEN $2 THEN $3 ELSE renovate_instance.github_com_token_enc END, updated_at = now()",
        overrides,
        token.is_some(),
        token.flatten()
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Credentials the runner pulls the Renovate image with: (username, sealed password).
pub async fn image_pull(e: impl PgExecutor<'_>) -> Result<Option<(String, String)>> {
    Ok(sqlx::query!("SELECT image_pull_username, image_pull_password_enc FROM renovate_instance WHERE id")
        .fetch_optional(e)
        .await?
        .and_then(|r| Some((r.image_pull_username, r.image_pull_password_enc?))))
}

/// Set (`Some`) or remove (`None`) the image pull credentials.
pub async fn set_image_pull(e: impl PgExecutor<'_>, creds: Option<(&str, &str)>) -> Result<()> {
    let (user, sealed) = creds.map_or(("", None), |(u, s)| (u, Some(s)));
    sqlx::query!(
        "INSERT INTO renovate_instance (id, image_pull_username, image_pull_password_enc) VALUES (true, $1, $2)
         ON CONFLICT (id) DO UPDATE SET image_pull_username = $1, image_pull_password_enc = $2, updated_at = now()",
        user,
        sealed
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Projects with dependency updates on.
pub async fn enabled_count(e: impl PgExecutor<'_>) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM renovate_settings WHERE enabled"#).fetch_one(e).await
}

// ------------------------------------------------------------ host rules

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct HostRule {
    pub id: i64,
    pub match_host: String,
    pub host_type: Option<String>,
    pub username: String,
    #[serde(skip)]
    pub password_enc: String,
    pub created_at: DateTime<Utc>,
}

/// A project's host rules, or the instance's (`None`: every project's runs).
pub async fn host_rules(e: impl PgExecutor<'_>, project_id: Option<i64>) -> Result<Vec<HostRule>> {
    sqlx::query_as!(
        HostRule,
        "SELECT id, match_host, host_type, username, password_enc, created_at FROM renovate_host_rules
         WHERE project_id IS NOT DISTINCT FROM $1 ORDER BY id",
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn add_host_rule(
    e: impl PgExecutor<'_>,
    project_id: Option<i64>,
    match_host: &str,
    host_type: Option<&str>,
    username: &str,
    password_enc: &str,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO renovate_host_rules (project_id, match_host, host_type, username, password_enc) VALUES ($1, $2, $3, $4, $5) RETURNING id",
        project_id,
        match_host,
        host_type,
        username,
        password_enc
    )
    .fetch_one(e)
    .await
}

pub async fn delete_host_rule(e: impl PgExecutor<'_>, project_id: Option<i64>, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM renovate_host_rules WHERE id = $1 AND project_id IS NOT DISTINCT FROM $2", id, project_id)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

// ------------------------------------------------------------ group defaults

pub async fn group_default(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<bool> {
    Ok(sqlx::query_scalar!("SELECT enabled FROM renovate_group_defaults WHERE namespace_id = $1", namespace_id)
        .fetch_optional(e)
        .await?
        .unwrap_or(false))
}

pub async fn set_group_default(e: impl PgExecutor<'_>, namespace_id: i64, enabled: bool) -> Result<()> {
    sqlx::query!(
        "INSERT INTO renovate_group_defaults (namespace_id, enabled) VALUES ($1, $2)
         ON CONFLICT (namespace_id) DO UPDATE SET enabled = $2, updated_at = now()",
        namespace_id,
        enabled
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Whether a group above `traversal_ids` (the project's namespace path) turned
/// dependency updates on for its projects.
pub async fn inherited_default(e: impl PgExecutor<'_>, traversal_ids: &[i64]) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM renovate_group_defaults WHERE enabled AND namespace_id = ANY ($1)) AS "x!""#,
        traversal_ids
    )
    .fetch_one(e)
    .await
}

/// Projects under a group that never chose (no settings yet).
pub async fn undecided_projects_under(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT p.id FROM projects p JOIN namespaces n ON n.id = p.namespace_id
          WHERE $1 = ANY (n.traversal_ids) AND NOT EXISTS (SELECT 1 FROM renovate_settings s WHERE s.project_id = p.id)
          ORDER BY p.id",
        namespace_id
    )
    .fetch_all(e)
    .await
}
