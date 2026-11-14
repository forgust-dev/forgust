//! CI: runners, pipelines, jobs, the pending queue and trace chunks (M4,
//! ADR-0014 and ADR-0017).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

/// Live logs are stored in chunks of this size (ADR-0014).
pub const TRACE_CHUNK: i64 = 128 * 1024;

// ---------------------------------------------------------------- settings

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProjectCiSettings {
    pub project_id: i64,
    pub ci_config_path: String,
    pub instance_runners_enabled: bool,
    pub default_timeout_secs: i32,
    pub default_git_depth: i32,
    pub only_merge_if_pipeline_succeeds: bool,
    pub job_token_scope_enabled: bool,
    /// Cancel older pipelines of a branch when a new commit's pipeline starts.
    pub auto_cancel_redundant: bool,
    /// Merge requests merge through a merge train (each tested on the merge
    /// of everything ahead of it).
    pub merge_trains_enabled: bool,
    /// Merge check: no new critical or high security finding left
    /// undismissed.
    pub block_new_security_findings: bool,
}

impl ProjectCiSettings {
    /// What a project gets before anyone saves its CI/CD settings.
    pub fn defaults(project_id: i64) -> Self {
        ProjectCiSettings {
            project_id,
            ci_config_path: ".gitlab-ci.yml".into(),
            instance_runners_enabled: true,
            default_timeout_secs: 3600,
            default_git_depth: 20,
            only_merge_if_pipeline_succeeds: false,
            job_token_scope_enabled: true,
            auto_cancel_redundant: true,
            merge_trains_enabled: false,
            block_new_security_findings: false,
        }
    }
}

/// The project's CI/CD settings (defaults when never saved; reads write nothing).
pub async fn settings(e: impl PgExecutor<'_>, project_id: i64) -> Result<ProjectCiSettings> {
    Ok(sqlx::query_as!(
        ProjectCiSettings,
        "SELECT project_id, ci_config_path, instance_runners_enabled, default_timeout_secs, default_git_depth,
                only_merge_if_pipeline_succeeds, job_token_scope_enabled, auto_cancel_redundant, merge_trains_enabled,
                block_new_security_findings
         FROM ci_project_settings WHERE project_id = $1",
        project_id
    )
    .fetch_optional(e)
    .await?
    .unwrap_or_else(|| ProjectCiSettings::defaults(project_id)))
}

pub async fn update_settings(e: impl PgExecutor<'_>, s: &ProjectCiSettings) -> Result<()> {
    sqlx::query!(
        "INSERT INTO ci_project_settings (project_id, ci_config_path, instance_runners_enabled, default_timeout_secs, default_git_depth,
                                          only_merge_if_pipeline_succeeds, job_token_scope_enabled, auto_cancel_redundant,
                                          merge_trains_enabled, block_new_security_findings)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (project_id) DO UPDATE SET ci_config_path = $2, instance_runners_enabled = $3, default_timeout_secs = $4,
             default_git_depth = $5, only_merge_if_pipeline_succeeds = $6, job_token_scope_enabled = $7, auto_cancel_redundant = $8,
             merge_trains_enabled = $9, block_new_security_findings = $10",
        s.project_id,
        s.ci_config_path,
        s.instance_runners_enabled,
        s.default_timeout_secs,
        s.default_git_depth,
        s.only_merge_if_pipeline_succeeds,
        s.job_token_scope_enabled,
        s.auto_cancel_redundant,
        s.merge_trains_enabled,
        s.block_new_security_findings
    )
    .execute(e)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------- runners

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Runner {
    pub id: i64,
    /// `instance`, `group` or `project`.
    pub kind: String,
    pub group_id: Option<i64>,
    pub project_id: Option<i64>,
    pub description: String,
    /// Last four characters of the token, to tell tokens apart.
    pub token_last4: String,
    pub tags: Vec<String>,
    pub run_untagged: bool,
    pub protected_only: bool,
    pub paused: bool,
    pub maximum_timeout_secs: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub contacted_at: Option<DateTime<Utc>>,
}

pub struct NewRunner<'a> {
    pub kind: &'a str,
    pub group_id: Option<i64>,
    pub project_id: Option<i64>,
    pub description: &'a str,
    pub token_hash: &'a [u8],
    pub token_last4: &'a str,
    pub tags: &'a [String],
    pub run_untagged: bool,
    pub protected_only: bool,
    pub paused: bool,
    pub maximum_timeout_secs: Option<i32>,
    pub created_by: Option<i64>,
}

pub async fn create_runner(e: impl PgExecutor<'_>, r: NewRunner<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO ci_runners (kind, group_id, project_id, description, token_hash, token_last4, tags, run_untagged,
                                 protected_only, maximum_timeout_secs, created_by, paused)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
        r.kind,
        r.group_id,
        r.project_id,
        r.description,
        r.token_hash,
        r.token_last4,
        r.tags,
        r.run_untagged,
        r.protected_only,
        r.maximum_timeout_secs,
        r.created_by,
        r.paused
    )
    .fetch_one(e)
    .await
}

