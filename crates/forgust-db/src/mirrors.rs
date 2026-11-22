//! Trivy's databases, mirrored (`vulnerability_db_mirrors`): the manifest of
//! each as it came, and the blobs it names.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Mirror {
    pub name: String,
    pub tag: String,
    pub media_type: String,
    pub manifest: Vec<u8>,
    pub digest: String,
    pub blob_digests: Vec<String>,
    pub previous_blob_digests: Vec<String>,
    pub size: i64,
    pub origin: String,
    pub stored_at: DateTime<Utc>,
}

/// What admins see of a mirrored database.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MirrorStatus {
    pub name: String,
    pub tag: String,
    pub digest: String,
    pub size: i64,
    /// The upstream reference it came from, or `file`.
    pub origin: String,
    pub stored_at: DateTime<Utc>,
}

pub async fn get(e: impl PgExecutor<'_>, name: &str) -> Result<Option<Mirror>> {
    sqlx::query_as!(
        Mirror,
        "SELECT name, tag, media_type, manifest, digest, blob_digests, previous_blob_digests, size, origin, stored_at
           FROM vulnerability_db_mirrors WHERE name = $1",
        name
    )
    .fetch_optional(e)
    .await
}

pub async fn all(e: impl PgExecutor<'_>) -> Result<Vec<MirrorStatus>> {
    sqlx::query_as!(MirrorStatus, "SELECT name, tag, digest, size, origin, stored_at FROM vulnerability_db_mirrors ORDER BY name")
        .fetch_all(e)
        .await
}

pub struct NewCopy<'a> {
    pub name: &'a str,
    pub tag: &'a str,
    pub media_type: &'a str,
    pub manifest: &'a [u8],
    pub digest: &'a str,
    pub blob_digests: &'a [String],
    pub size: i64,
    pub origin: &'a str,
}

/// Swap in a new copy; the one it replaces keeps its blobs until the next
/// swap. The blobs no copy names any more are queued for deletion
/// (`blob_deletions`, under `blob_key`).
pub async fn swap(c: &mut sqlx::PgConnection, n: NewCopy<'_>, blob_key: impl Fn(&str) -> String) -> Result<()> {
    let old = sqlx::query!("SELECT blob_digests, previous_blob_digests FROM vulnerability_db_mirrors WHERE name = $1 FOR UPDATE", n.name)
        .fetch_optional(&mut *c)
        .await?;
    let (kept, dropped) = match old {
        Some(o) => (o.blob_digests, o.previous_blob_digests),
        None => (vec![], vec![]),
    };
    sqlx::query!(
        "INSERT INTO vulnerability_db_mirrors (name, tag, media_type, manifest, digest, blob_digests, previous_blob_digests, size, origin, stored_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
         ON CONFLICT (name) DO UPDATE SET tag = EXCLUDED.tag, media_type = EXCLUDED.media_type, manifest = EXCLUDED.manifest,
             digest = EXCLUDED.digest, blob_digests = EXCLUDED.blob_digests, previous_blob_digests = EXCLUDED.previous_blob_digests,
             size = EXCLUDED.size, origin = EXCLUDED.origin, stored_at = now()",
        n.name,
        n.tag,
        n.media_type,
        n.manifest,
        n.digest,
        n.blob_digests,
        &kept,
        n.size,
        n.origin
    )
    .execute(&mut *c)
    .await?;
    // blobs are shared by digest: one still named by any copy stays
    let unused: Vec<String> = sqlx::query_scalar!(
        r#"SELECT d AS "d!" FROM unnest($1::text[]) AS d
           EXCEPT SELECT unnest(blob_digests || previous_blob_digests) FROM vulnerability_db_mirrors"#,
        &dropped
    )
    .fetch_all(&mut *c)
    .await?;
    let keys: Vec<String> = unused.iter().map(|d| blob_key(d)).collect();
    sqlx::query!("INSERT INTO blob_deletions (blob_key) SELECT unnest($1::text[])", &keys).execute(&mut *c).await?;
    Ok(())
}

/// A copy names this blob (one queued for deletion and named again stays).
pub async fn names_blob(e: impl PgExecutor<'_>, digest: &str) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM vulnerability_db_mirrors WHERE $1 = ANY (blob_digests || previous_blob_digests)) AS "e!""#,
        digest
    )
    .fetch_one(e)
    .await
}
