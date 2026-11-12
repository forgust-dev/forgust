//! Releases on tags, their asset links, and project uploads (M7).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Release {
    pub id: i64,
    pub project_id: i64,
    pub tag_name: String,
    pub name: String,
    pub description: String,
    pub author_id: Option<i64>,
    pub author_username: Option<String>,
    pub author_name: Option<String>,
    pub released_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ReleaseLink {
    pub id: i64,
    pub release_id: i64,
    pub name: String,
    pub url: String,
    pub direct_asset_path: Option<String>,
    /// other, runbook, image or package
    pub link_type: String,
    pub created_at: DateTime<Utc>,
}

/// Newest first (by release date).
pub async fn list(e: impl PgExecutor<'_>, project_id: i64, limit: i64, offset: i64) -> Result<Vec<Release>> {
    sqlx::query_as!(
        Release,
        r#"SELECT r.id, r.project_id, r.tag_name, r.name, r.description, r.author_id, u.username AS "author_username?",
                u.name AS "author_name?", r.released_at, r.created_at, r.updated_at
           FROM releases r LEFT JOIN users u ON u.id = r.author_id
          WHERE r.project_id = $1 ORDER BY r.released_at DESC, r.id DESC LIMIT $2 OFFSET $3"#,
        project_id,
        limit,
        offset
    )
    .fetch_all(e)
    .await
}

pub async fn count(e: impl PgExecutor<'_>, project_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar!("SELECT count(*) FROM releases WHERE project_id = $1", project_id).fetch_one(e).await?.unwrap_or(0))
}

pub async fn find(e: impl PgExecutor<'_>, project_id: i64, tag_name: &str) -> Result<Option<Release>> {
    sqlx::query_as!(
        Release,
        r#"SELECT r.id, r.project_id, r.tag_name, r.name, r.description, r.author_id, u.username AS "author_username?",
                u.name AS "author_name?", r.released_at, r.created_at, r.updated_at
           FROM releases r LEFT JOIN users u ON u.id = r.author_id
          WHERE r.project_id = $1 AND r.tag_name = $2"#,
        project_id,
        tag_name
    )
    .fetch_optional(e)
    .await
}

/// Releases of these tags (to show them on the tags page).
pub async fn for_tags(e: impl PgExecutor<'_>, project_id: i64, tags: &[String]) -> Result<Vec<Release>> {
    sqlx::query_as!(
        Release,
        r#"SELECT r.id, r.project_id, r.tag_name, r.name, r.description, r.author_id, u.username AS "author_username?",
                u.name AS "author_name?", r.released_at, r.created_at, r.updated_at
           FROM releases r LEFT JOIN users u ON u.id = r.author_id
          WHERE r.project_id = $1 AND r.tag_name = ANY($2)"#,
        project_id,
        tags
    )
    .fetch_all(e)
    .await
}

/// Delete the releases of deleted tags; returns them (for webhooks).
pub async fn delete_for_tags(e: impl PgExecutor<'_>, project_id: i64, tags: &[String]) -> Result<Vec<String>> {
    sqlx::query_scalar!("DELETE FROM releases WHERE project_id = $1 AND tag_name = ANY($2) RETURNING tag_name", project_id, tags)
        .fetch_all(e)
        .await
}

/// `None` when the tag already has a release.
pub async fn create(
    e: impl PgExecutor<'_>,
    project_id: i64,
    tag_name: &str,
    name: &str,
    description: &str,
    author_id: Option<i64>,
    released_at: Option<DateTime<Utc>>,
) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        "INSERT INTO releases (project_id, tag_name, name, description, author_id, released_at)
         VALUES ($1, $2, $3, $4, $5, coalesce($6, now()))
         ON CONFLICT (project_id, tag_name) DO NOTHING RETURNING id",
        project_id,
        tag_name,
        name,
        description,
        author_id,
        released_at
    )
    .fetch_optional(e)
    .await
}

pub async fn update(e: impl PgExecutor<'_>, id: i64, name: &str, description: &str, released_at: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        "UPDATE releases SET name = $2, description = $3, released_at = $4, updated_at = now() WHERE id = $1",
        id,
        name,
        description,
        released_at
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM releases WHERE id = $1", id).execute(e).await?;
    Ok(())
}

pub async fn links(e: impl PgExecutor<'_>, release_ids: &[i64]) -> Result<Vec<ReleaseLink>> {
    sqlx::query_as!(
        ReleaseLink,
        "SELECT id, release_id, name, url, direct_asset_path, link_type, created_at FROM release_links
          WHERE release_id = ANY($1) ORDER BY release_id, id",
        release_ids
    )
    .fetch_all(e)
    .await
}

pub async fn link_by_path(e: impl PgExecutor<'_>, release_id: i64, path: &str) -> Result<Option<ReleaseLink>> {
    sqlx::query_as!(
        ReleaseLink,
        "SELECT id, release_id, name, url, direct_asset_path, link_type, created_at FROM release_links
          WHERE release_id = $1 AND direct_asset_path = $2",
        release_id,
        path
    )
    .fetch_optional(e)
    .await
}