pub async fn runner_by_token(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<Runner>> {
    sqlx::query_as!(
        Runner,
        "SELECT id, kind, group_id, project_id, description, token_last4, tags, run_untagged, protected_only, paused,
                maximum_timeout_secs, created_at, contacted_at
         FROM ci_runners WHERE token_hash = $1",
        hash
    )
    .fetch_optional(e)
    .await
}

pub async fn runner(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Runner>> {
    sqlx::query_as!(
        Runner,
        "SELECT id, kind, group_id, project_id, description, token_last4, tags, run_untagged, protected_only, paused,
                maximum_timeout_secs, created_at, contacted_at
         FROM ci_runners WHERE id = $1",
        id
    )
    .fetch_optional(e)
    .await
}

/// Runners a project can use: its own, those of its ancestor groups, and
/// instance runners when enabled for the project.
pub async fn runners_for_project(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<Runner>> {
    sqlx::query_as!(
        Runner,
        "SELECT r.id, r.kind, r.group_id, r.project_id, r.description, r.token_last4, r.tags, r.run_untagged, r.protected_only,
                r.paused, r.maximum_timeout_secs, r.created_at, r.contacted_at
         FROM ci_runners r, projects p JOIN namespaces n ON n.id = p.namespace_id
         LEFT JOIN ci_project_settings s ON s.project_id = p.id
         WHERE p.id = $1
           AND (r.project_id = p.id
                OR r.group_id = ANY (n.traversal_ids)
                OR (r.kind = 'instance' AND coalesce(s.instance_runners_enabled, true)))
         ORDER BY r.kind = 'project' DESC, r.kind = 'group' DESC, r.id",
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn runners_of_group(e: impl PgExecutor<'_>, group_id: i64) -> Result<Vec<Runner>> {
    sqlx::query_as!(
        Runner,
        "SELECT id, kind, group_id, project_id, description, token_last4, tags, run_untagged, protected_only, paused,
                maximum_timeout_secs, created_at, contacted_at
         FROM ci_runners WHERE group_id = $1 ORDER BY id",
        group_id
    )
    .fetch_all(e)
    .await
}

pub async fn instance_runners(e: impl PgExecutor<'_>) -> Result<Vec<Runner>> {
    sqlx::query_as!(
        Runner,
        "SELECT id, kind, group_id, project_id, description, token_last4, tags, run_untagged, protected_only, paused,
                maximum_timeout_secs, created_at, contacted_at
         FROM ci_runners WHERE kind = 'instance' ORDER BY id"
    )
    .fetch_all(e)
    .await
}

pub struct RunnerUpdate<'a> {
    pub description: &'a str,
    pub tags: &'a [String],
    pub run_untagged: bool,
    pub protected_only: bool,
    pub paused: bool,
    pub maximum_timeout_secs: Option<i32>,
}

pub async fn update_runner(e: impl PgExecutor<'_>, id: i64, u: RunnerUpdate<'_>) -> Result<()> {
    sqlx::query!(
        "UPDATE ci_runners SET description = $2, tags = $3, run_untagged = $4, protected_only = $5, paused = $6,
                               maximum_timeout_secs = $7 WHERE id = $1",
        id,
        u.description,
        u.tags,
        u.run_untagged,
        u.protected_only,
        u.paused,
        u.maximum_timeout_secs
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_runner_token(e: impl PgExecutor<'_>, id: i64, hash: &[u8], last4: &str) -> Result<()> {
    sqlx::query!("UPDATE ci_runners SET token_hash = $2, token_last4 = $3 WHERE id = $1", id, hash, last4).execute(e).await?;
    Ok(())
}

pub async fn delete_runner(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM ci_runners WHERE id = $1", id).execute(e).await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct RunnerManager {
    pub id: i64,
    pub runner_id: i64,
    pub system_id: String,
    pub version: String,
    pub revision: String,
    pub platform: String,
    pub architecture: String,
    pub executor: String,
    pub ip_address: String,
    pub created_at: DateTime<Utc>,
    pub contacted_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct ManagerInfo<'a> {
    pub version: &'a str,
    pub revision: &'a str,
    pub platform: &'a str,
    pub architecture: &'a str,
    pub executor: &'a str,
    pub ip_address: &'a str,
}

/// Create or refresh the runner manager for `(runner, system_id)` and mark the
/// runner as contacted. Empty info fields keep the stored values.
pub async fn touch_manager(e: impl PgExecutor<'_>, runner_id: i64, system_id: &str, i: ManagerInfo<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "WITH r AS (UPDATE ci_runners SET contacted_at = now() WHERE id = $1)
         INSERT INTO ci_runner_managers (runner_id, system_id, version, revision, platform, architecture, executor, ip_address)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (runner_id, system_id) DO UPDATE SET contacted_at = now(),
             version = CASE WHEN $3 = '' THEN ci_runner_managers.version ELSE $3 END,
             revision = CASE WHEN $4 = '' THEN ci_runner_managers.revision ELSE $4 END,
             platform = CASE WHEN $5 = '' THEN ci_runner_managers.platform ELSE $5 END,
             architecture = CASE WHEN $6 = '' THEN ci_runner_managers.architecture ELSE $6 END,
             executor = CASE WHEN $7 = '' THEN ci_runner_managers.executor ELSE $7 END,
             ip_address = CASE WHEN $8 = '' THEN ci_runner_managers.ip_address ELSE $8 END
         RETURNING id",
        runner_id,
        system_id,
        i.version,
        i.revision,
        i.platform,
        i.architecture,
        i.executor,
        i.ip_address
    )
    .fetch_one(e)
    .await
}

pub async fn managers(e: impl PgExecutor<'_>, runner_id: i64) -> Result<Vec<RunnerManager>> {
    sqlx::query_as!(
        RunnerManager,
        "SELECT id, runner_id, system_id, version, revision, platform, architecture, executor, ip_address, created_at, contacted_at
         FROM ci_runner_managers WHERE runner_id = $1 ORDER BY contacted_at DESC",
        runner_id
    )
    .fetch_all(e)
    .await
}

/// The managers of several runners, in one query (runner lists).
pub async fn managers_of(e: impl PgExecutor<'_>, runner_ids: &[i64]) -> Result<Vec<RunnerManager>> {
    sqlx::query_as!(
        RunnerManager,
        "SELECT id, runner_id, system_id, version, revision, platform, architecture, executor, ip_address, created_at, contacted_at
         FROM ci_runner_managers WHERE runner_id = ANY($1) ORDER BY contacted_at DESC",
        runner_ids
    )
    .fetch_all(e)
    .await
}

pub async fn delete_manager(e: impl PgExecutor<'_>, runner_id: i64, system_id: &str) -> Result<bool> {
    let n = sqlx::query!("DELETE FROM ci_runner_managers WHERE runner_id = $1 AND system_id = $2", runner_id, system_id)
        .execute(e)
        .await?
        .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------- pipelines

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Pipeline {
    pub id: i64,
    pub project_id: i64,
    pub iid: i64,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub tag: bool,
    pub sha: String,
    pub before_sha: String,
    /// `push`, `web`, `api`, `schedule`, `merge_request_event`.
    pub source: String,
    pub status: String,
    pub protected: bool,
    pub merge_request_id: Option<i64>,
    pub schedule_id: Option<i64>,
    pub user_id: Option<i64>,
    pub username: Option<String>,
    /// Subject of the commit, stored at creation.
    pub commit_title: Option<String>,
    pub yaml_errors: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

pub struct NewPipeline<'a> {
    pub project_id: i64,
    pub iid: i64,
    pub git_ref: &'a str,
    pub tag: bool,
    pub sha: &'a str,
    pub before_sha: &'a str,
    pub source: &'a str,
    pub protected: bool,
    pub merge_request_id: Option<i64>,
    pub schedule_id: Option<i64>,
    pub user_id: Option<i64>,
    pub yaml_errors: Option<&'a str>,
    pub variables_enc: Option<&'a str>,
    pub commit_title: Option<&'a str>,
    /// `workflow:auto_cancel:on_new_commit`.
    pub auto_cancel: &'a str,
    /// `workflow:auto_cancel:on_job_failure`.
    pub auto_cancel_on_job_failure: &'a str,
    /// The `trigger` job that started this (downstream) pipeline.
    pub upstream_build_id: Option<i64>,
}

/// Older unfinished pipelines a new one makes redundant: same branch, same kind
/// (pushes, or the same merge request's), another commit. With their
/// `auto_cancel` mode.
pub async fn redundant_pipelines(e: impl PgExecutor<'_>, newer: &Pipeline) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query!(
        "SELECT id, auto_cancel FROM ci_pipelines
          WHERE project_id = $1 AND ref = $2 AND NOT tag AND id < $3 AND sha <> $4 AND source = $5
            AND merge_request_id IS NOT DISTINCT FROM $6
            AND status IN ('created', 'waiting_for_resource', 'preparing', 'pending', 'running')
          ORDER BY id",
        newer.project_id,
        newer.git_ref,
        newer.id,
        newer.sha,
        newer.source,
        newer.merge_request_id
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.id, r.auto_cancel))
    .collect())
}

pub async fn insert_pipeline(e: impl PgExecutor<'_>, p: NewPipeline<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO ci_pipelines (project_id, iid, ref, tag, sha, before_sha, source, protected, merge_request_id, schedule_id,
                                   user_id, yaml_errors, variables_enc, commit_title, status, auto_cancel, auto_cancel_on_job_failure,
                                   upstream_build_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                 CASE WHEN $12::text IS NULL THEN 'created' ELSE 'failed' END, $15, $16, $17)
         RETURNING id",
        p.project_id,
        p.iid,
        p.git_ref,
        p.tag,
        p.sha,
        p.before_sha,
        p.source,
        p.protected,
        p.merge_request_id,
        p.schedule_id,
        p.user_id,
        p.yaml_errors,
        p.variables_enc,
        p.commit_title,
        p.auto_cancel,
        p.auto_cancel_on_job_failure,
        p.upstream_build_id
    )
    .fetch_one(e)
    .await
}

/// How the pipeline before this one ended (`success` or `failed`; canceled,
/// skipped and child pipelines are not counted, nor the instance's own:
/// dependency updates and security scans): the one before on the same ref
/// and for the same merge request (a branch's pipelines and its merge
/// request's are two series).
pub async fn previous_status(e: impl PgExecutor<'_>, p: &Pipeline) -> Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT status FROM ci_pipelines
          WHERE project_id = $1 AND ref = $2 AND tag = $3 AND merge_request_id IS NOT DISTINCT FROM $4 AND id < $5
            AND status IN ('success', 'failed') AND source NOT IN ('parent_pipeline', 'renovate', 'security')
          ORDER BY id DESC LIMIT 1",
        p.project_id,
        p.git_ref,
        p.tag,
        p.merge_request_id,
        p.id
    )
    .fetch_optional(e)
    .await
}

/// Record how the pipeline ended (`success` or `failed`) for its notifications,
/// and return what was recorded before (none: it ends for the first time).
pub async fn set_notified_status(e: impl PgExecutor<'_>, pipeline_id: i64, status: &str) -> Result<Option<String>> {
    sqlx::query_scalar!(
        "UPDATE ci_pipelines p SET notified_status = $2
           FROM (SELECT id, notified_status FROM ci_pipelines WHERE id = $1 FOR UPDATE) before
          WHERE p.id = before.id
      RETURNING before.notified_status",
        pipeline_id,
        status
    )
    .fetch_one(e)
    .await
}

