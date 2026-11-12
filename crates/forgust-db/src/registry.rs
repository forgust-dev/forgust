//! Container registry rows (ADR-0022): repositories, blobs linked to them,
//! manifests, tags, uploads in progress and cleanup policies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Repository {
    pub id: i64,
    pub project_id: i64,
    /// Below the project's path: `""` for the project's own image.
    pub name: String,
    pub created_at: DateTime<Utc>,
}

pub async fn repository(e: impl PgExecutor<'_>, project_id: i64, name: &str) -> Result<Option<Repository>> {
    sqlx::query_as!(
        Repository,
        "SELECT id, project_id, name, created_at FROM registry_repositories WHERE project_id = $1 AND name = $2",
        project_id,
        name
    )
    .fetch_optional(e)
    .await
}

pub async fn repository_by_id(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Repository>> {
    sqlx::query_as!(Repository, "SELECT id, project_id, name, created_at FROM registry_repositories WHERE id = $1", id)
        .fetch_optional(e)
        .await
}

/// The repository, created on first push.
pub async fn ensure_repository(e: impl PgExecutor<'_>, project_id: i64, name: &str) -> Result<Repository> {
    sqlx::query_as!(
        Repository,
        "INSERT INTO registry_repositories (project_id, name) VALUES ($1, $2)
         ON CONFLICT (project_id, name) DO UPDATE SET name = EXCLUDED.name
         RETURNING id, project_id, name, created_at",
        project_id,
        name
    )
    .fetch_one(e)
    .await
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct RepositorySummary {
    pub id: i64,
    pub name: String,
    pub tags_count: i64,
    /// Bytes of the distinct blobs its manifests use.
    pub size: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
}

pub async fn repositories(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<RepositorySummary>> {
    sqlx::query_as!(
        RepositorySummary,
        r#"SELECT r.id, r.name, r.created_at,
                  (SELECT count(*) FROM registry_tags t WHERE t.repository_id = r.id) AS "tags_count!",
                  (SELECT COALESCE(sum(b.size), 0)::bigint FROM registry_blobs b
                    WHERE b.digest IN (SELECT unnest(m.blob_digests) FROM registry_manifests m WHERE m.repository_id = r.id)) AS "size!",
                  (SELECT max(t.updated_at) FROM registry_tags t WHERE t.repository_id = r.id) AS updated_at
             FROM registry_repositories r WHERE r.project_id = $1 ORDER BY r.name"#,
        project_id
    )
    .fetch_all(e)
    .await
}

/// Every repository of every project (for the catalog), after `after` (a
/// full name), at most `limit`; the caller filters by access.
pub async fn all_repositories(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<(Repository, String)>> {
    let rows = sqlx::query!(
        "SELECT r.id, r.project_id, r.name, r.created_at, rt.path AS full_path FROM registry_repositories r
           JOIN routes rt ON rt.source_type = 'project' AND rt.source_id = r.project_id
          ORDER BY rt.path, r.name LIMIT $1",
        limit
    )
    .fetch_all(e)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (Repository { id: r.id, project_id: r.project_id, name: r.name, created_at: r.created_at }, r.full_path))
        .collect())
}

pub async fn delete_repository(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM registry_repositories WHERE id = $1", id).execute(e).await?;
    Ok(())
}

// ------------------------------------------------------------------ blobs

/// Size of a blob linked to the repository.
pub async fn linked_blob(e: impl PgExecutor<'_>, repository_id: i64, digest: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        "SELECT b.size FROM registry_blobs b JOIN registry_repository_blobs l ON l.digest = b.digest
          WHERE l.repository_id = $1 AND b.digest = $2",
        repository_id,
        digest
    )
    .fetch_optional(e)
    .await
}

pub async fn blob_exists(e: impl PgExecutor<'_>, digest: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!("SELECT size FROM registry_blobs WHERE digest = $1", digest).fetch_optional(e).await
}

/// Record a stored blob (if new) and link it to the repository.
pub async fn add_blob(c: &mut PgConnection, repository_id: i64, digest: &str, size: i64) -> Result<()> {
    sqlx::query!("INSERT INTO registry_blobs (digest, size) VALUES ($1, $2) ON CONFLICT (digest) DO NOTHING", digest, size)
        .execute(&mut *c)
        .await?;
    link_blob(&mut *c, repository_id, digest).await
}

pub async fn link_blob(e: impl PgExecutor<'_>, repository_id: i64, digest: &str) -> Result<()> {
    sqlx::query!(
        "INSERT INTO registry_repository_blobs (repository_id, digest) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        repository_id,
        digest
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn unlink_blob(e: impl PgExecutor<'_>, repository_id: i64, digest: &str) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM registry_repository_blobs WHERE repository_id = $1 AND digest = $2", repository_id, digest)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

/// Of `digests`, those linked to the repository.
pub async fn linked_of(e: impl PgExecutor<'_>, repository_id: i64, digests: &[String]) -> Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT digest FROM registry_repository_blobs WHERE repository_id = $1 AND digest = ANY($2)",
        repository_id,
        digests
    )
    .fetch_all(e)
    .await
}

pub async fn blob_sizes(e: impl PgExecutor<'_>, digests: &[String]) -> Result<i64> {
    Ok(sqlx::query_scalar!("SELECT COALESCE(sum(size), 0)::bigint FROM registry_blobs WHERE digest = ANY($1)", digests)
        .fetch_one(e)
        .await?
        .unwrap_or(0))
}

// ------------------------------------------------------------------ manifests and tags

#[derive(Debug, Clone)]
pub struct Manifest {
    pub id: i64,
    pub repository_id: i64,
    pub digest: String,
    pub media_type: String,
    pub payload: Vec<u8>,
    pub blob_digests: Vec<String>,
    pub manifest_digests: Vec<String>,
    pub total_size: i64,
    pub subject: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub struct NewManifest<'a> {
    pub repository_id: i64,
    pub digest: &'a str,
    pub media_type: &'a str,
    pub payload: &'a [u8],
    pub blob_digests: &'a [String],
    pub manifest_digests: &'a [String],
    pub total_size: i64,
    pub subject: Option<&'a str>,
}

pub async fn put_manifest(e: impl PgExecutor<'_>, m: NewManifest<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO registry_manifests (repository_id, digest, media_type, payload, blob_digests, manifest_digests, total_size, subject)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (repository_id, digest) DO UPDATE SET media_type = EXCLUDED.media_type
         RETURNING id",
        m.repository_id,
        m.digest,
        m.media_type,
        m.payload,
        m.blob_digests,
        m.manifest_digests,
        m.total_size,
        m.subject
    )
    .fetch_one(e)
    .await
}

pub async fn manifest(e: impl PgExecutor<'_>, repository_id: i64, digest: &str) -> Result<Option<Manifest>> {
    sqlx::query_as!(
        Manifest,
        "SELECT id, repository_id, digest, media_type, payload, blob_digests, manifest_digests, total_size, subject, created_at
           FROM registry_manifests WHERE repository_id = $1 AND digest = $2",
        repository_id,
        digest
    )
    .fetch_optional(e)
    .await
}

pub async fn manifest_by_tag(e: impl PgExecutor<'_>, repository_id: i64, tag: &str) -> Result<Option<Manifest>> {
    sqlx::query_as!(
        Manifest,
        "SELECT m.id, m.repository_id, m.digest, m.media_type, m.payload, m.blob_digests, m.manifest_digests, m.total_size, m.subject, m.created_at
           FROM registry_tags t JOIN registry_manifests m ON m.id = t.manifest_id WHERE t.repository_id = $1 AND t.name = $2",
        repository_id,
        tag
    )
    .fetch_optional(e)
    .await
}

/// Of `digests`, the manifests the repository has.
pub async fn manifests_of(e: impl PgExecutor<'_>, repository_id: i64, digests: &[String]) -> Result<Vec<(String, i64)>> {
    let rows = sqlx::query!(
        "SELECT digest, total_size FROM registry_manifests WHERE repository_id = $1 AND digest = ANY($2)",
        repository_id,
        digests
    )
    .fetch_all(e)
    .await?;
    Ok(rows.into_iter().map(|r| (r.digest, r.total_size)).collect())
}

/// Manifests whose `subject` is `digest` (OCI referrers).
pub async fn referrers(e: impl PgExecutor<'_>, repository_id: i64, digest: &str) -> Result<Vec<Manifest>> {
    sqlx::query_as!(
        Manifest,
        "SELECT id, repository_id, digest, media_type, payload, blob_digests, manifest_digests, total_size, subject, created_at
           FROM registry_manifests WHERE repository_id = $1 AND subject = $2 ORDER BY id",
        repository_id,
        digest
    )
    .fetch_all(e)
    .await
}

pub async fn delete_manifest(e: impl PgExecutor<'_>, repository_id: i64, digest: &str) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM registry_manifests WHERE repository_id = $1 AND digest = $2", repository_id, digest)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

pub async fn set_tag(e: impl PgExecutor<'_>, repository_id: i64, name: &str, manifest_id: i64) -> Result<()> {
    sqlx::query!(
        "INSERT INTO registry_tags (repository_id, name, manifest_id) VALUES ($1, $2, $3)
         ON CONFLICT (repository_id, name) DO UPDATE SET manifest_id = EXCLUDED.manifest_id, updated_at = now()",
        repository_id,
        name,
        manifest_id
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete_tag(e: impl PgExecutor<'_>, repository_id: i64, name: &str) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM registry_tags WHERE repository_id = $1 AND name = $2", repository_id, name)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Tag {
    pub name: String,
    pub digest: String,
    pub media_type: String,
    pub total_size: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Tags in name order, after `last`, at most `limit`.
/// Each image repository of a project with its most recently pushed tag:
/// `(repository name, tag)`, by repository name.
pub async fn newest_tags(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query!(
        "SELECT r.name AS repo, t.name AS tag FROM (
             SELECT DISTINCT ON (t.repository_id) t.repository_id, t.name FROM registry_tags t
               JOIN registry_repositories r ON r.id = t.repository_id
              WHERE r.project_id = $1 ORDER BY t.repository_id, t.updated_at DESC
         ) t JOIN registry_repositories r ON r.id = t.repository_id ORDER BY r.name",
        project_id
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.repo, r.tag))
    .collect())
}

pub async fn tags(e: impl PgExecutor<'_>, repository_id: i64, last: &str, limit: i64) -> Result<Vec<Tag>> {
    sqlx::query_as!(
        Tag,
        "SELECT t.name, m.digest, m.media_type, m.total_size, m.created_at, t.updated_at
           FROM registry_tags t JOIN registry_manifests m ON m.id = t.manifest_id
          WHERE t.repository_id = $1 AND t.name > $2 ORDER BY t.name LIMIT $3",
        repository_id,
        last,
        limit
    )
    .fetch_all(e)
    .await
}

// ------------------------------------------------------------------ uploads

#[derive(Debug, Clone)]
pub struct Upload {
    pub id: uuid::Uuid,
    pub repository_id: i64,
    pub size: i64,
    pub parts: i32,
    pub single_digest: Option<String>,
}

pub async fn create_upload(e: impl PgExecutor<'_>, id: uuid::Uuid, repository_id: i64) -> Result<()> {
    sqlx::query!("INSERT INTO registry_uploads (id, repository_id) VALUES ($1, $2)", id, repository_id).execute(e).await?;
    Ok(())
}

pub async fn upload(e: impl PgExecutor<'_>, id: uuid::Uuid, repository_id: i64) -> Result<Option<Upload>> {
    sqlx::query_as!(
        Upload,
        "SELECT id, repository_id, size, parts, single_digest FROM registry_uploads WHERE id = $1 AND repository_id = $2",
        id,
        repository_id
    )
    .fetch_optional(e)
    .await
}

/// Record a part appended at `expected_size` (the upload's size before it);
/// false when someone else appended first.
pub async fn add_upload_part(
    e: impl PgExecutor<'_>,
    id: uuid::Uuid,
    expected_size: i64,
    part_size: i64,
    part_digest: &str,
) -> Result<bool> {
    Ok(sqlx::query!(
        "UPDATE registry_uploads SET size = size + $3, parts = parts + 1,
                single_digest = CASE WHEN parts = 0 THEN $4 ELSE NULL END, updated_at = now()
          WHERE id = $1 AND size = $2",
        id,
        expected_size,
        part_size,
        part_digest
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}

pub async fn delete_upload(e: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<()> {
    sqlx::query!("DELETE FROM registry_uploads WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// Uploads untouched for a day: (id, parts) to remove.
pub async fn stale_uploads(e: impl PgExecutor<'_>) -> Result<Vec<(uuid::Uuid, i32)>> {
    let rows =
        sqlx::query!("SELECT id, parts FROM registry_uploads WHERE updated_at < now() - interval '1 day' LIMIT 500").fetch_all(e).await?;
    Ok(rows.into_iter().map(|r| (r.id, r.parts)).collect())
}

// ------------------------------------------------------------------ cleanup and GC

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CleanupPolicy {
    pub enabled: bool,
    /// Keep the newest N tags of each image.
    pub keep_n: Option<i32>,
    /// Remove tags older than this many days.
    pub older_than_days: Option<i32>,
    /// Tags that may be removed (regex, whole name).
    pub name_regex: String,
    /// Tags that are always kept (regex, whole name).
    pub name_regex_keep: String,
}

impl Default for CleanupPolicy {
    fn default() -> Self {
        CleanupPolicy {
            enabled: false,
            keep_n: Some(10),
            older_than_days: Some(90),
            name_regex: ".*".into(),
            name_regex_keep: String::new(),
        }
    }
}

pub async fn cleanup_policy(e: impl PgExecutor<'_>, project_id: i64) -> Result<CleanupPolicy> {
    Ok(sqlx::query_as!(
        CleanupPolicy,
        "SELECT enabled, keep_n, older_than_days, name_regex, name_regex_keep FROM registry_cleanup_policies WHERE project_id = $1",
        project_id
    )
    .fetch_optional(e)
    .await?
    .unwrap_or_default())
}

pub async fn set_cleanup_policy(e: impl PgExecutor<'_>, project_id: i64, p: &CleanupPolicy) -> Result<()> {
    sqlx::query!(
        "INSERT INTO registry_cleanup_policies (project_id, enabled, keep_n, older_than_days, name_regex, name_regex_keep)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (project_id) DO UPDATE SET enabled = $2, keep_n = $3, older_than_days = $4, name_regex = $5, name_regex_keep = $6",
        project_id,
        p.enabled,
        p.keep_n,
        p.older_than_days,
        p.name_regex,
        p.name_regex_keep
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Projects whose cleanup policy is due (on, not run in the last day).
pub async fn due_cleanups(e: impl PgExecutor<'_>) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "UPDATE registry_cleanup_policies SET last_run_at = now()
          WHERE project_id IN (SELECT c.project_id FROM registry_cleanup_policies c JOIN projects p ON p.id = c.project_id
                                WHERE c.enabled AND p.archived_at IS NULL
                                  AND (c.last_run_at IS NULL OR c.last_run_at < now() - interval '1 day') LIMIT 50)
          RETURNING project_id"
    )
    .fetch_all(e)
    .await
}

/// Untagged manifests that no other manifest lists, older than an hour (a
/// multi-arch push writes children before their index): removed by GC.
pub async fn delete_dangling_manifests(e: impl PgExecutor<'_>) -> Result<u64> {
    Ok(sqlx::query!(
        "DELETE FROM registry_manifests m
          WHERE m.created_at < now() - interval '1 hour'
            AND NOT EXISTS (SELECT 1 FROM registry_tags t WHERE t.manifest_id = m.id)
            AND NOT EXISTS (SELECT 1 FROM registry_manifests p WHERE p.repository_id = m.repository_id AND m.digest = ANY(p.manifest_digests))
            AND (m.subject IS NULL OR NOT EXISTS (SELECT 1 FROM registry_manifests s WHERE s.repository_id = m.repository_id AND s.digest = m.subject))"
    )
    .execute(e)
    .await?
    .rows_affected())
}

/// Links no manifest needs any more (older than a day, so pushes in flight
/// keep theirs), then blobs nothing links: their files go through the
/// deletion queue.
pub async fn delete_unreferenced_blobs(c: &mut PgConnection) -> Result<u64> {
    sqlx::query!(
        "DELETE FROM registry_repository_blobs l
          WHERE l.created_at < now() - interval '1 day'
            AND NOT EXISTS (SELECT 1 FROM registry_manifests m WHERE m.repository_id = l.repository_id AND l.digest = ANY(m.blob_digests))"
    )
    .execute(&mut *c)
    .await?;
    Ok(sqlx::query!(
        "DELETE FROM registry_blobs b
          WHERE b.created_at < now() - interval '1 day'
            AND NOT EXISTS (SELECT 1 FROM registry_repository_blobs l WHERE l.digest = b.digest)"
    )
    .execute(&mut *c)
    .await?
    .rows_affected())
}
