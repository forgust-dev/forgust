//! Personal/bot access tokens, deploy tokens, CI job tokens, LFS SSH tokens
//! (ADR-0015). Only hashes are stored.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AccessToken {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// When an MCP client last used it.
    pub mcp_used_at: Option<DateTime<Utc>>,
}

pub async fn create_pat(
    e: impl PgExecutor<'_>,
    user_id: i64,
    name: &str,
    hash: &[u8],
    scopes: &[String],
    expires_at: Option<DateTime<Utc>>,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO personal_access_tokens (user_id, name, token_hash, scopes, expires_at) VALUES ($1, $2, $3, $4, $5) RETURNING id",
        user_id,
        name,
        hash,
        scopes,
        expires_at
    )
    .fetch_one(e)
    .await
}

/// A live token and its owner, in one round trip (every token request).
pub async fn find_pat_with_user(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<(AccessToken, crate::users::User)>> {
    let r = sqlx::query!(
        r#"SELECT t.id AS tid, t.user_id, t.name AS token_name, t.scopes, t.expires_at, t.revoked, t.last_used_at, t.created_at AS token_created_at,
                  t.mcp_used_at, u.id, u.username, u.email, u.name, u.user_type, u.is_admin, u.is_external, u.state, u.namespace_id,
                  u.totp_enabled, u.created_at, u.email_verified_at, u.unconfirmed_email, u.avatar_at
             FROM personal_access_tokens t JOIN users u ON u.id = t.user_id
            WHERE t.token_hash = $1 AND NOT t.revoked AND (t.expires_at IS NULL OR t.expires_at > now())"#,
        hash
    )
    .fetch_optional(e)
    .await?;
    Ok(r.map(|r| {
        (
            AccessToken {
                id: r.tid,
                user_id: r.user_id,
                name: r.token_name,
                scopes: r.scopes,
                expires_at: r.expires_at,
                revoked: r.revoked,
                last_used_at: r.last_used_at,
                created_at: r.token_created_at,
                mcp_used_at: r.mcp_used_at,
            },
            crate::users::User {
                id: r.id,
                username: r.username,
                email: r.email,
                name: r.name,
                user_type: r.user_type,
                is_admin: r.is_admin,
                is_external: r.is_external,
                state: r.state,
                namespace_id: r.namespace_id,
                totp_enabled: r.totp_enabled,
                created_at: r.created_at,
                email_verified_at: r.email_verified_at,
                unconfirmed_email: r.unconfirmed_email,
                avatar_at: r.avatar_at,
            },
        )
    }))
}

/// The token (by hash) called the MCP server: noted at most once a minute.
/// Returns its id.
pub async fn mark_used_by_mcp(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        r#"WITH t AS (SELECT id FROM personal_access_tokens WHERE token_hash = $1),
                _u AS (UPDATE personal_access_tokens p SET mcp_used_at = now() FROM t
                        WHERE p.id = t.id AND (p.mcp_used_at IS NULL OR p.mcp_used_at < now() - interval '1 minute'))
           SELECT id AS "id!" FROM t"#,
        hash
    )
    .fetch_optional(e)
    .await
}