pub async fn pipeline(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Pipeline>> {
    sqlx::query_as!(
        Pipeline,
        "SELECT p.id, p.project_id, p.iid, p.ref AS git_ref, p.tag, p.sha, p.before_sha, p.source, p.status, p.protected,
                    p.merge_request_id, p.schedule_id, p.user_id, u.username AS \"username?\", p.commit_title, p.yaml_errors, p.created_at,
                    p.started_at, p.finished_at, p.updated_at
             FROM ci_pipelines p LEFT JOIN users u ON u.id = p.user_id WHERE p.id = $1",
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn pipeline_by_iid(e: impl PgExecutor<'_>, project_id: i64, iid: i64) -> Result<Option<Pipeline>> {
    sqlx::query_as!(
        Pipeline,
        "SELECT p.id, p.project_id, p.iid, p.ref AS git_ref, p.tag, p.sha, p.before_sha, p.source, p.status, p.protected,
                    p.merge_request_id, p.schedule_id, p.user_id, u.username AS \"username?\", p.commit_title, p.yaml_errors, p.created_at,
                    p.started_at, p.finished_at, p.updated_at
             FROM ci_pipelines p LEFT JOIN users u ON u.id = p.user_id WHERE p.project_id = $1 AND p.iid = $2",
        project_id,
        iid
    )
    .fetch_optional(e)
    .await
}

pub async fn pipeline_variables(e: impl PgExecutor<'_>, id: i64) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!("SELECT variables_enc FROM ci_pipelines WHERE id = $1", id).fetch_optional(e).await?.flatten())
}

/// Newest first; filters are optional (`None` or empty matches everything).
/// Statuses of a pipeline that is not finished yet.
pub const ACTIVE: &[&str] = &["created", "waiting_for_resource", "preparing", "pending", "running", "scheduled", "manual"];

pub async fn list_pipelines(
    e: impl PgExecutor<'_>,
    project_id: i64,
    git_ref: Option<&str>,
    status: Option<&str>,
    sha: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<Pipeline>> {
    sqlx::query_as!(
        Pipeline,
        "SELECT p.id, p.project_id, p.iid, p.ref AS git_ref, p.tag, p.sha, p.before_sha, p.source, p.status, p.protected,
                    p.merge_request_id, p.schedule_id, p.user_id, u.username AS \"username?\", p.commit_title, p.yaml_errors, p.created_at,
                    p.started_at, p.finished_at, p.updated_at
             FROM ci_pipelines p LEFT JOIN users u ON u.id = p.user_id WHERE p.project_id = $1 AND ($2::text IS NULL OR p.ref = $2) AND ($3::text IS NULL OR p.status = $3)
               AND ($4::text IS NULL OR p.sha = $4)
               -- built-in Renovate and security runs show in the full list, never as a ref's or commit's pipeline
               AND (($2::text IS NULL AND $4::text IS NULL) OR NOT is_system_source(p.source))
             ORDER BY p.id DESC LIMIT $5 OFFSET $6",
        project_id,
        git_ref,
        status,
        sha,
        limit,
        offset
    )
    .fetch_all(e)
    .await
}

/// The newest pipeline of each of these commits (not Forgust's own: as
/// `list_pipelines` by sha), for lists.
pub async fn newest_pipelines_of(e: impl PgExecutor<'_>, project_id: i64, shas: &[String]) -> Result<Vec<Pipeline>> {
    sqlx::query_as!(
        Pipeline,
        "SELECT DISTINCT ON (p.sha) p.id, p.project_id, p.iid, p.ref AS git_ref, p.tag, p.sha, p.before_sha, p.source, p.status, p.protected,
                p.merge_request_id, p.schedule_id, p.user_id, u.username AS \"username?\", p.commit_title, p.yaml_errors, p.created_at,
                p.started_at, p.finished_at, p.updated_at
         FROM ci_pipelines p LEFT JOIN users u ON u.id = p.user_id
         WHERE p.project_id = $1 AND p.sha = ANY($2) AND NOT is_system_source(p.source)
         ORDER BY p.sha, p.id DESC",
        project_id,
        shas
    )
    .fetch_all(e)
    .await
}

pub async fn pipelines_of_mr(
    e: impl PgExecutor<'_>,
    mr_id: i64,
    source_branch: &str,
    project_id: i64,
    limit: i64,
) -> Result<Vec<Pipeline>> {
    sqlx::query_as!(
        Pipeline,
        "SELECT p.id, p.project_id, p.iid, p.ref AS git_ref, p.tag, p.sha, p.before_sha, p.source, p.status, p.protected,
                    p.merge_request_id, p.schedule_id, p.user_id, u.username AS \"username?\", p.commit_title, p.yaml_errors, p.created_at,
                    p.started_at, p.finished_at, p.updated_at
             FROM ci_pipelines p LEFT JOIN users u ON u.id = p.user_id WHERE p.merge_request_id = $1 OR (p.project_id = $3 AND p.ref = $2 AND NOT p.tag AND p.source <> 'merge_request_event' AND NOT is_system_source(p.source))
             ORDER BY p.id DESC LIMIT $4",
        mr_id,
        source_branch,
        project_id,
        limit
    )
    .fetch_all(e)
    .await
}

/// Latest pipeline status per sha, for commit lists.
pub async fn statuses_for_shas(e: impl PgExecutor<'_>, project_id: i64, shas: &[String]) -> Result<Vec<(String, String, i64)>> {
    let rows = sqlx::query!(
        "SELECT DISTINCT ON (sha) sha, status, iid FROM ci_pipelines WHERE project_id = $1 AND sha = ANY ($2) AND NOT is_system_source(source)
         ORDER BY sha, id DESC",
        project_id,
        shas
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.sha, r.status, r.iid)).collect())
}

/// The newest pipeline status for `sha`: an MR pipeline of `mr_id` or a branch
/// pipeline of that commit.
/// A merge request pipeline already ran (or runs) for this MR at this commit.
pub async fn has_mr_pipeline(e: impl PgExecutor<'_>, mr_id: i64, sha: &str) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM ci_pipelines WHERE merge_request_id = $1 AND sha = $2 AND source = 'merge_request_event') AS "x!""#,
        mr_id,
        sha
    )
    .fetch_one(e)
    .await
}

pub async fn latest_status_for_sha(e: impl PgExecutor<'_>, project_id: i64, sha: &str, mr_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT status FROM ci_pipelines
         WHERE project_id = $1 AND sha = $2 AND (merge_request_id = $3 OR (merge_request_id IS NULL AND NOT tag)) AND NOT is_system_source(source)
         ORDER BY id DESC LIMIT 1",
        project_id,
        sha,
        mr_id
    )
    .fetch_optional(e)
    .await
}

pub async fn set_pipeline_status(e: impl PgExecutor<'_>, id: i64, status: &str) -> Result<bool> {
    let n = sqlx::query!(
        "UPDATE ci_pipelines SET status = $2, updated_at = now(),
             started_at = CASE WHEN $2 = 'running' AND started_at IS NULL THEN now() ELSE started_at END,
             finished_at = CASE WHEN $2 IN ('success', 'failed', 'canceled', 'skipped') THEN now()
                                WHEN $2 IN ('running', 'pending', 'created', 'manual') THEN NULL ELSE finished_at END
         WHERE id = $1 AND status IS DISTINCT FROM $2",
        id,
        status
    )
    .execute(e)
    .await?
    .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------- builds

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Build {
    pub id: i64,
    pub pipeline_id: i64,
    pub project_id: i64,
    pub name: String,
    pub stage: String,
    pub stage_idx: i32,
    pub status: String,
    #[serde(rename = "when")]
    pub when_: String,
    pub allow_failure: bool,
    pub needs: Option<Vec<String>>,
    #[schema(value_type = Object)]
    pub spec: serde_json::Value,
    pub retry_max: i32,
    pub retry_when: Vec<String>,
    pub attempt: i32,
    pub retried: bool,
    pub tags: Vec<String>,
    pub timeout_secs: i32,
    pub user_id: Option<i64>,
    pub runner_id: Option<i64>,
    pub runner_manager_id: Option<i64>,
    pub failure_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub trace_size: i64,
    pub trace_archived: bool,
    pub created_at: DateTime<Utc>,
    pub queued_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub contacted_at: Option<DateTime<Utc>>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
}

pub struct NewBuild<'a> {
    pub pipeline_id: i64,
    pub project_id: i64,
    pub name: &'a str,
    pub stage: &'a str,
    pub stage_idx: i32,
    pub status: &'a str,
    pub when_: &'a str,
    pub allow_failure: bool,
    pub needs: Option<&'a [String]>,
    pub spec: &'a serde_json::Value,
    pub retry_max: i32,
    pub retry_when: &'a [String],
    pub attempt: i32,
    pub tags: &'a [String],
    pub timeout_secs: i32,
    pub user_id: Option<i64>,
    pub resource_group: Option<&'a str>,
}

