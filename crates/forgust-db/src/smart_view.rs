//! The Smart View: the instance's settings, per-project settings, and the maps
//! kept: the latest of the default branch and of each open merge request.

use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Settings {
    pub project_id: i64,
    pub enabled: bool,
    pub bot_user_id: Option<i64>,
    pub user_id: Option<i64>,
}

pub async fn get(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<Settings>> {
    sqlx::query_as!(Settings, "SELECT project_id, enabled, bot_user_id, user_id FROM smart_view_settings WHERE project_id = $1", project_id)
        .fetch_optional(e)
        .await
}

/// Turn it on or off; `bot_user_id` and `user_id` are kept when None.
pub async fn set(e: impl PgExecutor<'_>, project_id: i64, enabled: bool, bot_user_id: Option<i64>, user_id: Option<i64>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO smart_view_settings (project_id, enabled, bot_user_id, user_id) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id) DO UPDATE SET enabled = $2,
             bot_user_id = coalesce($3, smart_view_settings.bot_user_id),
             user_id = coalesce($4, smart_view_settings.user_id), updated_at = now()",
        project_id,
        enabled,
        bot_user_id,
        user_id
    )
    .execute(e)
    .await?;
    Ok(())
}

/// The project's bot was made again: its id, nothing else.
pub async fn set_bot(e: impl PgExecutor<'_>, project_id: i64, bot_user_id: i64) -> Result<()> {
    sqlx::query!("UPDATE smart_view_settings SET bot_user_id = $2, updated_at = now() WHERE project_id = $1", project_id, bot_user_id)
        .execute(e)
        .await?;
    Ok(())
}

/// Projects with the Smart View on.
pub async fn enabled_count(e: impl PgExecutor<'_>) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM smart_view_settings WHERE enabled"#).fetch_one(e).await
}

// ------------------------------------------------------------ instance

/// The administrator's changes over the config file: the fields set.
pub async fn instance(e: impl PgExecutor<'_>) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar!("SELECT overrides FROM smart_view_instance WHERE id").fetch_optional(e).await?.unwrap_or(serde_json::json!({})))
}

pub async fn set_instance(e: impl PgExecutor<'_>, overrides: &serde_json::Value) -> Result<()> {
    sqlx::query!(
        "INSERT INTO smart_view_instance (id, overrides) VALUES (true, $1)
         ON CONFLICT (id) DO UPDATE SET overrides = $1, updated_at = now()",
        overrides
    )
    .execute(e)
    .await?;
    Ok(())
}

// ------------------------------------------------------------ maps

/// A commit mapped or being mapped, without its report.
#[derive(Debug, Clone)]
pub struct Map {
    pub id: i64,
    pub git_ref: String,
    pub sha: String,
    pub pipeline_id: Option<i64>,
    /// pending, ready or failed
    pub state: String,
    pub error: Option<String>,
}

/// Take `sha` to map for the default branch (`merge_request_id` None) or a
/// merge request: its id, or None when it is mapped or being mapped already.
/// A try that failed is forgotten: it is tried again.
pub async fn claim(
    tx: &mut sqlx::PgConnection,
    project_id: i64,
    merge_request_id: Option<i64>,
    git_ref: &str,
    sha: &str,
) -> Result<Option<i64>> {
    sqlx::query!(
        "DELETE FROM smart_view_maps
          WHERE project_id = $1 AND coalesce(merge_request_id, 0) = coalesce($2::bigint, 0) AND sha = $3 AND state = 'failed'",
        project_id,
        merge_request_id,
        sha
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query_scalar!(
        "INSERT INTO smart_view_maps (project_id, merge_request_id, git_ref, sha) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id, coalesce(merge_request_id, 0), sha) DO NOTHING RETURNING id",
        project_id,
        merge_request_id,
        git_ref,
        sha
    )
    .fetch_optional(&mut *tx)
    .await
}

/// The pipeline mapping a claimed commit started.
pub async fn started(e: impl PgExecutor<'_>, id: i64, pipeline_id: i64) -> Result<()> {
    sqlx::query!("UPDATE smart_view_maps SET pipeline_id = $2 WHERE id = $1", id, pipeline_id).execute(e).await?;
    Ok(())
}

/// A claimed commit's pipeline did not start: it may be claimed again.
pub async fn unclaim(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM smart_view_maps WHERE id = $1 AND state = 'pending'", id).execute(e).await?;
    Ok(())
}

/// `sha` is the head of the branch or merge request: the runs of its other
/// commits that still go are given up. Their pipelines, to cancel.
pub async fn drop_pending_but(e: impl PgExecutor<'_>, project_id: i64, merge_request_id: Option<i64>, sha: &str) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar!(
        "DELETE FROM smart_view_maps
          WHERE project_id = $1 AND coalesce(merge_request_id, 0) = coalesce($2::bigint, 0) AND state = 'pending' AND sha <> $3
          RETURNING pipeline_id",
        project_id,
        merge_request_id,
        sha
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .flatten()
    .collect())
}

