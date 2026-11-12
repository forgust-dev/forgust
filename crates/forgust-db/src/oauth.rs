//! OAuth 2.1 for MCP clients (ADR-0025): dynamically registered
//! public clients, one-time authorization codes, rotating refresh tokens.
//! Codes and refresh tokens are stored hashed.

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Client {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
}

pub async fn create_client(e: impl PgExecutor<'_>, client_id: &str, name: &str, redirect_uris: &[String]) -> Result<()> {
    sqlx::query!("INSERT INTO oauth_clients (client_id, name, redirect_uris) VALUES ($1, $2, $3)", client_id, name, redirect_uris)
        .execute(e)
        .await?;
    Ok(())
}

pub async fn find_client(e: impl PgExecutor<'_>, client_id: &str) -> Result<Option<Client>> {
    sqlx::query_as!(Client, "SELECT client_id, name, redirect_uris FROM oauth_clients WHERE client_id = $1", client_id)
        .fetch_optional(e)
        .await
}

pub struct Code {
    pub client_id: String,
    pub user_id: i64,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub scopes: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn create_code(
    e: impl PgExecutor<'_>,
    hash: &[u8],
    client_id: &str,
    user_id: i64,
    redirect_uri: &str,
    code_challenge: &str,
    scopes: &[String],
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO oauth_codes (code_hash, client_id, user_id, redirect_uri, code_challenge, scopes, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        hash,
        client_id,
        user_id,
        redirect_uri,
        code_challenge,
        scopes,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// A live code, used up (codes work once).
pub async fn take_code(e: impl PgExecutor<'_>, hash: &[u8]) -> Result<Option<Code>> {
    sqlx::query_as!(
        Code,
        "WITH gone AS (DELETE FROM oauth_codes WHERE code_hash = $1 OR expires_at < now() RETURNING *)
         SELECT client_id, user_id, redirect_uri, code_challenge, scopes FROM gone WHERE code_hash = $1 AND expires_at >= now()",
        hash
    )
    .fetch_optional(e)
    .await
}

pub struct Refresh {
    pub client_id: String,
    pub user_id: i64,
    pub scopes: Vec<String>,
    pub access_token_id: Option<i64>,
}

pub async fn create_refresh(
    e: impl PgExecutor<'_>,
    hash: &[u8],
    client_id: &str,
    user_id: i64,
    scopes: &[String],
    access_token_id: i64,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO oauth_refresh_tokens (token_hash, client_id, user_id, scopes, access_token_id, expires_at) VALUES ($1, $2, $3, $4, $5, $6)",
        hash,
        client_id,
        user_id,
        scopes,
        access_token_id,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// What presenting a refresh token found.
pub enum RefreshUse {
    /// Live and unused: now used.
    Fresh(Refresh),
    /// Already used once: someone holds a copy. Its access token is returned
    /// so the caller can revoke it.
    Reused {
        user_id: i64,
        access_token_id: Option<i64>,
    },
    Unknown,
}

/// Use a refresh token (each use gets a new one). Used ones stay until they
/// expire, so a second use is recognised.
pub async fn use_refresh(c: &mut sqlx::PgConnection, hash: &[u8]) -> Result<RefreshUse> {
    sqlx::query!("DELETE FROM oauth_refresh_tokens WHERE expires_at < now()").execute(&mut *c).await?;
    let fresh = sqlx::query_as!(
        Refresh,
        "UPDATE oauth_refresh_tokens SET used_at = now() WHERE token_hash = $1 AND used_at IS NULL
         RETURNING client_id, user_id, scopes, access_token_id",
        hash
    )
    .fetch_optional(&mut *c)
    .await?;
    if let Some(r) = fresh {
        return Ok(RefreshUse::Fresh(r));
    }
    let used = sqlx::query!("SELECT user_id, access_token_id FROM oauth_refresh_tokens WHERE token_hash = $1", hash)
        .fetch_optional(&mut *c)
        .await?;
    Ok(match used {
        Some(u) => RefreshUse::Reused { user_id: u.user_id, access_token_id: u.access_token_id },
        None => RefreshUse::Unknown,
    })
}

/// Forget every refresh token of one access token (its client's grant).
pub async fn delete_family(e: impl PgExecutor<'_>, access_token_id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM oauth_refresh_tokens WHERE access_token_id = $1", access_token_id).execute(e).await?;
    Ok(())
}