pub async fn insert_build(e: impl PgExecutor<'_>, b: NewBuild<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO ci_builds (pipeline_id, project_id, name, stage, stage_idx, status, when_, allow_failure, needs, spec,
                                retry_max, retry_when, attempt, tags, timeout_secs, user_id, resource_group)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) RETURNING id",
        b.pipeline_id,
        b.project_id,
        b.name,
        b.stage,
        b.stage_idx,
        b.status,
        b.when_,
        b.allow_failure,
        b.needs,
        b.spec,
        b.retry_max,
        b.retry_when,
        b.attempt,
        b.tags,
        b.timeout_secs,
        b.user_id,
        b.resource_group
    )
    .fetch_one(e)
    .await
}

/// The newest successful run of job `name` on `git_ref` of a project
/// (`needs:project`): the job of the newest pipeline that has it passed.
pub async fn latest_successful_job(e: impl PgExecutor<'_>, project_id: i64, git_ref: &str, name: &str) -> Result<Option<Build>> {
    sqlx::query_as!(
        Build,
        "SELECT b.id, b.pipeline_id, b.project_id, b.name, b.stage, b.stage_idx, b.status, b.when_, b.allow_failure, b.needs, b.spec,
                b.retry_max, b.retry_when, b.attempt, b.retried, b.tags, b.timeout_secs, b.user_id, b.runner_id, b.runner_manager_id,
                b.failure_reason, b.exit_code, b.trace_size, b.trace_archived, b.created_at, b.queued_at, b.started_at, b.finished_at,
                b.contacted_at, b.cancel_requested_at
           FROM ci_builds b JOIN ci_pipelines p ON p.id = b.pipeline_id
          WHERE b.project_id = $1 AND p.ref = $2 AND b.name = $3 AND b.status = 'success' AND NOT b.retried
          ORDER BY p.id DESC, b.id DESC LIMIT 1",
        project_id,
        git_ref,
        name
    )
    .fetch_optional(e)
    .await
}

/// Job `name` of a pipeline, when it passed (`needs:pipeline`).
pub async fn successful_job_in(e: impl PgExecutor<'_>, pipeline_id: i64, name: &str) -> Result<Option<Build>> {
    sqlx::query_as!(
        Build,
        "SELECT id, pipeline_id, project_id, name, stage, stage_idx, status, when_, allow_failure, needs, spec, retry_max,
                retry_when, attempt, retried, tags, timeout_secs, user_id, runner_id, runner_manager_id, failure_reason,
                exit_code, trace_size, trace_archived, created_at, queued_at, started_at, finished_at, contacted_at,
                cancel_requested_at
           FROM ci_builds WHERE pipeline_id = $1 AND name = $2 AND status = 'success' AND NOT retried ORDER BY id DESC LIMIT 1",
        pipeline_id,
        name
    )
    .fetch_optional(e)
    .await
}

