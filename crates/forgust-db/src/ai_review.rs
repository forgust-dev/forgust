//! AI review of merge requests: the instance's endpoint settings, per-project
//! settings, and one row per review run.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Settings {
    pub project_id: i64,
    pub enabled: bool,
    pub bot_user_id: Option<i64>,
    pub user_id: Option<i64>,
}

pub async fn get(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<Settings>> {
    sqlx::query_as!(
        Settings,
        "SELECT project_id, enabled, bot_user_id, user_id FROM ai_review_settings WHERE project_id = $1",
        project_id
    )
    .fetch_optional(e)
    .await
}

/// Turn it on or off; `bot_user_id` and `user_id` are kept when None.
pub async fn set(e: impl PgExecutor<'_>, project_id: i64, enabled: bool, bot_user_id: Option<i64>, user_id: Option<i64>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO ai_review_settings (project_id, enabled, bot_user_id, user_id) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id) DO UPDATE SET enabled = $2,
             bot_user_id = coalesce($3, ai_review_settings.bot_user_id),
             user_id = coalesce($4, ai_review_settings.user_id), updated_at = now()",
        project_id,
        enabled,
        bot_user_id,
        user_id
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Projects with AI review on.
pub async fn enabled_count(e: impl PgExecutor<'_>) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM ai_review_settings WHERE enabled"#).fetch_one(e).await
}

// ------------------------------------------------------------ instance

/// The administrator's changes over the config file: (fields set, sealed key).
pub async fn instance(e: impl PgExecutor<'_>) -> Result<(serde_json::Value, Option<String>)> {
    Ok(sqlx::query!("SELECT overrides, api_key_enc FROM ai_review_instance WHERE id")
        .fetch_optional(e)
        .await?
        .map(|r| (r.overrides, r.api_key_enc))
        .unwrap_or((serde_json::json!({}), None)))
}

/// Save the overrides; `key`: None keeps the stored key, Some(None) clears it.
pub async fn set_instance(e: impl PgExecutor<'_>, overrides: &serde_json::Value, key: Option<Option<&str>>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO ai_review_instance (id, overrides, api_key_enc) VALUES (true, $1, $3)
         ON CONFLICT (id) DO UPDATE SET overrides = $1,
             api_key_enc = CASE WHEN $2 THEN $3 ELSE ai_review_instance.api_key_enc END, updated_at = now()",
        overrides,
        key.is_some(),
        key.flatten()
    )
    .execute(e)
    .await?;
    Ok(())
}

// ------------------------------------------------------------ runs

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Run {
    pub pipeline_id: i64,
    pub merge_request_id: i64,
    pub base_sha: String,
    pub head_sha: String,
    pub requested_by: Option<i64>,
    /// pending, posted, outdated or failed
    pub state: String,
    pub comments_posted: i32,
    pub comments_dropped: i32,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub async fn insert_run(
    e: impl PgExecutor<'_>,
    pipeline_id: i64,
    merge_request_id: i64,
    base_sha: &str,
    head_sha: &str,
    requested_by: i64,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO ai_review_runs (pipeline_id, merge_request_id, base_sha, head_sha, requested_by) VALUES ($1, $2, $3, $4, $5)",
        pipeline_id,
        merge_request_id,
        base_sha,
        head_sha,
        requested_by
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn run(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Option<Run>> {
    sqlx::query_as!(
        Run,
        "SELECT pipeline_id, merge_request_id, base_sha, head_sha, requested_by, state, comments_posted, comments_dropped,
                error, created_at, finished_at
           FROM ai_review_runs WHERE pipeline_id = $1",
        pipeline_id
    )
    .fetch_optional(e)
    .await
}

/// A merge request's newest run.
pub async fn latest(e: impl PgExecutor<'_>, merge_request_id: i64) -> Result<Option<Run>> {
    sqlx::query_as!(
        Run,
        "SELECT pipeline_id, merge_request_id, base_sha, head_sha, requested_by, state, comments_posted, comments_dropped,
                error, created_at, finished_at
           FROM ai_review_runs WHERE merge_request_id = $1 ORDER BY created_at DESC, pipeline_id DESC LIMIT 1",
        merge_request_id
    )
    .fetch_optional(e)
    .await
}

/// The head of a merge request's newest run whose comments were posted: a
/// review asked again starts from there.
pub async fn last_posted_head(e: impl PgExecutor<'_>, merge_request_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT head_sha FROM ai_review_runs WHERE merge_request_id = $1 AND state = 'posted'
          ORDER BY created_at DESC, pipeline_id DESC LIMIT 1",
        merge_request_id
    )
    .fetch_optional(e)
    .await
}

/// Record how a run ended. Only a pending run changes: a report read twice
/// posts once.
pub async fn finish(e: impl PgExecutor<'_>, pipeline_id: i64, state: &str, posted: i32, dropped: i32, error: Option<&str>) -> Result<bool> {
    Ok(sqlx::query!(
        "UPDATE ai_review_runs SET state = $2, comments_posted = $3, comments_dropped = $4, error = $5, finished_at = now()
          WHERE pipeline_id = $1 AND state = 'pending'",
        pipeline_id,
        state,
        posted,
        dropped,
        error
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}

/// Review pipelines not finished yet: of one merge request, or of the instance.
pub async fn active(e: impl PgExecutor<'_>, merge_request_id: Option<i64>) -> Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM ci_pipelines
            WHERE source = 'ai_review' AND ($1::bigint IS NULL OR merge_request_id = $1) AND status = ANY($2)"#,
        merge_request_id,
        crate::ci::ACTIVE as &[&str]
    )
    .fetch_one(e)
    .await
}

/// A project's review pipelines not finished yet.
pub async fn active_pipelines(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM ci_pipelines WHERE source = 'ai_review' AND project_id = $1 AND status = ANY($2)",
        project_id,
        crate::ci::ACTIVE as &[&str]
    )
    .fetch_all(e)
    .await
}

/// Review pipelines still unfinished after `secs` (no runner took them).
pub async fn stale(e: impl PgExecutor<'_>, secs: f64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM ci_pipelines
          WHERE source = 'ai_review' AND created_at < now() - make_interval(secs => $1) AND status = ANY($2)",
        secs,
        crate::ci::ACTIVE as &[&str]
    )
    .fetch_all(e)
    .await
}

/// Pending runs whose pipeline ended without a report being read (canceled,
/// failed before uploading): what to mark failed.
pub async fn ended_pending(e: impl PgExecutor<'_>) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query!(
        "SELECT r.pipeline_id, p.status FROM ai_review_runs r JOIN ci_pipelines p ON p.id = r.pipeline_id
          WHERE r.state = 'pending' AND p.status <> ALL($1) AND p.finished_at < now() - interval '5 minutes'",
        crate::ci::ACTIVE as &[&str]
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.pipeline_id, r.status))
    .collect())
}
