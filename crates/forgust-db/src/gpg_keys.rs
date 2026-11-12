//! OpenPGP public keys of users, for verifying signed commits.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GpgKey {
    pub id: i64,
    pub user_id: i64,
    /// The armored public key, as uploaded.
    pub armored: String,
    pub fingerprint: String,
    pub key_ids: Vec<String>,
    pub emails: Vec<String>,
    pub created_at: DateTime<Utc>,
}

pub async fn insert(
    e: impl PgExecutor<'_>,
    user_id: i64,
    armored: &str,
    fingerprint: &str,
    key_ids: &[String],
    emails: &[String],
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO gpg_keys (user_id, armored, fingerprint, key_ids, emails) VALUES ($1, $2, $3, $4, $5) RETURNING id",
        user_id,
        armored,
        fingerprint,
        key_ids,
        emails
    )
    .fetch_one(e)
    .await
}

pub async fn list_for_user(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<GpgKey>> {
    sqlx::query_as!(
        GpgKey,
        "SELECT id, user_id, armored, fingerprint, key_ids, emails, created_at FROM gpg_keys WHERE user_id = $1 ORDER BY created_at DESC",
        user_id
    )
    .fetch_all(e)
    .await
}

/// The key that has `key_id` (16 upper-case hex digits) as its primary key or a subkey.
pub async fn by_key_id(e: impl PgExecutor<'_>, key_id: &str) -> Result<Option<GpgKey>> {
    sqlx::query_as!(
        GpgKey,
        "SELECT id, user_id, armored, fingerprint, key_ids, emails, created_at FROM gpg_keys WHERE key_ids @> ARRAY[$1::text]",
        key_id
    )
    .fetch_optional(e)
    .await
}

pub async fn delete_for_user(e: impl PgExecutor<'_>, user_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM gpg_keys WHERE id = $1 AND user_id = $2", id, user_id).execute(e).await?.rows_affected() > 0)
}