/// OAuth refresh: the same token gets a new secret and expiry, unless
/// its owner revoked it.
pub async fn rotate_pat(e: impl PgExecutor<'_>, id: i64, user_id: i64, hash: &[u8], expires_at: DateTime<Utc>) -> Result<bool> {
    let r = sqlx::query!(
        "UPDATE personal_access_tokens SET token_hash = $3, expires_at = $4 WHERE id = $1 AND user_id = $2 AND NOT revoked",
        id,
        user_id,
        hash,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(r.rows_affected() == 1)
}

pub async fn touch_pat(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE personal_access_tokens SET last_used_at = now()
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
        id
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn list_pats(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<AccessToken>> {
    sqlx::query_as!(
        AccessToken,
        "SELECT id, user_id, name, scopes, expires_at, revoked, last_used_at, created_at, mcp_used_at FROM personal_access_tokens
         WHERE user_id = $1 ORDER BY created_at DESC",
        user_id
    )
    .fetch_all(e)
    .await
}

pub async fn revoke_pat(e: impl PgExecutor<'_>, user_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("UPDATE personal_access_tokens SET revoked = true WHERE id = $1 AND user_id = $2", id, user_id)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DeployToken {
    pub id: i64,
    pub name: String,
    pub username: String,
    pub project_id: Option<i64>,
    pub namespace_id: Option<i64>,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
pub async fn create_deploy_token(
    e: impl PgExecutor<'_>,
    name: &str,
    username: &str,
    hash: &[u8],
    project_id: Option<i64>,
    namespace_id: Option<i64>,
    scopes: &[String],
    expires_at: Option<DateTime<Utc>>,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO deploy_tokens (name, username, token_hash, project_id, namespace_id, scopes, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        name,
        username,
        hash,
        project_id,
        namespace_id,
        scopes,
        expires_at
    )
    .fetch_one(e)
    .await
}

pub async fn find_deploy_token(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<DeployToken>> {
    sqlx::query_as!(
        DeployToken,
        "SELECT id, name, username, project_id, namespace_id, scopes, expires_at, revoked, created_at FROM deploy_tokens
         WHERE token_hash = $1 AND NOT revoked AND (expires_at IS NULL OR expires_at > now())",
        hash
    )
    .fetch_optional(e)
    .await
}

pub async fn list_deploy_tokens(e: impl PgExecutor<'_>, project_id: Option<i64>, namespace_id: Option<i64>) -> Result<Vec<DeployToken>> {
    sqlx::query_as!(
        DeployToken,
        "SELECT id, name, username, project_id, namespace_id, scopes, expires_at, revoked, created_at FROM deploy_tokens
         WHERE project_id IS NOT DISTINCT FROM $1 AND namespace_id IS NOT DISTINCT FROM $2 ORDER BY created_at DESC",
        project_id,
        namespace_id
    )
    .fetch_all(e)
    .await
}

pub async fn revoke_deploy_token(e: impl PgExecutor<'_>, id: i64, project_id: Option<i64>, namespace_id: Option<i64>) -> Result<bool> {
    Ok(sqlx::query!(
        "UPDATE deploy_tokens SET revoked = true WHERE id = $1 AND project_id IS NOT DISTINCT FROM $2 AND namespace_id IS NOT DISTINCT FROM $3",
        id,
        project_id,
        namespace_id
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}

pub struct JobToken {
    pub id: i64,
    pub project_id: i64,
    pub user_id: i64,
}

pub async fn create_job_token(
    e: impl PgExecutor<'_>,
    hash: &[u8],
    project_id: i64,
    user_id: i64,
    expires_at: DateTime<Utc>,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO ci_job_tokens (token_hash, project_id, user_id, expires_at) VALUES ($1, $2, $3, $4) RETURNING id",
        hash,
        project_id,
        user_id,
        expires_at
    )
    .fetch_one(e)
    .await
}

/// A live CI job token and the job's user, in one round trip.
pub async fn find_job_token(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<(JobToken, crate::users::User)>> {
    let r = sqlx::query!(
        r#"SELECT t.id AS tid, t.project_id, t.user_id, u.id, u.username, u.email, u.name, u.user_type, u.is_admin, u.is_external, u.state,
                  u.namespace_id, u.totp_enabled, u.created_at, u.email_verified_at, u.unconfirmed_email, u.avatar_at
             FROM ci_job_tokens t JOIN users u ON u.id = t.user_id
            WHERE t.token_hash = $1 AND t.expires_at > now()"#,
        hash
    )
    .fetch_optional(e)
    .await?;
    Ok(r.map(|r| {
        (
            JobToken { id: r.tid, project_id: r.project_id, user_id: r.user_id },
            crate::users::User {
                id: r.id,
                username: r.username,
                email: r.email,
                name: r.name,
                user_type: r.user_type,
                is_admin: r.is_admin,
                is_external: r.is_external,
                state: r.state,
                namespace_id: r.namespace_id,
                totp_enabled: r.totp_enabled,
                created_at: r.created_at,
                email_verified_at: r.email_verified_at,
                unconfirmed_email: r.unconfirmed_email,
                avatar_at: r.avatar_at,
            },
        )
    }))
}

pub struct LfsSshToken {
    pub project_id: i64,
    pub principal_kind: String,
    pub principal_id: i64,
    pub can_write: bool,
}

pub async fn create_lfs_ssh_token(
    e: impl PgExecutor<'_>,
    hash: &[u8],
    project_id: i64,
    principal_kind: &str,
    principal_id: i64,
    can_write: bool,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO lfs_ssh_tokens (token_hash, project_id, principal_kind, principal_id, can_write, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        hash,
        project_id,
        principal_kind,
        principal_id,
        can_write,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn find_lfs_ssh_token(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<LfsSshToken>> {
    sqlx::query_as!(
        LfsSshToken,
        "SELECT project_id, principal_kind, principal_id, can_write FROM lfs_ssh_tokens WHERE token_hash = $1 AND expires_at > now()",
        hash
    )
    .fetch_optional(e)
    .await
}
