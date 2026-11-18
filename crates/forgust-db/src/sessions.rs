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
/// Whose live session this is and when it was created (its sign-in), if it
/// is the account's own (not an impersonation).
pub async fn own_sign_in(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<Option<(i64, DateTime<Utc>)>> {
    let r = sqlx::query!(
        "SELECT user_id, created_at FROM sessions WHERE token_hash = $1 AND expires_at > now() AND impersonator_id IS NULL",
        token_hash
    )
    .fetch_optional(e)
    .await?;
    Ok(r.map(|r| (r.user_id, r.created_at)))
}

pub async fn delete_others(e: impl PgExecutor<'_>, user_id: i64, keep: &[u8]) -> Result<()> {
    sqlx::query!(
        "WITH c AS (DELETE FROM login_challenges WHERE user_id = $1),
              w AS (DELETE FROM webauthn_ceremonies WHERE user_id = $1)
         DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2",
        user_id,
        keep
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete_for_user(e: impl PgExecutor<'_>, user_id: i64) -> Result<()> {
    sqlx::query!(
        "WITH c AS (DELETE FROM login_challenges WHERE user_id = $1),
              w AS (DELETE FROM webauthn_ceremonies WHERE user_id = $1)
         DELETE FROM sessions WHERE user_id = $1",
        user_id
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete_expired(pool: &sqlx::PgPool) -> Result<u64> {
    let mut n = sqlx::query!("DELETE FROM sessions WHERE expires_at < now()").execute(pool).await?.rows_affected();
    n += sqlx::query!("DELETE FROM login_challenges WHERE expires_at < now()").execute(pool).await?.rows_affected();
    n += sqlx::query!("DELETE FROM oidc_states WHERE expires_at < now()").execute(pool).await?.rows_affected();
    n += sqlx::query!("DELETE FROM webauthn_ceremonies WHERE expires_at < now()").execute(pool).await?.rows_affected();
    n += sqlx::query!("DELETE FROM lfs_ssh_tokens WHERE expires_at < now()").execute(pool).await?.rows_affected();
    Ok(n)
}

/// `provider_sync`: what the provider sign-in behind this challenge says about
/// the account, to apply once the second factor is given (never before).
pub async fn create_challenge(
    e: impl PgExecutor<'_>,
    token_hash: &[u8],
    user_id: i64,
    provider_sync: Option<&serde_json::Value>,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO login_challenges (token_hash, user_id, provider_sync, expires_at) VALUES ($1, $2, $3, $4)",
        token_hash,
        user_id,
        provider_sync,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Take a pending second-factor challenge for checking, locked until `c`'s
/// transaction ends: two answers sent at once are checked one after the other,
/// and the second finds the challenge gone once the first succeeded.
pub async fn lock_challenge(c: &mut sqlx::PgConnection, token_hash: &[u8]) -> Result<Option<Challenge>> {
    sqlx::query_as!(
        Challenge,
        "SELECT user_id, provider_sync, webauthn_state FROM login_challenges WHERE token_hash = $1 AND expires_at > now() FOR UPDATE",
        token_hash
    )
    .fetch_optional(&mut *c)
    .await
}

/// A sign-in waiting for its second factor.
pub struct Challenge {
    pub user_id: i64,
    /// What its provider sign-in said about the account, to apply once complete.
    pub provider_sync: Option<serde_json::Value>,
    /// What a security key was asked to sign for it, if one was.
    pub webauthn_state: Option<serde_json::Value>,
}

/// Whose pending sign-in this is (without taking it).
pub async fn challenge_user(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<Option<i64>> {
    sqlx::query_scalar!("SELECT user_id FROM login_challenges WHERE token_hash = $1 AND expires_at > now()", token_hash)
        .fetch_optional(e)
        .await
}

/// The session behind a cookie's token (its id), if live.
pub async fn id_by_token(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<Option<i64>> {
    sqlx::query_scalar!("SELECT id FROM sessions WHERE token_hash = $1 AND expires_at > now()", token_hash).fetch_optional(e).await
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
    link_user_id: Option<i64>,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO oidc_states (state, provider, nonce, pkce_verifier, link_user_id, expires_at) VALUES ($1, $2, $3, $4, $5, $6)",
        state,
        provider,
        nonce,
        pkce,
        link_user_id,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// What a provider sign-in remembered between its redirect and its callback.
pub struct OidcState {
    pub provider: String,
    pub nonce: String,
    pub pkce_verifier: String,
    /// The account that started it from its settings, to link the identity to
    /// (None: a sign-in).
    pub link_user_id: Option<i64>,
}

/// Consume an OIDC state.
pub async fn take_oidc_state(e: impl PgExecutor<'_>, state: &str) -> Result<Option<OidcState>> {
    sqlx::query_as!(
        OidcState,
        "DELETE FROM oidc_states WHERE state = $1 AND expires_at > now() RETURNING provider, nonce, pkce_verifier, link_user_id",
        state
    )
    .fetch_optional(e)
    .await
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
                  u.id, u.username, u.email, u.name, u.user_type, u.is_admin, u.is_external, u.state, u.namespace_id, u.totp_enabled, u.security_keys_enabled,
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
                security_keys_enabled: r.security_keys_enabled,
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
