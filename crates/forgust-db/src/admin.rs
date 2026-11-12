//! Instance-wide numbers for administrators: counts, CI activity,
//! storage by kind and by project, the job queue.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::{PgExecutor, Row};

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct InstanceCounts {
    /// People (not bots) who can sign in.
    pub users_active: i64,
    pub users_blocked: i64,
    pub users_admins: i64,
    /// People whose email is not confirmed yet.
    pub users_unconfirmed: i64,
    /// Project, group and Renovate bots.
    pub bots: i64,
    pub groups: i64,
    pub projects: i64,
    pub merge_requests_open: i64,
    pub merge_requests_merged: i64,
    pub issues_open: i64,
    pub issues_closed: i64,
}

pub async fn counts(e: impl PgExecutor<'_>) -> Result<InstanceCounts> {
    let r = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM users WHERE user_type = 'human' AND state = 'active') AS "users_active!",
             (SELECT count(*) FROM users WHERE user_type = 'human' AND state = 'blocked') AS "users_blocked!",
             (SELECT count(*) FROM users WHERE user_type = 'human' AND is_admin) AS "users_admins!",
             (SELECT count(*) FROM users WHERE user_type = 'human' AND email_verified_at IS NULL) AS "users_unconfirmed!",
             (SELECT count(*) FROM users WHERE user_type <> 'human') AS "bots!",
             (SELECT count(*) FROM namespaces WHERE kind = 'group') AS "groups!",
             (SELECT count(*) FROM projects) AS "projects!",
             (SELECT count(*) FROM merge_requests WHERE state = 'opened') AS "mr_open!",
             (SELECT count(*) FROM merge_requests WHERE state = 'merged') AS "mr_merged!",
             (SELECT count(*) FROM issues WHERE state = 'opened') AS "issues_open!",
             (SELECT count(*) FROM issues WHERE state = 'closed') AS "issues_closed!""#
    )
    .fetch_one(e)
    .await?;
    Ok(InstanceCounts {
        users_active: r.users_active,
        users_blocked: r.users_blocked,
        users_admins: r.users_admins,
        users_unconfirmed: r.users_unconfirmed,
        bots: r.bots,
        groups: r.groups,
        projects: r.projects,
        merge_requests_open: r.mr_open,
        merge_requests_merged: r.mr_merged,
        issues_open: r.issues_open,
        issues_closed: r.issues_closed,
    })
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PipelineDay {
    pub day: NaiveDate,
    pub success: i64,
    pub failed: i64,
    /// Canceled, skipped, still running...
    pub other: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct BusyProject {
    pub project_id: i64,
    pub full_path: String,
    pub pipelines: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CiActivity {
    /// The last 30 days, oldest first (days without pipelines included).
    pub pipelines_per_day: Vec<PipelineDay>,
    pub jobs_last_30_days: i64,
    /// Waiting for a runner now.
    pub jobs_queued: i64,
    pub jobs_running: i64,
    pub runners: i64,
    /// Asked for work in the last hour.
    pub runners_online: i64,
    pub runners_paused: i64,
    /// Most pipelines in the last 30 days.
    pub busiest_projects: Vec<BusyProject>,
}

pub async fn ci_activity(e: &sqlx::PgPool) -> Result<CiActivity> {
    let days = sqlx::query!(
        r#"SELECT d::date AS "day!",
                  count(p.id) FILTER (WHERE p.status = 'success') AS "success!",
                  count(p.id) FILTER (WHERE p.status = 'failed') AS "failed!",
                  count(p.id) FILTER (WHERE p.status NOT IN ('success', 'failed')) AS "other!"
             FROM generate_series(current_date - 29, current_date, interval '1 day') d
             LEFT JOIN ci_pipelines p ON p.created_at >= d AND p.created_at < d + interval '1 day'
            GROUP BY d ORDER BY d"#
    )
    .fetch_all(e)
    .await?;
    let r = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM ci_builds WHERE created_at > now() - interval '30 days') AS "jobs_30!",
             (SELECT count(*) FROM ci_pending_jobs) AS "queued!",
             (SELECT count(*) FROM ci_builds WHERE status IN ('running', 'canceling')) AS "running!",
             (SELECT count(*) FROM ci_runners) AS "runners!",
             (SELECT count(*) FROM ci_runners WHERE contacted_at > now() - interval '1 hour') AS "online!",
             (SELECT count(*) FROM ci_runners WHERE paused) AS "paused!""#
    )
    .fetch_one(e)
    .await?;
    let busy = sqlx::query!(
        r#"SELECT p.project_id AS "project_id!", r.path AS "full_path!", count(*) AS "pipelines!"
             FROM ci_pipelines p JOIN routes r ON r.source_type = 'project' AND r.source_id = p.project_id
            WHERE p.created_at > now() - interval '30 days'
            GROUP BY p.project_id, r.path ORDER BY 3 DESC LIMIT 10"#
    )
    .fetch_all(e)
    .await?;
    Ok(CiActivity {
        pipelines_per_day: days
            .into_iter()
            .map(|d| PipelineDay { day: d.day, success: d.success, failed: d.failed, other: d.other })
            .collect(),
        jobs_last_30_days: r.jobs_30,
        jobs_queued: r.queued,
        jobs_running: r.running,
        runners: r.runners,
        runners_online: r.online,
        runners_paused: r.paused,
        busiest_projects: busy
            .into_iter()
            .map(|b| BusyProject { project_id: b.project_id, full_path: b.full_path, pipelines: b.pipelines })
            .collect(),
    })
}