pub async fn of_pipeline(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Option<Map>> {
    sqlx::query_as!(Map, "SELECT id, git_ref, sha, pipeline_id, state, error FROM smart_view_maps WHERE pipeline_id = $1", pipeline_id)
        .fetch_optional(e)
        .await
}

/// The job's report was read: the map is ready, and the older ones of the same
/// branch or merge request go (one newer, still being mapped, stays). Nothing
/// when the commit is no longer wanted (given up, the Smart View turned off).
pub async fn ready(tx: &mut sqlx::PgConnection, id: i64, report: &serde_json::Value) -> Result<()> {
    let row = sqlx::query!(
        "UPDATE smart_view_maps SET state = 'ready', report = $2, error = NULL, finished_at = now()
          WHERE id = $1 RETURNING project_id, merge_request_id",
        id,
        report
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else { return Ok(()) };
    sqlx::query!(
        "DELETE FROM smart_view_maps
          WHERE project_id = $1 AND coalesce(merge_request_id, 0) = coalesce($2::bigint, 0) AND id < $3",
        row.project_id,
        row.merge_request_id,
        id
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// The commit could not be mapped. The last map that is ready stays shown; an
/// older failure goes. Nothing when it was settled already.
pub async fn fail(tx: &mut sqlx::PgConnection, id: i64, error: &str) -> Result<()> {
    let row = sqlx::query!(
        "UPDATE smart_view_maps SET state = 'failed', error = $2, finished_at = now()
          WHERE id = $1 AND state = 'pending' RETURNING project_id, merge_request_id",
        id,
        error
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else { return Ok(()) };
    sqlx::query!(
        "DELETE FROM smart_view_maps
          WHERE project_id = $1 AND coalesce(merge_request_id, 0) = coalesce($2::bigint, 0) AND id < $3 AND state = 'failed'",
        row.project_id,
        row.merge_request_id,
        id
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// The latest map that is ready of the default branch (`merge_request_id`
/// None) or of a merge request.
pub async fn latest_ready(e: impl PgExecutor<'_>, project_id: i64, merge_request_id: Option<i64>) -> Result<Option<Map>> {
    sqlx::query_as!(
        Map,
        "SELECT id, git_ref, sha, pipeline_id, state, error FROM smart_view_maps
          WHERE project_id = $1 AND coalesce(merge_request_id, 0) = coalesce($2::bigint, 0) AND state = 'ready'
          ORDER BY id DESC LIMIT 1",
        project_id,
        merge_request_id
    )
    .fetch_optional(e)
    .await
}

/// A ready map's report, as the job wrote it. None when the map went since.
pub async fn report(e: impl PgExecutor<'_>, id: i64) -> Result<Option<serde_json::Value>> {
    Ok(sqlx::query_scalar!("SELECT report FROM smart_view_maps WHERE id = $1", id).fetch_optional(e).await?.flatten())
}

/// The latest commit taken to map, whatever came of it.
pub async fn latest(e: impl PgExecutor<'_>, project_id: i64, merge_request_id: Option<i64>) -> Result<Option<Map>> {
    sqlx::query_as!(
        Map,
        "SELECT id, git_ref, sha, pipeline_id, state, error FROM smart_view_maps
          WHERE project_id = $1 AND coalesce(merge_request_id, 0) = coalesce($2::bigint, 0)
          ORDER BY id DESC LIMIT 1",
        project_id,
        merge_request_id
    )
    .fetch_optional(e)
    .await
}

/// Forget a project's maps (the Smart View turned off). The pipelines of
/// those still being mapped, to cancel.
pub async fn drop_project(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    Ok(sqlx::query!("DELETE FROM smart_view_maps WHERE project_id = $1 RETURNING pipeline_id, state", project_id)
        .fetch_all(e)
        .await?
        .into_iter()
        .filter(|r| r.state == "pending")
        .filter_map(|r| r.pipeline_id)
        .collect())
}

/// Forget the maps nothing shows any more: of merge requests merged or closed,
/// and of a branch that is no longer the default one. The pipelines of those
/// still being mapped, to cancel.
pub async fn drop_outdated(e: impl PgExecutor<'_>) -> Result<Vec<i64>> {
    Ok(sqlx::query!(
        "DELETE FROM smart_view_maps m
          WHERE EXISTS (SELECT 1 FROM merge_requests mr WHERE mr.id = m.merge_request_id AND mr.state <> 'opened')
             OR (m.merge_request_id IS NULL
                 AND EXISTS (SELECT 1 FROM projects p WHERE p.id = m.project_id AND p.default_branch IS DISTINCT FROM m.git_ref))
          RETURNING m.pipeline_id, m.state"
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .filter(|r| r.state == "pending")
    .filter_map(|r| r.pipeline_id)
    .collect())
}

/// Commits still being mapped after `secs` (no runner took the job): the
/// map's id and its pipeline.
pub async fn stale(e: impl PgExecutor<'_>, secs: f64) -> Result<Vec<(i64, Option<i64>)>> {
    Ok(sqlx::query!(
        "SELECT id, pipeline_id FROM smart_view_maps WHERE state = 'pending' AND created_at < now() - make_interval(secs => $1)",
        secs
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.id, r.pipeline_id))
    .collect())
}

/// Commits being mapped whose pipeline ended without a report being read
/// (canceled, failed before uploading), as (map, pipeline status).
pub async fn ended_pending(e: impl PgExecutor<'_>) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query!(
        "SELECT m.id, p.status FROM smart_view_maps m JOIN ci_pipelines p ON p.id = m.pipeline_id
          WHERE m.state = 'pending' AND p.status <> ALL($1) AND p.finished_at < now() - interval '5 minutes'",
        crate::ci::ACTIVE as &[&str]
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.id, r.status))
    .collect())
}
