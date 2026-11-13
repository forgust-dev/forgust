use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use sqlx::postgres::{PgTypeInfo, PgValueRef, Postgres};

use crate::Result;

/// What an SSH key is for, as GitLab's `usage_type`: signing in over SSH,
/// verifying the commits its owner signs, or both. Deploy keys only sign in
/// (the table's CHECK holds them to it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyUsage {
    Auth,
    Signing,
    AuthAndSigning,
}

impl KeyUsage {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyUsage::Auth => "auth",
            KeyUsage::Signing => "signing",
            KeyUsage::AuthAndSigning => "auth_and_signing",
        }
    }
}

// stored as text: the column's CHECK lists the same values
impl sqlx::Type<Postgres> for KeyUsage {
    fn type_info() -> PgTypeInfo {
        <&str as sqlx::Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <&str as sqlx::Type<Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, Postgres> for KeyUsage {
    fn decode(value: PgValueRef<'r>) -> std::result::Result<Self, sqlx::error::BoxDynError> {
        match <&str as sqlx::Decode<Postgres>>::decode(value)? {
            "auth" => Ok(KeyUsage::Auth),
            "signing" => Ok(KeyUsage::Signing),
            "auth_and_signing" => Ok(KeyUsage::AuthAndSigning),
            other => Err(format!("unknown SSH key usage {other:?}").into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SshKey {
    pub id: i64,
    pub kind: String,
    pub user_id: Option<i64>,
    pub title: String,
    pub public_key: String,
    pub fingerprint: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub usage_type: KeyUsage,
}

impl SshKey {
    /// The key may sign in over SSH.
    pub fn for_auth(&self) -> bool {
        self.usage_type != KeyUsage::Signing
    }

    /// The key verifies the commits its owner signs.
    pub fn for_signing(&self) -> bool {
        self.usage_type != KeyUsage::Auth
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn insert(
    e: impl PgExecutor<'_>,
    kind: &str,
    user_id: Option<i64>,
    title: &str,
    public_key: &str,
    fingerprint: &str,
    expires_at: Option<DateTime<Utc>>,
    usage_type: KeyUsage,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO ssh_keys (kind, user_id, title, public_key, fingerprint, expires_at, usage_type) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        kind,
        user_id,
        title,
        public_key,
        fingerprint,
        expires_at,
        usage_type.as_str()
    )
    .fetch_one(e)
    .await
}

/// A key that has not expired (for signing in, and for adding keys).
pub async fn find_by_fingerprint(e: impl PgExecutor<'_>, fingerprint: &str) -> Result<Option<SshKey>> {
    sqlx::query_as!(
        SshKey,
        r#"SELECT id, kind, user_id, title, public_key, fingerprint, expires_at, last_used_at, created_at, usage_type AS "usage_type: KeyUsage" FROM ssh_keys
         WHERE fingerprint = $1 AND (expires_at IS NULL OR expires_at > now())"#,
        fingerprint
    )
    .fetch_optional(e)
    .await
}

/// A key, expired or not: the commits it signed stay verified once it expires
/// (deleting it is what takes that back).
pub async fn find_signing_key(e: impl PgExecutor<'_>, fingerprint: &str) -> Result<Option<SshKey>> {
    sqlx::query_as!(
        SshKey,
        r#"SELECT id, kind, user_id, title, public_key, fingerprint, expires_at, last_used_at, created_at, usage_type AS "usage_type: KeyUsage" FROM ssh_keys
         WHERE fingerprint = $1"#,
        fingerprint
    )
    .fetch_optional(e)
    .await
}

pub async fn touch(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE ssh_keys SET last_used_at = now() WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
        id
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn list_for_user(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<SshKey>> {
    sqlx::query_as!(
        SshKey,
        r#"SELECT id, kind, user_id, title, public_key, fingerprint, expires_at, last_used_at, created_at, usage_type AS "usage_type: KeyUsage" FROM ssh_keys
         WHERE user_id = $1 ORDER BY created_at DESC"#,
        user_id
    )
    .fetch_all(e)
    .await
}

pub async fn delete_for_user(e: impl PgExecutor<'_>, user_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM ssh_keys WHERE id = $1 AND user_id = $2", id, user_id).execute(e).await?.rows_affected() > 0)
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DeployKey {
    pub id: i64,
    pub title: String,
    pub fingerprint: String,
    pub can_push: bool,
    pub created_at: DateTime<Utc>,
}

pub async fn link_deploy_key(e: impl PgExecutor<'_>, key_id: i64, project_id: i64, can_push: bool) -> Result<()> {
    sqlx::query!(
        "INSERT INTO deploy_keys_projects (deploy_key_id, project_id, can_push) VALUES ($1, $2, $3)
         ON CONFLICT (deploy_key_id, project_id) DO UPDATE SET can_push = EXCLUDED.can_push",
        key_id,
        project_id,
        can_push
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn deploy_keys_for_project(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<DeployKey>> {
    sqlx::query_as!(
        DeployKey,
        "SELECT k.id, k.title, k.fingerprint, l.can_push, k.created_at FROM deploy_keys_projects l
         JOIN ssh_keys k ON k.id = l.deploy_key_id WHERE l.project_id = $1 ORDER BY k.created_at DESC",
        project_id
    )
    .fetch_all(e)
    .await
}

/// `Some(can_push)` when the deploy key is attached to the project.
pub async fn deploy_key_access(e: impl PgExecutor<'_>, key_id: i64, project_id: i64) -> Result<Option<bool>> {
    sqlx::query_scalar!("SELECT can_push FROM deploy_keys_projects WHERE deploy_key_id = $1 AND project_id = $2", key_id, project_id)
        .fetch_optional(e)
        .await
}

pub async fn unlink_deploy_key(e: impl PgExecutor<'_>, key_id: i64, project_id: i64) -> Result<bool> {
    let r = sqlx::query!("DELETE FROM deploy_keys_projects WHERE deploy_key_id = $1 AND project_id = $2", key_id, project_id)
        .execute(e)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn delete_orphan_deploy_key(e: impl PgExecutor<'_>, key_id: i64) -> Result<()> {
    sqlx::query!(
        "DELETE FROM ssh_keys k WHERE k.id = $1 AND k.kind = 'deploy'
         AND NOT EXISTS (SELECT 1 FROM deploy_keys_projects l WHERE l.deploy_key_id = k.id)",
        key_id
    )
    .execute(e)
    .await?;
    Ok(())
}