// ------------------------------------------------------------ storage

#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct StorageKinds {
    /// Git repositories on disk (as of their last measure, see `repository_bytes_at`).
    pub repositories: i64,
    /// Each LFS object once, however many projects use it.
    pub lfs: i64,
    pub artifacts: i64,
    /// Job logs moved to the blob store.
    pub job_logs_archived: i64,
    /// Logs of running (or not yet archived) jobs, in the database.
    pub job_logs_live: i64,
    /// Each image layer once.
    pub registry: i64,
    pub uploads: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProjectStorage {
    pub project_id: i64,
    pub full_path: String,
    pub repository: i64,
    pub lfs: i64,
    pub artifacts: i64,
    pub registry: i64,
    pub uploads: i64,
    pub total: i64,
    /// Its own storage quota.
    pub quota: Option<i64>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GroupStorage {
    pub namespace_id: i64,
    pub full_path: String,
    pub kind: String,
    pub projects: i64,
    pub total: i64,
    /// The quota in force (its own, or the instance's default).
    pub quota: Option<i64>,
    /// Its own setting: None for the default, 0 for none.
    pub own_quota: Option<i64>,
}

pub async fn storage_kinds(e: impl PgExecutor<'_>) -> Result<StorageKinds> {
    let r = sqlx::query!(
        r#"SELECT
             (SELECT coalesce(sum(repository_bytes), 0) FROM projects)::bigint AS "repos!",
             (SELECT coalesce(sum(size), 0) FROM lfs_objects)::bigint AS "lfs!",
             (SELECT coalesce(sum(size), 0) FROM ci_job_artifacts WHERE file_type <> 'trace')::bigint AS "artifacts!",
             (SELECT coalesce(sum(trace_size), 0) FROM ci_builds WHERE trace_archived)::bigint AS "logs_archived!",
             (SELECT coalesce(sum(octet_length(data)), 0) FROM ci_job_trace_chunks)::bigint AS "logs_live!",
             (SELECT coalesce(sum(size), 0) FROM registry_blobs)::bigint AS "registry!",
             (SELECT coalesce(sum(size), 0) FROM uploads)::bigint AS "uploads!""#
    )
    .fetch_one(e)
    .await?;
    Ok(StorageKinds {
        repositories: r.repos,
        lfs: r.lfs,
        artifacts: r.artifacts,
        job_logs_archived: r.logs_archived,
        job_logs_live: r.logs_live,
        registry: r.registry,
        uploads: r.uploads,
    })
}

/// The projects that keep the most, largest first. LFS objects and image layers
/// count for every project that uses them.
pub async fn largest_projects(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<ProjectStorage>> {
    let rows = sqlx::query!(
        r#"WITH lfs AS (SELECT lp.project_id, sum(o.size) AS b FROM lfs_objects_projects lp JOIN lfs_objects o ON o.oid = lp.oid GROUP BY 1),
                art AS (SELECT project_id, sum(size) AS b FROM ci_job_artifacts GROUP BY 1),
                reg AS (SELECT d.project_id, sum(b.size) AS b
                          FROM (SELECT DISTINCT r.project_id, l.digest FROM registry_repository_blobs l
                                  JOIN registry_repositories r ON r.id = l.repository_id) d
                          JOIN registry_blobs b ON b.digest = d.digest GROUP BY 1),
                up AS (SELECT project_id, sum(size) AS b FROM uploads GROUP BY 1)
           SELECT p.id AS "project_id!", r.path AS "full_path!", p.repository_bytes AS "repository!",
                  coalesce(lfs.b, 0)::bigint AS "lfs!", coalesce(art.b, 0)::bigint AS "artifacts!",
                  coalesce(reg.b, 0)::bigint AS "registry!", coalesce(up.b, 0)::bigint AS "uploads!",
                  (p.repository_bytes + coalesce(lfs.b, 0) + coalesce(art.b, 0) + coalesce(reg.b, 0) + coalesce(up.b, 0))::bigint AS "total!",
                  nullif(p.quota_bytes, 0) AS quota
             FROM projects p
             JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
             LEFT JOIN lfs ON lfs.project_id = p.id LEFT JOIN art ON art.project_id = p.id
             LEFT JOIN reg ON reg.project_id = p.id LEFT JOIN up ON up.project_id = p.id
            ORDER BY 8 DESC, 2 LIMIT $1"#,
        limit
    )
    .fetch_all(e)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ProjectStorage {
            project_id: r.project_id,
            full_path: r.full_path,
            repository: r.repository,
            lfs: r.lfs,
            artifacts: r.artifacts,
            registry: r.registry,
            uploads: r.uploads,
            total: r.total,
            quota: r.quota,
        })
        .collect())
}