/// `None` when the name or path is taken in this release.
pub async fn add_link(
    e: impl PgExecutor<'_>,
    release_id: i64,
    name: &str,
    url: &str,
    direct_asset_path: Option<&str>,
    link_type: &str,
) -> Result<Option<ReleaseLink>> {
    sqlx::query_as!(
        ReleaseLink,
        "INSERT INTO release_links (release_id, name, url, direct_asset_path, link_type) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT DO NOTHING
         RETURNING id, release_id, name, url, direct_asset_path, link_type, created_at",
        release_id,
        name,
        url,
        direct_asset_path,
        link_type
    )
    .fetch_optional(e)
    .await
}

pub async fn delete_link(e: impl PgExecutor<'_>, release_id: i64, link_id: i64) -> Result<Option<ReleaseLink>> {
    sqlx::query_as!(
        ReleaseLink,
        "DELETE FROM release_links WHERE release_id = $1 AND id = $2
         RETURNING id, release_id, name, url, direct_asset_path, link_type, created_at",
        release_id,
        link_id
    )
    .fetch_optional(e)
    .await
}

// ---------------------------------------------------------------- uploads

#[derive(Debug, Clone, Serialize)]
pub struct Upload {
    pub id: i64,
    pub project_id: i64,
    pub secret: String,
    pub filename: String,
    pub size: i64,
    pub content_type: String,
    pub blob_key: String,
    pub uploaded_by: Option<i64>,
    pub created_at: DateTime<Utc>,
}

pub struct NewUpload<'a> {
    pub project_id: i64,
    pub secret: &'a str,
    pub filename: &'a str,
    pub size: i64,
    pub content_type: &'a str,
    pub blob_key: &'a str,
    pub uploaded_by: Option<i64>,
}

pub async fn add_upload(e: impl PgExecutor<'_>, u: NewUpload<'_>) -> Result<Upload> {
    sqlx::query_as!(
        Upload,
        "INSERT INTO uploads (project_id, secret, filename, size, content_type, blob_key, uploaded_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id, project_id, secret, filename, size, content_type, blob_key, uploaded_by, created_at",
        u.project_id,
        u.secret,
        u.filename,
        u.size,
        u.content_type,
        u.blob_key,
        u.uploaded_by
    )
    .fetch_one(e)
    .await
}

pub async fn upload(e: impl PgExecutor<'_>, project_id: i64, secret: &str) -> Result<Option<Upload>> {
    sqlx::query_as!(
        Upload,
        "SELECT id, project_id, secret, filename, size, content_type, blob_key, uploaded_by, created_at
           FROM uploads WHERE project_id = $1 AND secret = $2",
        project_id,
        secret
    )
    .fetch_optional(e)
    .await
}

/// Blob keys whose rows were deleted, taken off the queue (the caller deletes them).
pub async fn take_blob_deletions(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<String>> {
    sqlx::query_scalar!(
        "DELETE FROM blob_deletions WHERE id IN (SELECT id FROM blob_deletions ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED)
         RETURNING blob_key",
        limit
    )
    .fetch_all(e)
    .await
}

/// Uploads by this person in the last hour (any project).
pub async fn recent_uploads_by(e: impl PgExecutor<'_>, user_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar!("SELECT count(*) FROM uploads WHERE uploaded_by = $1 AND created_at > now() - interval '1 hour'", user_id)
        .fetch_one(e)
        .await?
        .unwrap_or(0))
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StorageUse {
    pub uploads_bytes: i64,
    pub uploads_count: i64,
    pub artifacts_bytes: i64,
    pub lfs_bytes: i64,
    /// Container images: the distinct blobs its repositories use.
    pub registry_bytes: i64,
}

/// What a project keeps in the blob store.
pub async fn storage_use(e: impl PgExecutor<'_>, project_id: i64) -> Result<StorageUse> {
    let r = sqlx::query!(
        r#"SELECT (SELECT coalesce(sum(size), 0) FROM uploads WHERE project_id = $1)::bigint AS "uploads_bytes!",
                  (SELECT count(*) FROM uploads WHERE project_id = $1) AS "uploads_count!",
                  (SELECT coalesce(sum(size), 0) FROM ci_job_artifacts WHERE project_id = $1)::bigint AS "artifacts_bytes!",
                  (SELECT coalesce(sum(o.size), 0) FROM lfs_objects_projects lp JOIN lfs_objects o ON o.oid = lp.oid
                    WHERE lp.project_id = $1)::bigint AS "lfs_bytes!",
                  (SELECT coalesce(sum(b.size), 0) FROM registry_blobs b
                    WHERE b.digest IN (SELECT l.digest FROM registry_repository_blobs l
                                         JOIN registry_repositories r ON r.id = l.repository_id WHERE r.project_id = $1))::bigint AS "registry_bytes!""#,
        project_id
    )
    .fetch_one(e)
    .await?;
    Ok(StorageUse {
        uploads_bytes: r.uploads_bytes,
        uploads_count: r.uploads_count,
        artifacts_bytes: r.artifacts_bytes,
        lfs_bytes: r.lfs_bytes,
        registry_bytes: r.registry_bytes,
    })
}

/// The tag of the newest release (`~latest` of a CI/CD component).
pub async fn latest_tag(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT tag_name FROM releases WHERE project_id = $1 AND released_at <= now() ORDER BY released_at DESC, id DESC LIMIT 1",
        project_id
    )
    .fetch_optional(e)
    .await
}