/// The job a live job token belongs to (one query: every trace update and
/// keep-alive asks).
pub async fn build_by_token(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<Build>> {
    sqlx::query_as!(
        Build,
        "SELECT b.id, b.pipeline_id, b.project_id, b.name, b.stage, b.stage_idx, b.status, b.when_, b.allow_failure, b.needs, b.spec, b.retry_max,
                b.retry_when, b.attempt, b.retried, b.tags, b.timeout_secs, b.user_id, b.runner_id, b.runner_manager_id, b.failure_reason,
                b.exit_code, b.trace_size, b.trace_archived, b.created_at, b.queued_at, b.started_at, b.finished_at, b.contacted_at,
                b.cancel_requested_at
           FROM ci_job_tokens t JOIN ci_builds b ON b.id = t.build_id
          WHERE t.token_hash = $1 AND t.expires_at > now()",
        hash
    )
    .fetch_optional(e)
    .await
}

pub async fn build(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Build>> {
    sqlx::query_as!(
        Build,
        "SELECT id, pipeline_id, project_id, name, stage, stage_idx, status, when_, allow_failure, needs, spec, retry_max,
                    retry_when, attempt, retried, tags, timeout_secs, user_id, runner_id, runner_manager_id, failure_reason,
                    exit_code, trace_size, trace_archived, created_at, queued_at, started_at, finished_at, contacted_at,
                    cancel_requested_at
             FROM ci_builds WHERE id = $1",
        id
    )
    .fetch_optional(e)
    .await
}

/// The build row, locked for this transaction (state changes, trace appends).
pub async fn build_for_update(c: &mut PgConnection, id: i64) -> Result<Option<Build>> {
    sqlx::query_as!(
        Build,
        "SELECT id, pipeline_id, project_id, name, stage, stage_idx, status, when_, allow_failure, needs, spec, retry_max,
                    retry_when, attempt, retried, tags, timeout_secs, user_id, runner_id, runner_manager_id, failure_reason,
                    exit_code, trace_size, trace_archived, created_at, queued_at, started_at, finished_at, contacted_at,
                    cancel_requested_at
             FROM ci_builds WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(c)
    .await
}

/// Every attempt of every job, oldest first.
pub async fn builds_of_pipeline(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Vec<Build>> {
    sqlx::query_as!(
        Build,
        "SELECT id, pipeline_id, project_id, name, stage, stage_idx, status, when_, allow_failure, needs, spec, retry_max,
                    retry_when, attempt, retried, tags, timeout_secs, user_id, runner_id, runner_manager_id, failure_reason,
                    exit_code, trace_size, trace_archived, created_at, queued_at, started_at, finished_at, contacted_at,
                    cancel_requested_at
             FROM ci_builds WHERE pipeline_id = $1 ORDER BY stage_idx, name, id",
        pipeline_id
    )
    .fetch_all(e)
    .await
}

/// Latest attempts of the jobs of several pipelines (list pages), in one query.
pub async fn live_builds_of_pipelines(e: impl PgExecutor<'_>, pipeline_ids: &[i64]) -> Result<Vec<Build>> {
    sqlx::query_as!(
        Build,
        "SELECT id, pipeline_id, project_id, name, stage, stage_idx, status, when_, allow_failure, needs, spec, retry_max,
                retry_when, attempt, retried, tags, timeout_secs, user_id, runner_id, runner_manager_id, failure_reason,
                exit_code, trace_size, trace_archived, created_at, queued_at, started_at, finished_at, contacted_at,
                cancel_requested_at
         FROM ci_builds WHERE pipeline_id = ANY ($1) AND NOT retried ORDER BY pipeline_id, stage_idx, name, id",
        pipeline_ids
    )
    .fetch_all(e)
    .await
}

pub async fn lock_pipeline_builds(c: &mut PgConnection, pipeline_id: i64) -> Result<Vec<Build>> {
    sqlx::query_as!(
        Build,
        "SELECT id, pipeline_id, project_id, name, stage, stage_idx, status, when_, allow_failure, needs, spec, retry_max,
                    retry_when, attempt, retried, tags, timeout_secs, user_id, runner_id, runner_manager_id, failure_reason,
                    exit_code, trace_size, trace_archived, created_at, queued_at, started_at, finished_at, contacted_at,
                    cancel_requested_at
             FROM ci_builds WHERE pipeline_id = $1 AND NOT retried ORDER BY stage_idx, name, id FOR UPDATE",
        pipeline_id
    )
    .fetch_all(c)
    .await
}

pub async fn set_build_status(e: impl PgExecutor<'_>, id: i64, status: &str, failure_reason: Option<&str>) -> Result<()> {
    sqlx::query!(
        "UPDATE ci_builds SET status = $2, updated_at = now(),
             failure_reason = coalesce($3, failure_reason),
             queued_at = CASE WHEN $2 = 'pending' THEN now() ELSE queued_at END,
             finished_at = CASE WHEN $2 IN ('success', 'failed', 'canceled', 'skipped') THEN now() ELSE finished_at END
         WHERE id = $1",
        id,
        status,
        failure_reason
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Finish a job only if it is still running; false when another path
/// (runner update, stuck-job sweep) already finished it.
pub async fn finish_running(
    e: impl PgExecutor<'_>,
    id: i64,
    status: &str,
    failure_reason: Option<&str>,
    exit_code: Option<i32>,
) -> Result<bool> {
    let n = sqlx::query!(
        "UPDATE ci_builds SET status = $2, failure_reason = $3, exit_code = $4, finished_at = now(), updated_at = now(),
             contacted_at = now()
         WHERE id = $1 AND status IN ('running', 'canceling')",
        id,
        status,
        failure_reason,
        exit_code
    )
    .execute(e)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// One more refused final update for missing log bytes; returns the count.
pub async fn bump_trace_rewinds(e: impl PgExecutor<'_>, id: i64) -> Result<i16> {
    sqlx::query_scalar!("UPDATE ci_builds SET trace_rewinds = trace_rewinds + 1 WHERE id = $1 RETURNING trace_rewinds", id)
        .fetch_one(e)
        .await
}

pub async fn touch_build(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET contacted_at = now() WHERE id = $1", id).execute(e).await?;
    Ok(())
}

pub async fn request_cancel(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE ci_builds SET status = 'canceling', cancel_requested_at = now(), updated_at = now() WHERE id = $1 AND status = 'running'",
        id
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_build_timeout(e: impl PgExecutor<'_>, id: i64, secs: i32) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET timeout_secs = $2 WHERE id = $1", id, secs).execute(e).await?;
    Ok(())
}

/// Finished jobs whose log is still in chunks (archiving failed or pending).
pub async fn unarchived_finished(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM ci_builds WHERE NOT trace_archived AND status IN ('success', 'failed', 'canceled')
           AND finished_at < now() - interval '30 seconds'
         ORDER BY finished_at LIMIT $1",
        limit
    )
    .fetch_all(e)
    .await
}

pub async fn set_allow_failure(e: impl PgExecutor<'_>, id: i64, allow: bool) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET allow_failure = $2 WHERE id = $1", id, allow).execute(e).await?;
    Ok(())
}

pub async fn set_build_user(e: impl PgExecutor<'_>, id: i64, user_id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET user_id = $2 WHERE id = $1", id, user_id).execute(e).await?;
    Ok(())
}

pub async fn mark_retried(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET retried = true WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// Jobs whose runner went silent: running past their timeout plus grace, or
/// not heard from for `silence` seconds.
pub async fn stale_running(e: impl PgExecutor<'_>, silence_secs: f64, grace_secs: i32) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM ci_builds WHERE status IN ('running', 'canceling') AND runner_id IS NOT NULL
           AND (coalesce(contacted_at, started_at) < now() - make_interval(secs => $1)
                OR started_at < now() - make_interval(secs => timeout_secs + $2))",
        silence_secs,
        grace_secs
    )
    .fetch_all(e)
    .await
}

// ---------------------------------------------------------------- pending queue

pub async fn enqueue(e: impl PgExecutor<'_>, build_id: i64) -> Result<()> {
    sqlx::query!(
        "INSERT INTO ci_pending_jobs (build_id, project_id, tags, protected, traversal_ids, instance_runners_enabled, instance_only)
         SELECT b.id, b.project_id, b.tags, pl.protected, n.traversal_ids, coalesce(s.instance_runners_enabled, true) OR is_system_source(pl.source),
                is_system_source(pl.source)
         FROM ci_builds b JOIN ci_pipelines pl ON pl.id = b.pipeline_id JOIN projects p ON p.id = b.project_id
         JOIN namespaces n ON n.id = p.namespace_id LEFT JOIN ci_project_settings s ON s.project_id = p.id
         WHERE b.id = $1
         ON CONFLICT (build_id) DO NOTHING",
        build_id
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn dequeue(e: impl PgExecutor<'_>, build_id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM ci_pending_jobs WHERE build_id = $1", build_id).execute(e).await?;
    Ok(())
}

/// Claim the oldest pending job this runner may run (ADR-0014): scope by
/// runner kind, protected refs, and tags (a job's tags must all be on the
/// runner; untagged jobs need `run_untagged`). Marks it running.
pub async fn claim(c: &mut PgConnection, r: &Runner, manager_id: i64) -> Result<Option<Build>> {
    let id = sqlx::query_scalar!(
        "SELECT q.build_id FROM ci_pending_jobs q
         WHERE CASE $1
                 WHEN 'instance' THEN q.instance_runners_enabled
                 WHEN 'group' THEN $2::bigint = ANY (q.traversal_ids) AND NOT q.instance_only
                 ELSE q.project_id = $3::bigint AND NOT q.instance_only
               END
           AND (NOT $4 OR q.protected)
           AND q.tags <@ $5
           AND ($6 OR cardinality(q.tags) > 0)
         ORDER BY q.build_id
         LIMIT 1
         FOR UPDATE SKIP LOCKED",
        r.kind,
        r.group_id,
        r.project_id,
        r.protected_only,
        &r.tags,
        r.run_untagged
    )
    .fetch_optional(&mut *c)
    .await?;
    let Some(id) = id else { return Ok(None) };
    sqlx::query!("DELETE FROM ci_pending_jobs WHERE build_id = $1", id).execute(&mut *c).await?;
    sqlx::query!(
        "UPDATE ci_builds SET status = 'running', runner_id = $2, runner_manager_id = $3, started_at = now(), contacted_at = now(),
             updated_at = now()
         WHERE id = $1",
        id,
        r.id,
        manager_id
    )
    .execute(&mut *c)
    .await?;
    build_for_update(c, id).await
}

// ---------------------------------------------------------------- traces

pub enum Append {
    /// New total size.
    Ok(i64),
    /// The offset did not match: the current size.
    RangeMismatch(i64),
}

/// Append bytes at `offset` (must equal the current size) to a locked build.
pub async fn append_trace(c: &mut PgConnection, build: &Build, offset: i64, data: &[u8]) -> Result<Append> {
    if offset != build.trace_size {
        return Ok(Append::RangeMismatch(build.trace_size));
    }
    let mut pos = offset;
    let mut rest = data;
    while !rest.is_empty() {
        let idx = (pos / TRACE_CHUNK) as i32;
        let room = (TRACE_CHUNK - pos % TRACE_CHUNK) as usize;
        let (part, tail) = rest.split_at(rest.len().min(room));
        sqlx::query!(
            "INSERT INTO ci_job_trace_chunks (build_id, chunk_index, data) VALUES ($1, $2, $3)
             ON CONFLICT (build_id, chunk_index) DO UPDATE SET data = ci_job_trace_chunks.data || $3",
            build.id,
            idx,
            part
        )
        .execute(&mut *c)
        .await?;
        pos += part.len() as i64;
        rest = tail;
    }
    sqlx::query!("UPDATE ci_builds SET trace_size = $2, contacted_at = now() WHERE id = $1", build.id, pos).execute(&mut *c).await?;
    Ok(Append::Ok(pos))
}

/// Bytes from `offset` to the end of the live trace (chunks still in Postgres).
/// At most `max` live log bytes from `offset` (only the chunks that hold them are read).
pub async fn read_trace(e: impl PgExecutor<'_>, build_id: i64, offset: i64, max: i64) -> Result<Vec<u8>> {
    let offset = offset.max(0);
    let max = max.max(0);
    let first = (offset / TRACE_CHUNK) as i32;
    let last = (offset.saturating_add(max) / TRACE_CHUNK).min(i32::MAX as i64) as i32;
    let chunks = sqlx::query_scalar!(
        "SELECT data FROM ci_job_trace_chunks WHERE build_id = $1 AND chunk_index BETWEEN $2 AND $3 ORDER BY chunk_index",
        build_id,
        first,
        last
    )
    .fetch_all(e)
    .await?;
    let mut out = Vec::new();
    for c in chunks {
        out.extend_from_slice(&c);
    }
    let skip = (offset - first as i64 * TRACE_CHUNK) as usize;
    let mut out = if skip >= out.len() { Vec::new() } else { out.split_off(skip) };
    out.truncate(usize::try_from(max).unwrap_or(usize::MAX));
    Ok(out)
}

pub async fn mark_trace_archived(c: &mut PgConnection, build_id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET trace_archived = true WHERE id = $1", build_id).execute(&mut *c).await?;
    sqlx::query!("DELETE FROM ci_job_trace_chunks WHERE build_id = $1", build_id).execute(&mut *c).await?;
    Ok(())
}

// ---------------------------------------------------------------- artifacts

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Artifact {
    pub id: i64,
    pub build_id: i64,
    pub project_id: i64,
    pub file_type: String,
    pub file_format: String,
    pub filename: String,
    pub size: i64,
    #[serde(skip)]
    pub blob_key: String,
    pub expire_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub struct NewArtifact<'a> {
    pub build_id: i64,
    pub project_id: i64,
    pub file_type: &'a str,
    pub file_format: &'a str,
    pub filename: &'a str,
    pub size: i64,
    pub blob_key: &'a str,
    pub expire_at: Option<DateTime<Utc>>,
}

/// Insert or replace (a retried upload of the same type); returns the blob key
/// it replaced, for deletion.
pub async fn upsert_artifact(c: &mut PgConnection, a: NewArtifact<'_>) -> Result<Option<String>> {
    let old = sqlx::query_scalar!("SELECT blob_key FROM ci_job_artifacts WHERE build_id = $1 AND file_type = $2", a.build_id, a.file_type)
        .fetch_optional(&mut *c)
        .await?;
    sqlx::query!(
        "INSERT INTO ci_job_artifacts (build_id, project_id, file_type, file_format, filename, size, blob_key, expire_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (build_id, file_type) DO UPDATE SET file_format = $4, filename = $5, size = $6, blob_key = $7, expire_at = $8,
             created_at = now()",
        a.build_id,
        a.project_id,
        a.file_type,
        a.file_format,
        a.filename,
        a.size,
        a.blob_key,
        a.expire_at
    )
    .execute(&mut *c)
    .await?;
    Ok(old.filter(|k| k != a.blob_key))
}

/// Bytes of artifacts a project keeps (every type, expired ones not yet removed included).
pub async fn artifact_bytes(e: impl PgExecutor<'_>, project_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar!("SELECT COALESCE(sum(size), 0)::bigint FROM ci_job_artifacts WHERE project_id = $1", project_id)
        .fetch_one(e)
        .await?
        .unwrap_or(0))
}

pub async fn artifacts_of_build(e: impl PgExecutor<'_>, build_id: i64) -> Result<Vec<Artifact>> {
    sqlx::query_as!(
        Artifact,
        "SELECT id, build_id, project_id, file_type, file_format, filename, size, blob_key, expire_at, created_at
         FROM ci_job_artifacts WHERE build_id = $1 ORDER BY id",
        build_id
    )
    .fetch_all(e)
    .await
}

/// Unexpired archives of several jobs (a job's dependencies), in one query.
pub async fn archives_of(e: impl PgExecutor<'_>, build_ids: &[i64]) -> Result<Vec<Artifact>> {
    sqlx::query_as!(
        Artifact,
        "SELECT id, build_id, project_id, file_type, file_format, filename, size, blob_key, expire_at, created_at
         FROM ci_job_artifacts WHERE build_id = ANY ($1) AND file_type = 'archive' AND (expire_at IS NULL OR expire_at > now())",
        build_ids
    )
    .fetch_all(e)
    .await
}

pub async fn artifact(e: impl PgExecutor<'_>, build_id: i64, file_type: &str) -> Result<Option<Artifact>> {
    sqlx::query_as!(
        Artifact,
        "SELECT id, build_id, project_id, file_type, file_format, filename, size, blob_key, expire_at, created_at
         FROM ci_job_artifacts WHERE build_id = $1 AND file_type = $2 AND (expire_at IS NULL OR expire_at > now())",
        build_id,
        file_type
    )
    .fetch_optional(e)
    .await
}

/// Up to `limit` expired artifacts (rows deleted; the caller removes the blobs).
pub async fn take_expired_artifacts(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<String>> {
    sqlx::query_scalar!(
        "DELETE FROM ci_job_artifacts WHERE id IN (
             SELECT id FROM ci_job_artifacts WHERE expire_at < now() ORDER BY expire_at LIMIT $1 FOR UPDATE SKIP LOCKED)
         RETURNING blob_key",
        limit
    )
    .fetch_all(e)
    .await
}

// ---------------------------------------------------------------- schedules

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Schedule {
    pub id: i64,
    pub project_id: i64,
    pub description: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub cron: String,
    pub cron_timezone: String,
    pub active: bool,
    pub owner_id: Option<i64>,
    pub owner_username: Option<String>,
    #[serde(skip)]
    pub variables_enc: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_pipeline_id: Option<i64>,
    pub created_at: DateTime<Utc>,
}

pub struct ScheduleInput<'a> {
    pub description: &'a str,
    pub git_ref: &'a str,
    pub cron: &'a str,
    pub cron_timezone: &'a str,
    pub active: bool,
    pub variables_enc: Option<&'a str>,
    pub next_run_at: Option<DateTime<Utc>>,
}

pub async fn schedules(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<Schedule>> {
    sqlx::query_as!(
        Schedule,
        r#"SELECT s.id, s.project_id, s.description, s.ref AS git_ref, s.cron, s.cron_timezone, s.active, s.owner_id,
                  u.username AS "owner_username?", s.variables_enc, s.next_run_at, s.last_pipeline_id, s.created_at
           FROM ci_pipeline_schedules s LEFT JOIN users u ON u.id = s.owner_id
           WHERE s.project_id = $1 ORDER BY s.id"#,
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn schedule(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<Option<Schedule>> {
    sqlx::query_as!(
        Schedule,
        r#"SELECT s.id, s.project_id, s.description, s.ref AS git_ref, s.cron, s.cron_timezone, s.active, s.owner_id,
                  u.username AS "owner_username?", s.variables_enc, s.next_run_at, s.last_pipeline_id, s.created_at
           FROM ci_pipeline_schedules s LEFT JOIN users u ON u.id = s.owner_id
           WHERE s.project_id = $1 AND s.id = $2"#,
        project_id,
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn create_schedule(e: impl PgExecutor<'_>, project_id: i64, owner_id: i64, s: ScheduleInput<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO ci_pipeline_schedules (project_id, owner_id, description, ref, cron, cron_timezone, active, variables_enc, next_run_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        project_id,
        owner_id,
        s.description,
        s.git_ref,
        s.cron,
        s.cron_timezone,
        s.active,
        s.variables_enc,
        s.next_run_at
    )
    .fetch_one(e)
    .await
}

/// Editing makes the editor the owner (pipelines run as them), as GitLab does.
pub async fn update_schedule(e: impl PgExecutor<'_>, id: i64, owner_id: i64, s: ScheduleInput<'_>) -> Result<()> {
    sqlx::query!(
        "UPDATE ci_pipeline_schedules SET owner_id = $2, description = $3, ref = $4, cron = $5, cron_timezone = $6, active = $7,
             variables_enc = $8, next_run_at = $9
         WHERE id = $1",
        id,
        owner_id,
        s.description,
        s.git_ref,
        s.cron,
        s.cron_timezone,
        s.active,
        s.variables_enc,
        s.next_run_at
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete_schedule(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<bool> {
    let n = sqlx::query!("DELETE FROM ci_pipeline_schedules WHERE project_id = $1 AND id = $2", project_id, id)
        .execute(e)
        .await?
        .rows_affected();
    Ok(n > 0)
}

/// Due schedules, locked for this transaction (several workers never run one twice).
pub async fn due_schedules(c: &mut PgConnection, limit: i64) -> Result<Vec<Schedule>> {
    sqlx::query_as!(
        Schedule,
        r#"SELECT s.id, s.project_id, s.description, s.ref AS git_ref, s.cron, s.cron_timezone, s.active, s.owner_id,
                  NULL::text AS "owner_username?", s.variables_enc, s.next_run_at, s.last_pipeline_id, s.created_at
           FROM ci_pipeline_schedules s
           WHERE s.active AND s.next_run_at <= now()
             -- an archived project's schedules wait (they stay on)
             AND NOT EXISTS (SELECT 1 FROM projects p WHERE p.id = s.project_id AND p.archived_at IS NOT NULL)
           ORDER BY s.next_run_at LIMIT $1 FOR UPDATE SKIP LOCKED"#,
        limit
    )
    .fetch_all(&mut *c)
    .await
}

/// Turn a schedule off (its owner can no longer run it).
pub async fn deactivate_schedule(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_pipeline_schedules SET active = false WHERE id = $1", id).execute(e).await?;
    Ok(())
}

pub async fn schedule_ran(e: impl PgExecutor<'_>, id: i64, next_run_at: Option<DateTime<Utc>>, pipeline_id: Option<i64>) -> Result<()> {
    sqlx::query!(
        "UPDATE ci_pipeline_schedules SET next_run_at = $2, last_pipeline_id = coalesce($3, last_pipeline_id) WHERE id = $1",
        id,
        next_run_at,
        pipeline_id
    )
    .execute(e)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------- job token scope

/// May job tokens of `source` read `target`? Yes when `target` lists `source`
/// or has turned the scope check off.
pub async fn job_token_allowed(e: impl PgExecutor<'_>, target: i64, source: i64) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        "SELECT NOT coalesce((SELECT job_token_scope_enabled FROM ci_project_settings WHERE project_id = $1), true)
             OR EXISTS (SELECT 1 FROM ci_job_token_allowlist WHERE project_id = $1 AND source_project_id = $2)",
        target,
        source
    )
    .fetch_one(e)
    .await?
    .unwrap_or(false))
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AllowedProject {
    pub project_id: i64,
    pub full_path: String,
    pub created_at: DateTime<Utc>,
}

pub async fn allowlist(e: impl PgExecutor<'_>, target: i64) -> Result<Vec<AllowedProject>> {
    sqlx::query_as!(
        AllowedProject,
        "SELECT a.source_project_id AS project_id, r.path AS full_path, a.created_at
         FROM ci_job_token_allowlist a JOIN routes r ON r.source_type = 'project' AND r.source_id = a.source_project_id
         WHERE a.project_id = $1 ORDER BY r.path",
        target
    )
    .fetch_all(e)
    .await
}

pub async fn allow(e: impl PgExecutor<'_>, target: i64, source: i64) -> Result<()> {
    sqlx::query!(
        "INSERT INTO ci_job_token_allowlist (project_id, source_project_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        target,
        source
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn disallow(e: impl PgExecutor<'_>, target: i64, source: i64) -> Result<bool> {
    let n = sqlx::query!("DELETE FROM ci_job_token_allowlist WHERE project_id = $1 AND source_project_id = $2", target, source)
        .execute(e)
        .await?
        .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------- job tokens

pub async fn create_build_token(
    e: impl PgExecutor<'_>,
    hash: &[u8],
    project_id: i64,
    user_id: i64,
    build_id: i64,
    expires_at: DateTime<Utc>,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO ci_job_tokens (token_hash, project_id, user_id, build_id, expires_at) VALUES ($1, $2, $3, $4, $5) RETURNING id",
        hash,
        project_id,
        user_id,
        build_id,
        expires_at
    )
    .fetch_one(e)
    .await
}

/// The build a job token belongs to (any state; callers check it is running).
pub async fn build_id_for_token(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!("SELECT build_id FROM ci_job_tokens WHERE token_hash = $1 AND expires_at > now()", hash)
        .fetch_optional(e)
        .await?
        .flatten())
}

/// Job tokens stop working when their job ends.
pub async fn expire_build_tokens(e: impl PgExecutor<'_>, build_id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_job_tokens SET expires_at = least(expires_at, now()) WHERE build_id = $1", build_id).execute(e).await?;
    Ok(())
}

// ------------------------------------------------------------------ external commit statuses

#[derive(Debug, Clone, Serialize)]
pub struct ExternalStatus {
    pub id: i64,
    pub sha: String,
    pub git_ref: Option<String>,
    pub name: String,
    pub status: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    pub author_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct NewExternalStatus<'a> {
    pub project_id: i64,
    pub sha: &'a str,
    pub git_ref: Option<&'a str>,
    pub name: &'a str,
    pub status: &'a str,
    pub description: Option<&'a str>,
    pub target_url: Option<&'a str>,
    pub author_id: Option<i64>,
}

/// One status per (commit, name): a new state replaces the old one.
pub async fn set_external_status(e: impl PgExecutor<'_>, s: NewExternalStatus<'_>) -> Result<ExternalStatus> {
    sqlx::query_as!(
        ExternalStatus,
        r#"INSERT INTO commit_statuses (project_id, sha, ref, name, status, description, target_url, author_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           ON CONFLICT (project_id, sha, name) DO UPDATE SET status = EXCLUDED.status, ref = coalesce(EXCLUDED.ref, commit_statuses.ref),
               description = EXCLUDED.description, target_url = EXCLUDED.target_url, author_id = EXCLUDED.author_id, updated_at = now()
           RETURNING id, sha, ref AS git_ref, name, status, description, target_url, author_id, created_at, updated_at"#,
        s.project_id,
        s.sha,
        s.git_ref,
        s.name,
        s.status,
        s.description,
        s.target_url,
        s.author_id
    )
    .fetch_one(e)
    .await
}

pub async fn external_statuses(e: impl PgExecutor<'_>, project_id: i64, sha: &str) -> Result<Vec<ExternalStatus>> {
    sqlx::query_as!(
        ExternalStatus,
        r#"SELECT id, sha, ref AS git_ref, name, status, description, target_url, author_id, created_at, updated_at
           FROM commit_statuses WHERE project_id = $1 AND sha = $2 ORDER BY id"#,
        project_id,
        sha
    )
    .fetch_all(e)
    .await
}

// ---------------------------------------------------------------- report data (M7)

/// Replace a job's dotenv variables.
pub async fn set_dotenv(conn: &mut PgConnection, build_id: i64, vars: &[(String, String)]) -> Result<()> {
    sqlx::query!("DELETE FROM ci_build_dotenv WHERE build_id = $1", build_id).execute(&mut *conn).await?;
    // all of them in one statement, in file order
    let (keys, values): (Vec<String>, Vec<String>) = vars.iter().cloned().unzip();
    sqlx::query!(
        "INSERT INTO ci_build_dotenv (build_id, position, key, value)
         SELECT $1, (t.ord - 1)::int, t.k, t.v FROM unnest($2::text[], $3::text[]) WITH ORDINALITY AS t(k, v, ord)",
        build_id,
        &keys,
        &values
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Dotenv variables of these jobs: (build id, key, value), in file order.
pub async fn dotenv_of(e: impl PgExecutor<'_>, build_ids: &[i64]) -> Result<Vec<(i64, String, String)>> {
    Ok(sqlx::query!("SELECT build_id, key, value FROM ci_build_dotenv WHERE build_id = ANY($1) ORDER BY build_id, position", build_ids)
        .fetch_all(e)
        .await?
        .into_iter()
        .map(|r| (r.build_id, r.key, r.value))
        .collect())
}

pub struct NewTestReport<'a> {
    pub build_id: i64,
    pub pipeline_id: i64,
    pub total: i32,
    pub failed: i32,
    pub errored: i32,
    pub skipped: i32,
    pub duration: f64,
    pub failures: &'a serde_json::Value,
    pub cases: &'a serde_json::Value,
    pub error: Option<&'a str>,
}

pub async fn set_test_report(e: impl PgExecutor<'_>, r: NewTestReport<'_>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO ci_build_test_reports (build_id, pipeline_id, total, failed, errored, skipped, duration, failures, cases, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (build_id) DO UPDATE SET total = $3, failed = $4, errored = $5, skipped = $6, duration = $7, failures = $8,
             cases = $9, error = $10, created_at = now()",
        r.build_id,
        r.pipeline_id,
        r.total,
        r.failed,
        r.errored,
        r.skipped,
        r.duration,
        r.failures,
        r.cases,
        r.error
    )
    .execute(e)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct TestReport {
    pub build_id: i64,
    pub job_name: String,
    pub stage: String,
    pub total: i32,
    pub failed: i32,
    pub errored: i32,
    pub skipped: i32,
    pub duration: f64,
    #[schema(value_type = Object)]
    pub failures: serde_json::Value,
    #[serde(skip)]
    pub cases: serde_json::Value,
    /// Set when the report could not be read.
    pub error: Option<String>,
}

/// Test reports of the latest attempt of each job of a pipeline.
pub async fn test_reports(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Vec<TestReport>> {
    sqlx::query_as!(
        TestReport,
        "SELECT t.build_id, b.name AS job_name, b.stage, t.total, t.failed, t.errored, t.skipped, t.duration, t.failures, t.cases, t.error
           FROM ci_build_test_reports t JOIN ci_builds b ON b.id = t.build_id
          WHERE t.pipeline_id = $1 AND NOT b.retried
          ORDER BY b.stage_idx, b.name",
        pipeline_id
    )
    .fetch_all(e)
    .await
}

/// The newest pipeline with test reports for a commit (a merge request's head),
/// or for a branch (its target), among finished ones for the branch.
pub async fn pipeline_with_tests(e: impl PgExecutor<'_>, project_id: i64, sha: Option<&str>, branch: Option<&str>) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        "SELECT p.id FROM ci_pipelines p
          WHERE p.project_id = $1 AND NOT p.tag AND NOT is_system_source(p.source)
            AND ($2::text IS NULL OR p.sha = $2)
            AND ($3::text IS NULL OR (p.ref = $3 AND p.source <> 'merge_request_event' AND p.status IN ('success', 'failed')))
            AND EXISTS (SELECT 1 FROM ci_build_test_reports t WHERE t.pipeline_id = p.id)
          ORDER BY p.id DESC LIMIT 1",
        project_id,
        sha,
        branch
    )
    .fetch_optional(e)
    .await
}

// ---------------------------------------------------------------- CI keywords

/// Another job of the project's resource group holds it (pending or running),
/// besides `except`.
pub async fn resource_busy(e: impl PgExecutor<'_>, project_id: i64, group: &str, except: i64) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM ci_builds WHERE project_id = $1 AND resource_group = $2 AND id <> $3
                          AND status IN ('pending', 'preparing', 'running', 'canceling')) AS "x!""#,
        project_id,
        group,
        except
    )
    .fetch_one(e)
    .await
}

pub async fn resource_group_of(e: impl PgExecutor<'_>, build_id: i64) -> Result<Option<(i64, String)>> {
    Ok(sqlx::query!("SELECT project_id, resource_group FROM ci_builds WHERE id = $1", build_id)
        .fetch_optional(e)
        .await?
        .and_then(|r| r.resource_group.map(|g| (r.project_id, g))))
}

/// The oldest job waiting for a free resource group: (id, pipeline id).
pub async fn next_waiting(e: impl PgExecutor<'_>, project_id: i64, group: &str) -> Result<Option<(i64, i64)>> {
    Ok(sqlx::query!(
        "SELECT id, pipeline_id FROM ci_builds WHERE project_id = $1 AND resource_group = $2 AND status = 'waiting_for_resource'
          ORDER BY id LIMIT 1",
        project_id,
        group
    )
    .fetch_optional(e)
    .await?
    .map(|r| (r.id, r.pipeline_id)))
}

pub async fn set_scheduled_at(e: impl PgExecutor<'_>, build_id: i64, at: Option<DateTime<Utc>>) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET scheduled_at = $2 WHERE id = $1", build_id, at).execute(e).await?;
    Ok(())
}

pub async fn scheduled_at(e: impl PgExecutor<'_>, build_id: i64) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar!("SELECT scheduled_at FROM ci_builds WHERE id = $1", build_id).fetch_optional(e).await?.flatten())
}

/// Delayed jobs whose time has come: (id, pipeline id).
pub async fn due_scheduled(e: impl PgExecutor<'_>) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query!("SELECT id, pipeline_id FROM ci_builds WHERE status = 'scheduled' AND scheduled_at <= now() LIMIT 500")
        .fetch_all(e)
        .await?
        .into_iter()
        .map(|r| (r.id, r.pipeline_id))
        .collect())
}

pub async fn set_coverage(e: impl PgExecutor<'_>, build_id: i64, coverage: f64) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET coverage = $2 WHERE id = $1", build_id, coverage).execute(e).await?;
    Ok(())
}

/// Coverage of jobs, by id.
pub async fn coverages(e: impl PgExecutor<'_>, ids: &[i64]) -> Result<Vec<(i64, f64)>> {
    Ok(sqlx::query!("SELECT id, coverage FROM ci_builds WHERE id = ANY($1) AND coverage IS NOT NULL", ids)
        .fetch_all(e)
        .await?
        .into_iter()
        .filter_map(|r| r.coverage.map(|c| (r.id, c)))
        .collect())
}

pub async fn link_downstream(e: impl PgExecutor<'_>, build_id: i64, pipeline_id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET downstream_pipeline_id = $2 WHERE id = $1", build_id, pipeline_id).execute(e).await?;
    Ok(())
}

pub async fn downstream_of(e: impl PgExecutor<'_>, build_id: i64) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!("SELECT downstream_pipeline_id FROM ci_builds WHERE id = $1", build_id).fetch_optional(e).await?.flatten())
}

/// The bridge job that started a pipeline, and `auto_cancel_on_job_failure`.
pub async fn pipeline_extras(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Option<(Option<i64>, String)>> {
    Ok(sqlx::query!("SELECT upstream_build_id, auto_cancel_on_job_failure FROM ci_pipelines WHERE id = $1", pipeline_id)
        .fetch_optional(e)
        .await?
        .map(|r| (r.upstream_build_id, r.auto_cancel_on_job_failure)))
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PipelineLink {
    pub id: i64,
    pub iid: i64,
    pub project_id: i64,
    pub project_path: String,
    pub status: String,
    pub source: String,
}

/// Pipelines the bridge jobs of `pipeline_id` started.
pub async fn downstream_pipelines(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Vec<(i64, PipelineLink)>> {
    Ok(sqlx::query!(
        "SELECT b.id AS build_id, p.id, p.iid, p.project_id, r.path AS project_path, p.status, p.source
           FROM ci_builds b JOIN ci_pipelines p ON p.id = b.downstream_pipeline_id
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.project_id
          WHERE b.pipeline_id = $1",
        pipeline_id
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| {
        (
            r.build_id,
            PipelineLink {
                id: r.id,
                iid: r.iid,
                project_id: r.project_id,
                project_path: r.project_path,
                status: r.status,
                source: r.source,
            },
        )
    })
    .collect())
}

/// The pipeline whose bridge job started `pipeline_id`.
pub async fn upstream_pipeline(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Option<PipelineLink>> {
    Ok(sqlx::query!(
        "SELECT u.id, u.iid, u.project_id, r.path AS project_path, u.status, u.source
           FROM ci_pipelines p JOIN ci_builds b ON b.id = p.upstream_build_id JOIN ci_pipelines u ON u.id = b.pipeline_id
           JOIN routes r ON r.source_type = 'project' AND r.source_id = u.project_id
          WHERE p.id = $1",
        pipeline_id
    )
    .fetch_optional(e)
    .await?
    .map(|r| PipelineLink {
        id: r.id,
        iid: r.iid,
        project_id: r.project_id,
        project_path: r.project_path,
        status: r.status,
        source: r.source,
    }))
}

pub async fn instance_key(e: impl PgExecutor<'_>, name: &str) -> Result<Option<String>> {
    sqlx::query_scalar!("SELECT value_enc FROM instance_keys WHERE name = $1", name).fetch_optional(e).await
}

/// Store a key unless one exists; returns the one kept.
pub async fn put_instance_key(e: impl PgExecutor<'_>, name: &str, value_enc: &str) -> Result<String> {
    sqlx::query_scalar!(
        "INSERT INTO instance_keys (name, value_enc) VALUES ($1, $2) ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING value_enc",
        name,
        value_enc
    )
    .fetch_one(e)
    .await
}

/// A job without a runner (a bridge) starts now.
pub async fn mark_started(e: impl PgExecutor<'_>, build_id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_builds SET started_at = coalesce(started_at, now()) WHERE id = $1", build_id).execute(e).await?;
    Ok(())
}