/// Top-level groups and personal namespaces by what their projects keep (each
/// project counted as in [`largest_projects`]).
pub async fn largest_namespaces(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<GroupStorage>> {
    let rows = sqlx::query!(
        r#"WITH lfs AS (SELECT lp.project_id, sum(o.size) AS b FROM lfs_objects_projects lp JOIN lfs_objects o ON o.oid = lp.oid GROUP BY 1),
                art AS (SELECT project_id, sum(size) AS b FROM ci_job_artifacts GROUP BY 1),
                reg AS (SELECT d.project_id, sum(b.size) AS b
                          FROM (SELECT DISTINCT r.project_id, l.digest FROM registry_repository_blobs l
                                  JOIN registry_repositories r ON r.id = l.repository_id) d
                          JOIN registry_blobs b ON b.digest = d.digest GROUP BY 1),
                up AS (SELECT project_id, sum(size) AS b FROM uploads GROUP BY 1),
                per AS (SELECT p.id, n.traversal_ids[1] AS root,
                               p.repository_bytes + coalesce(lfs.b, 0) + coalesce(art.b, 0) + coalesce(reg.b, 0) + coalesce(up.b, 0) AS b
                          FROM projects p JOIN namespaces n ON n.id = p.namespace_id
                          LEFT JOIN lfs ON lfs.project_id = p.id LEFT JOIN art ON art.project_id = p.id
                          LEFT JOIN reg ON reg.project_id = p.id LEFT JOIN up ON up.project_id = p.id)
           SELECT root AS "namespace_id!", r.path AS "full_path!", n.kind AS "kind!", count(*) AS "projects!",
                  coalesce(sum(per.b), 0)::bigint AS "total!",
                  nullif(coalesce(n.quota_bytes, (SELECT namespace_quota_bytes FROM instance_settings)), 0) AS quota, n.quota_bytes AS own_quota
             FROM per JOIN namespaces n ON n.id = per.root
             JOIN routes r ON r.source_type = 'namespace' AND r.source_id = per.root
            GROUP BY root, r.path, n.kind, n.quota_bytes ORDER BY 5 DESC, 2 LIMIT $1"#,
        limit
    )
    .fetch_all(e)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| GroupStorage {
            namespace_id: r.namespace_id,
            full_path: r.full_path,
            kind: r.kind,
            projects: r.projects,
            total: r.total,
            quota: r.quota,
            own_quota: r.own_quota,
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AdminProject {
    pub id: i64,
    pub full_path: String,
    pub name: String,
    /// 0 private, 10 internal, 20 public
    pub visibility: i16,
    pub repository: i64,
    /// Everything it keeps (as in [`largest_projects`]).
    pub total: i64,
    pub open_merge_requests: i64,
    pub pipelines_30_days: i64,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
}

/// Every project, searchable; `sort`: `size`, `activity` (default) or `name`.
pub async fn projects(e: impl PgExecutor<'_>, search: &str, sort: &str, limit: i64, offset: i64) -> Result<Vec<AdminProject>> {
    sqlx::query_as!(
        AdminProject,
        r#"WITH lfs AS (SELECT lp.project_id, sum(o.size) AS b FROM lfs_objects_projects lp JOIN lfs_objects o ON o.oid = lp.oid GROUP BY 1),
                art AS (SELECT project_id, sum(size) AS b FROM ci_job_artifacts GROUP BY 1),
                reg AS (SELECT d.project_id, sum(b.size) AS b
                          FROM (SELECT DISTINCT r.project_id, l.digest FROM registry_repository_blobs l
                                  JOIN registry_repositories r ON r.id = l.repository_id) d
                          JOIN registry_blobs b ON b.digest = d.digest GROUP BY 1),
                up AS (SELECT project_id, sum(size) AS b FROM uploads GROUP BY 1)
           SELECT p.id, r.path AS "full_path!", p.name, p.visibility, p.repository_bytes AS repository,
                  (p.repository_bytes + coalesce(lfs.b, 0) + coalesce(art.b, 0) + coalesce(reg.b, 0) + coalesce(up.b, 0))::bigint AS "total!",
                  (SELECT count(*) FROM merge_requests m WHERE m.project_id = p.id AND m.state = 'opened') AS "open_merge_requests!",
                  (SELECT count(*) FROM ci_pipelines c WHERE c.project_id = p.id AND c.created_at > now() - interval '30 days') AS "pipelines_30_days!",
                  p.created_at, p.last_activity_at
             FROM projects p
             JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
             LEFT JOIN lfs ON lfs.project_id = p.id LEFT JOIN art ON art.project_id = p.id
             LEFT JOIN reg ON reg.project_id = p.id LEFT JOIN up ON up.project_id = p.id
            WHERE $1 = '' OR r.path ILIKE '%' || $1 || '%' OR p.name ILIKE '%' || $1 || '%'
            ORDER BY CASE WHEN $2 = 'size' THEN (p.repository_bytes + coalesce(lfs.b, 0) + coalesce(art.b, 0) + coalesce(reg.b, 0) + coalesce(up.b, 0)) END DESC NULLS LAST,
                     CASE WHEN $2 = 'name' THEN r.path END,
                     p.last_activity_at DESC
            LIMIT $3 OFFSET $4"#,
        search,
        sort,
        limit,
        offset
    )
    .fetch_all(e)
    .await
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AdminGroup {
    pub id: i64,
    pub full_path: String,
    pub name: String,
    pub visibility: i16,
    /// Projects in it and its subgroups.
    pub projects: i64,
    pub members: i64,
    pub subgroups: i64,
    pub created_at: DateTime<Utc>,
}

/// Every group (not personal namespaces), searchable, by path.
pub async fn groups(e: impl PgExecutor<'_>, search: &str, limit: i64, offset: i64) -> Result<Vec<AdminGroup>> {
    sqlx::query_as!(
        AdminGroup,
        r#"SELECT n.id, r.path AS "full_path!", n.name, n.visibility,
                  (SELECT count(*) FROM projects p JOIN namespaces pn ON pn.id = p.namespace_id WHERE n.id = ANY(pn.traversal_ids)) AS "projects!",
                  (SELECT count(*) FROM members m WHERE m.source_type = 'namespace' AND m.source_id = n.id) AS "members!",
                  (SELECT count(*) FROM namespaces c WHERE c.parent_id = n.id) AS "subgroups!",
                  n.created_at
             FROM namespaces n JOIN routes r ON r.source_type = 'namespace' AND r.source_id = n.id
            WHERE n.kind = 'group' AND ($1 = '' OR r.path ILIKE '%' || $1 || '%' OR n.name ILIKE '%' || $1 || '%')
            ORDER BY r.path LIMIT $2 OFFSET $3"#,
        search,
        limit,
        offset
    )
    .fetch_all(e)
    .await
}

/// Projects whose repository size is unknown or older than a day.
pub async fn stale_repository_sizes(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM projects WHERE repository_bytes_at IS NULL OR repository_bytes_at < now() - interval '1 day'
          ORDER BY repository_bytes_at NULLS FIRST LIMIT $1",
        limit
    )
    .fetch_all(e)
    .await
}

pub async fn set_repository_bytes(e: impl PgExecutor<'_>, project_id: i64, bytes: i64) -> Result<()> {
    sqlx::query!("UPDATE projects SET repository_bytes = $2, repository_bytes_at = now() WHERE id = $1", project_id, bytes)
        .execute(e)
        .await?;
    Ok(())
}

// ------------------------------------------------------------ blob store scan

#[derive(Debug, Clone, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct PrefixUsage {
    pub prefix: String,
    pub objects: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct BlobScan {
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub prefixes: Vec<PrefixUsage>,
    /// What nothing references, by family (objects older than a day; a dry
    /// run: nothing is deleted).
    pub orphans: Vec<OrphanGroup>,
    /// False while it runs, or when it stopped at its limits (then the orphans
    /// are partial too).
    pub complete: bool,
    pub error: Option<String>,
}

/// Objects of one family that nothing references.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct OrphanGroup {
    /// `lfs` (no LFS record), `lfs_unused` (a record no project uses), `registry`
    /// (no blob record), `registry_uploads` (no upload in progress), `ci` (no
    /// artifact or log record), `uploads`, `avatars` (of a deleted user, project
    /// or group), `tmp` (interrupted writes), or `unrecognized` (keys Forgust does
    /// not write: listed, never called orphans).
    pub family: String,
    pub objects: i64,
    pub bytes: i64,
    /// A few of its keys.
    pub sample: Vec<String>,
}

pub async fn blob_scan(e: impl PgExecutor<'_>) -> Result<Option<BlobScan>> {
    let r =
        sqlx::query!("SELECT started_at, finished_at, prefixes, orphans, complete, error FROM admin_blob_scans").fetch_optional(e).await?;
    Ok(r.map(|r| BlobScan {
        started_at: r.started_at,
        finished_at: r.finished_at,
        prefixes: serde_json::from_value(r.prefixes).unwrap_or_default(),
        orphans: serde_json::from_value(r.orphans).unwrap_or_default(),
        complete: r.complete,
        error: r.error,
    }))
}

/// A scan starts (false when one is running, started less than an hour ago).
pub async fn start_blob_scan(e: impl PgExecutor<'_>) -> Result<bool> {
    Ok(sqlx::query!(
        "INSERT INTO admin_blob_scans (id, started_at) VALUES (true, now())
         ON CONFLICT (id) DO UPDATE SET started_at = now(), finished_at = NULL, prefixes = '[]', orphans = '[]', complete = false, error = NULL
         WHERE admin_blob_scans.finished_at IS NOT NULL OR admin_blob_scans.started_at < now() - interval '1 hour'"
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}

pub async fn finish_blob_scan(
    e: impl PgExecutor<'_>,
    prefixes: &[PrefixUsage],
    orphans: &[OrphanGroup],
    complete: bool,
    error: Option<&str>,
) -> Result<()> {
    let v = serde_json::to_value(prefixes).unwrap_or_default();
    let o = serde_json::to_value(orphans).unwrap_or_default();
    sqlx::query!(
        "UPDATE admin_blob_scans SET finished_at = now(), prefixes = $1, orphans = $2, complete = $3, error = $4",
        v,
        o,
        complete,
        error
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Of these blob keys, the ones something accounts for: `queued` (waiting in the
/// deletion queue), `unused` (an LFS object no project uses), or `ok`. A key
/// missing from the answer is referenced by nothing. The rules follow the code
/// that writes and deletes each family (see `blob.rs`, `registry.rs`,
/// `services/avatars.rs`).
pub async fn blob_key_status(e: impl PgExecutor<'_>, keys: &[String]) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query!(
        r#"SELECT k AS "key!",
                  CASE WHEN EXISTS (SELECT 1 FROM blob_deletions d WHERE d.blob_key = k) THEN 'queued'
                       WHEN k LIKE 'lfs/%' AND NOT EXISTS (SELECT 1 FROM lfs_objects_projects l WHERE l.oid = split_part(k, '/', 4)) THEN 'unused'
                       ELSE 'ok' END AS "status!"
             FROM unnest($1::text[]) AS k
            WHERE EXISTS (SELECT 1 FROM blob_deletions d WHERE d.blob_key = k)
               OR (k LIKE 'ci/%' AND EXISTS (SELECT 1 FROM ci_job_artifacts a WHERE a.blob_key = k))
               OR (k LIKE 'uploads/%' AND EXISTS (SELECT 1 FROM uploads u WHERE u.blob_key = k))
               OR (k LIKE 'lfs/%' AND EXISTS (SELECT 1 FROM lfs_objects o WHERE o.oid = split_part(k, '/', 4)))
               OR (k LIKE 'registry/blobs/%'
                   AND EXISTS (SELECT 1 FROM registry_blobs b WHERE b.digest = split_part(k, '/', 3) || ':' || split_part(k, '/', 4)))
               OR (k LIKE 'registry/uploads/%' AND EXISTS (SELECT 1 FROM registry_uploads r WHERE r.id::text = split_part(k, '/', 3)))
               OR (k ~ '^avatars/user/[0-9]+\.png$'
                   AND EXISTS (SELECT 1 FROM users x WHERE x.id = substring(k FROM '([0-9]+)')::bigint AND x.avatar_at IS NOT NULL))
               OR (k ~ '^avatars/project/[0-9]+\.png$'
                   AND EXISTS (SELECT 1 FROM projects x WHERE x.id = substring(k FROM '([0-9]+)')::bigint AND x.avatar_at IS NOT NULL))
               OR (k ~ '^avatars/group/[0-9]+\.png$'
                   AND EXISTS (SELECT 1 FROM namespaces x WHERE x.id = substring(k FROM '([0-9]+)')::bigint AND x.avatar_at IS NOT NULL))"#,
        keys
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.key, r.status)).collect())
}

// ------------------------------------------------------------ users

/// A person as the admin user list shows them.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
    pub name: String,
    pub email: String,
    pub user_type: String,
    pub is_admin: bool,
    pub is_external: bool,
    /// `active` or `blocked`.
    pub state: String,
    pub two_factor_enabled: bool,
    pub email_verified: bool,
    pub created_at: DateTime<Utc>,
    pub last_sign_in_at: Option<DateTime<Utc>>,
    /// Sessions not expired.
    pub sessions: i64,
    /// Personal access tokens not revoked or expired.
    pub tokens: i64,
}

/// `filter`: `all` (people), `admins`, `blocked`, `external`, `unconfirmed`, `two_factor_off` or `bots`.
pub async fn users(e: impl PgExecutor<'_>, search: &str, filter: &str, limit: i64, offset: i64) -> Result<Vec<AdminUser>> {
    sqlx::query_as!(
        AdminUser,
        r#"SELECT u.id, u.username, u.name, u.email, u.user_type, u.is_admin, u.is_external, u.state,
                  u.totp_enabled AS two_factor_enabled, (u.email_verified_at IS NOT NULL) AS "email_verified!",
                  u.created_at, u.last_sign_in_at,
                  (SELECT count(*) FROM sessions s WHERE s.user_id = u.id AND s.expires_at > now()) AS "sessions!",
                  (SELECT count(*) FROM personal_access_tokens t WHERE t.user_id = u.id AND NOT t.revoked
                     AND (t.expires_at IS NULL OR t.expires_at > now())) AS "tokens!"
             FROM users u
            WHERE ($1 = '' OR u.username ILIKE '%' || $1 || '%' OR u.name ILIKE '%' || $1 || '%' OR u.email ILIKE '%' || $1 || '%')
              AND CASE $2
                    WHEN 'bots' THEN u.user_type <> 'human'
                    WHEN 'admins' THEN u.user_type = 'human' AND u.is_admin
                    WHEN 'blocked' THEN u.user_type = 'human' AND u.state = 'blocked'
                    WHEN 'external' THEN u.user_type = 'human' AND u.is_external
                    WHEN 'unconfirmed' THEN u.user_type = 'human' AND u.email_verified_at IS NULL
                    WHEN 'two_factor_off' THEN u.user_type = 'human' AND NOT u.totp_enabled
                    ELSE u.user_type = 'human' END
            ORDER BY u.username LIMIT $3 OFFSET $4"#,
        search,
        filter,
        limit,
        offset
    )
    .fetch_all(e)
    .await
}

/// Groups that would have no Owner left without this user: they are its only
/// direct Owner, and no ancestor group has another (paths).
pub async fn sole_owned_groups(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<String>> {
    sqlx::query_scalar!(
        r#"SELECT r.path AS "path!" FROM members m
             JOIN namespaces n ON n.id = m.source_id AND n.kind = 'group'
             JOIN routes r ON r.source_type = 'namespace' AND r.source_id = n.id
            WHERE m.source_type = 'namespace' AND m.user_id = $1 AND m.access_level >= 50
              AND NOT EXISTS (SELECT 1 FROM members o WHERE o.source_type = 'namespace' AND o.source_id = ANY(n.traversal_ids)
                                AND o.user_id <> $1 AND o.access_level >= 50)
            ORDER BY 1"#,
        user_id
    )
    .fetch_all(e)
    .await
}

/// Projects in a user's personal namespace: (id, path).
pub async fn personal_projects(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query!(
        r#"SELECT p.id, r.path AS "path!" FROM projects p
             JOIN namespaces n ON n.id = p.namespace_id AND n.kind = 'user' AND n.owner_id = $1
             JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
            ORDER BY 2"#,
        user_id
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.id, r.path))
    .collect())
}

pub async fn active_admins(e: impl PgExecutor<'_>) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM users WHERE is_admin AND state = 'active' AND user_type = 'human'"#)
        .fetch_one(e)
        .await
}

/// Delete a person (in the caller's transaction, once their personal projects
/// are gone): their schedules stop, their personal namespace and its route go;
/// what they wrote stays, without its author.
pub async fn delete_user(conn: &mut sqlx::PgConnection, user_id: i64) -> Result<()> {
    sqlx::query!("UPDATE ci_pipeline_schedules SET active = false WHERE owner_id = $1", user_id).execute(&mut *conn).await?;
    let personal: Vec<i64> =
        sqlx::query_scalar!("SELECT id FROM namespaces WHERE kind = 'user' AND owner_id = $1", user_id).fetch_all(&mut *conn).await?;
    for ns in personal {
        crate::routes::delete_for(&mut *conn, "namespace", ns).await?;
        sqlx::query!("DELETE FROM namespaces WHERE id = $1", ns).execute(&mut *conn).await?;
    }
    sqlx::query!("DELETE FROM users WHERE id = $1", user_id).execute(&mut *conn).await?;
    Ok(())
}

pub async fn record_sign_in(e: impl PgExecutor<'_>, user_id: i64) -> Result<()> {
    sqlx::query!("UPDATE users SET last_sign_in_at = now() WHERE id = $1", user_id).execute(e).await?;
    Ok(())
}

pub async fn set_external(e: impl PgExecutor<'_>, user_id: i64, external: bool) -> Result<()> {
    sqlx::query!("UPDATE users SET is_external = $2, updated_at = now() WHERE id = $1", user_id, external).execute(e).await?;
    Ok(())
}

/// Revoke every personal access token of a user; how many were live.
pub async fn revoke_personal_tokens(e: impl PgExecutor<'_>, user_id: i64) -> Result<u64> {
    Ok(sqlx::query!("UPDATE personal_access_tokens SET revoked = true WHERE user_id = $1 AND NOT revoked", user_id)
        .execute(e)
        .await?
        .rows_affected())
}

// ------------------------------------------------------------ banner

#[derive(Debug, Clone, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct Banner {
    /// Plain text (no Markdown, no HTML); empty: no banner.
    pub text: String,
    /// `info` or `warn`.
    pub tone: String,
    /// Shown until then (None: until removed).
    pub until: Option<DateTime<Utc>>,
}

pub async fn banner(e: impl PgExecutor<'_>) -> Result<Banner> {
    sqlx::query_as!(Banner, r#"SELECT banner_text AS text, banner_tone AS tone, banner_until AS until FROM instance_settings"#)
        .fetch_one(e)
        .await
}

pub async fn set_banner(e: impl PgExecutor<'_>, b: &Banner) -> Result<()> {
    sqlx::query!(
        "UPDATE instance_settings SET banner_text = $1, banner_tone = $2, banner_until = $3, updated_at = now()",
        b.text,
        b.tone,
        b.until
    )
    .execute(e)
    .await?;
    Ok(())
}

// ------------------------------------------------------------ job queue

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct TaskQueue {
    pub task: String,
    /// Waiting to run (now or later).
    pub queued: i64,
    /// Locked by a worker.
    pub running: i64,
    /// Out of attempts: they stay until retried or dropped.
    pub failed: i64,
    /// Failed at least once, will try again.
    pub retrying: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FailedJob {
    pub id: i64,
    pub task: String,
    pub attempts: i16,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The first line of the last error, shortened, with credentials in URLs hidden.
    pub error: String,
}

/// The background job queue per task. The graphile_worker schema is made at
/// worker start, so these queries are checked at run time.
pub async fn job_queues(e: impl PgExecutor<'_>) -> Result<Vec<TaskQueue>> {
    let rows = sqlx::query(
        "SELECT task_identifier,
                count(*) FILTER (WHERE locked_at IS NULL AND attempts < max_attempts) AS queued,
                count(*) FILTER (WHERE locked_at IS NOT NULL) AS running,
                count(*) FILTER (WHERE attempts >= max_attempts) AS failed,
                count(*) FILTER (WHERE attempts > 0 AND attempts < max_attempts) AS retrying
           FROM graphile_worker.jobs GROUP BY task_identifier ORDER BY task_identifier",
    )
    .fetch_all(e)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(TaskQueue {
                task: r.try_get("task_identifier")?,
                queued: r.try_get("queued")?,
                running: r.try_get("running")?,
                failed: r.try_get("failed")?,
                retrying: r.try_get("retrying")?,
            })
        })
        .collect()
}

pub async fn failed_jobs(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<FailedJob>> {
    let rows = sqlx::query(
        "SELECT id, task_identifier, attempts, created_at, updated_at, coalesce(last_error, '') AS last_error
           FROM graphile_worker.jobs WHERE attempts >= max_attempts ORDER BY updated_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(e)
    .await?;
    rows.into_iter()
        .map(|r| {
            let err: String = r.try_get("last_error")?;
            Ok(FailedJob {
                id: r.try_get("id")?,
                task: r.try_get("task_identifier")?,
                attempts: r.try_get("attempts")?,
                created_at: r.try_get("created_at")?,
                updated_at: r.try_get("updated_at")?,
                error: redact_error(&err),
            })
        })
        .collect()
}

/// Run a failed job again now, with fresh attempts (not one a worker holds).
pub async fn retry_job(e: impl PgExecutor<'_>, id: i64) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "WITH target AS (SELECT id FROM graphile_worker.jobs WHERE id = $1 AND locked_at IS NULL AND attempts >= max_attempts)
         SELECT count(*) FROM graphile_worker.reschedule_jobs(ARRAY(SELECT id FROM target), run_at => now(), attempts => 0)",
    )
    .bind(id)
    .fetch_one(e)
    .await?;
    Ok(n > 0)
}

/// Drop a failed job (not one a worker holds).
pub async fn drop_job(e: impl PgExecutor<'_>, id: i64) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "WITH target AS (SELECT id FROM graphile_worker.jobs WHERE id = $1 AND locked_at IS NULL AND attempts >= max_attempts)
         SELECT count(*) FROM graphile_worker.complete_jobs(ARRAY(SELECT id FROM target))",
    )
    .bind(id)
    .fetch_one(e)
    .await?;
    Ok(n > 0)
}

/// A job error fit to show: its first line, at most 240 characters, without
/// `user:password@` in URLs.
pub fn redact_error(err: &str) -> String {
    let line = err.lines().next().unwrap_or_default();
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail.find(|c: char| c.is_whitespace() || c == '/' || c == '\'' || c == '"').unwrap_or(tail.len());
        let host = &tail[..end];
        match host.rfind('@') {
            Some(at) => {
                out.push_str("***@");
                out.push_str(&host[at + 1..]);
            }
            None => out.push_str(host),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    if out.chars().count() > 240 { format!("{}…", out.chars().take(240).collect::<String>()) } else { out }
}

#[cfg(test)]
mod tests {
    use super::redact_error;

    #[test]
    fn errors_hide_credentials() {
        assert_eq!(
            redact_error("git fetch: fatal: unable to access 'https://oauth2:glpat-x@gitlab.com/a/b.git/'\nmore"),
            "git fetch: fatal: unable to access 'https://***@gitlab.com/a/b.git/'"
        );
        assert_eq!(redact_error("plain error"), "plain error");
        assert_eq!(redact_error("see http://example.com/x"), "see http://example.com/x");
    }
}
