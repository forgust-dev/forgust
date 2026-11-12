use chrono::{DateTime, Utc};
use sqlx::PgExecutor;

use crate::Result;

pub struct SessionRow {
    pub id: i64,
    pub user_id: i64,
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// The administrator signed in as this user (impersonation).
    pub impersonator_id: Option<i64>,
}

pub async fn create(
    e: impl PgExecutor<'_>,
    token_hash: &[u8],
    user_id: i64,
    csrf: &str,
    ip: Option<&str>,
    user_agent: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO sessions (token_hash, user_id, csrf_token, ip, user_agent, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        token_hash,
        user_id,
        csrf,
        ip,
        user_agent,
        expires_at
    )
    .fetch_one(e)
    .await
}

pub async fn touch(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE sessions SET last_seen_at = now() WHERE id = $1", id).execute(e).await?;
    Ok(())
}

pub async fn delete(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<()> {
    sqlx::query!("DELETE FROM sessions WHERE token_hash = $1", token_hash).execute(e).await?;
    Ok(())
}

/// Every session of `user_id` but the one with `keep` (the one in use).
/// When a live session was created (its sign-in).
pub async fn created_at(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar!("SELECT created_at FROM sessions WHERE token_hash = $1 AND expires_at > now()", token_hash).fetch_optional(e).await
}

pub async fn delete_others(e: impl PgExecutor<'_>, user_id: i64, keep: &[u8]) -> Result<()> {
    sqlx::query!("DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2", user_id, keep).execute(e).await?;
    Ok(())
}

pub async fn delete_for_user(e: impl PgExecutor<'_>, user_id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM sessions WHERE user_id = $1", user_id).execute(e).await?;
    Ok(())
}

pub async fn delete_expired(pool: &sqlx::PgPool) -> Result<u64> {
    let mut n = sqlx::query!("DELETE FROM sessions WHERE expires_at < now()").execute(pool).await?.rows_affected();
    n += sqlx::query!("DELETE FROM login_challenges WHERE expires_at < now()").execute(pool).await?.rows_affected();
    n += sqlx::query!("DELETE FROM oidc_states WHERE expires_at < now()").execute(pool).await?.rows_affected();
    n += sqlx::query!("DELETE FROM lfs_ssh_tokens WHERE expires_at < now()").execute(pool).await?.rows_affected();
    Ok(n)
}

pub async fn create_challenge(e: impl PgExecutor<'_>, token_hash: &[u8], user_id: i64, expires_at: DateTime<Utc>) -> Result<()> {
    sqlx::query!("INSERT INTO login_challenges (token_hash, user_id, expires_at) VALUES ($1, $2, $3)", token_hash, user_id, expires_at)
        .execute(e)
        .await?;
    Ok(())
}

/// Look up (without consuming) a pending second-factor challenge.
pub async fn find_challenge(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<Option<i64>> {
    sqlx::query_scalar!("SELECT user_id FROM login_challenges WHERE token_hash = $1 AND expires_at > now()", token_hash)
        .fetch_optional(e)
        .await
}

pub async fn delete_challenge(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<()> {
    sqlx::query!("DELETE FROM login_challenges WHERE token_hash = $1", token_hash).execute(e).await?;
    Ok(())
}

pub async fn save_oidc_state(
    e: impl PgExecutor<'_>,
    state: &str,
    provider: &str,
    nonce: &str,
    pkce: &str,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO oidc_states (state, provider, nonce, pkce_verifier, expires_at) VALUES ($1, $2, $3, $4, $5)",
        state,
        provider,
        nonce,
        pkce,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Consume an OIDC state: returns (provider, nonce, pkce verifier).
pub async fn take_oidc_state(e: impl PgExecutor<'_>, state: &str) -> Result<Option<(String, String, String)>> {
    let r = sqlx::query!("DELETE FROM oidc_states WHERE state = $1 AND expires_at > now() RETURNING provider, nonce, pkce_verifier", state)
        .fetch_optional(e)
        .await?;
    Ok(r.map(|r| (r.provider, r.nonce, r.pkce_verifier)))
}

/// The account has a session from this address (its owner's usual places).
pub async fn signed_in_from(e: impl PgExecutor<'_>, user_id: i64, ip: &str) -> Result<bool> {
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM sessions WHERE user_id = $1 AND ip = $2) AS "found!""#, user_id, ip)
        .fetch_one(e)
        .await
}

/// A live session and its user, in one round trip (every signed-in request).
pub async fn find_with_user(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<Option<(SessionRow, crate::users::User)>> {
    let r = sqlx::query!(
        r#"SELECT s.id AS sid, s.user_id, s.csrf_token, s.expires_at, s.last_seen_at, s.impersonator_id,
                  u.id, u.username, u.email, u.name, u.user_type, u.is_admin, u.is_external, u.state, u.namespace_id, u.totp_enabled,
                  u.created_at, u.email_verified_at, u.unconfirmed_email, u.avatar_at
             FROM sessions s JOIN users u ON u.id = s.user_id
            WHERE s.token_hash = $1 AND s.expires_at > now()"#,
        token_hash
    )
    .fetch_optional(e)
    .await?;
    Ok(r.map(|r| {
        (
            SessionRow {
                id: r.sid,
                user_id: r.user_id,
                csrf_token: r.csrf_token,
                expires_at: r.expires_at,
                last_seen_at: r.last_seen_at,
                impersonator_id: r.impersonator_id,
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

/// A session of `user_id` started by administrator `impersonator_id`.
pub async fn create_impersonation(
    e: impl PgExecutor<'_>,
    token_hash: &[u8],
    user_id: i64,
    impersonator_id: i64,
    csrf: &str,
    ip: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO sessions (token_hash, user_id, csrf_token, ip, user_agent, expires_at, impersonator_id)
         VALUES ($1, $2, $3, $4, 'impersonation', $5, $6) RETURNING id",
        token_hash,
        user_id,
        csrf,
        ip,
        expires_at,
        impersonator_id
    )
    .fetch_one(e)
    .await
}

/// A session that does not count as a recent sign-in (`created_at` a day ago).
pub async fn mark_not_recent(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<()> {
    sqlx::query!("UPDATE sessions SET created_at = now() - interval '1 day' WHERE token_hash = $1", token_hash).execute(e).await?;
    Ok(())
}
