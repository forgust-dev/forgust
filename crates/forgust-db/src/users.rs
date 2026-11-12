use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub email: String,
    pub name: String,
    pub user_type: String,
    pub is_admin: bool,
    pub is_external: bool,
    pub state: String,
    pub namespace_id: Option<i64>,
    pub totp_enabled: bool,
    pub created_at: DateTime<Utc>,
    /// When the current email was confirmed (None: not yet).
    pub email_verified_at: Option<DateTime<Utc>>,
    /// A new address waiting for its confirmation link.
    pub unconfirmed_email: Option<String>,
    /// When the avatar was set (None: no avatar).
    pub avatar_at: Option<DateTime<Utc>>,
}

impl User {
    pub fn is_bot(&self) -> bool {
        self.user_type != "human"
    }
    /// A bot Forgust runs its own features as (built-in Renovate, scanners):
    /// not a project's access-token bot.
    pub fn is_builtin_bot(&self) -> bool {
        matches!(self.user_type.as_str(), "renovate_bot" | "security_bot")
    }
    pub fn is_active(&self) -> bool {
        self.state == "active"
    }
    pub fn email_verified(&self) -> bool {
        self.email_verified_at.is_some()
    }
}

pub struct NewUser<'a> {
    pub username: &'a str,
    pub email: &'a str,
    pub name: &'a str,
    pub password_hash: Option<&'a str>,
    pub user_type: &'a str,
    pub bot_source: Option<(&'a str, i64)>,
    pub is_admin: bool,
    /// Created by an administrator or confirmed by an identity provider.
    pub email_verified: bool,
}

pub async fn insert(conn: &mut PgConnection, u: NewUser<'_>) -> Result<User> {
    let (bst, bsi) = match u.bot_source {
        Some((t, i)) => (Some(t), Some(i)),
        None => (None, None),
    };
    sqlx::query_as!(
        User,
        r#"INSERT INTO users (username, email, name, password_hash, user_type, bot_source_type, bot_source_id, is_admin, email_verified_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, CASE WHEN $9 THEN now() END)
           RETURNING id, username, email, name, user_type, is_admin, is_external, state, namespace_id, totp_enabled, created_at,
                  email_verified_at, unconfirmed_email, avatar_at"#,
        u.username,
        u.email,
        u.name,
        u.password_hash,
        u.user_type,
        bst,
        bsi,
        u.is_admin,
        u.email_verified
    )
    .fetch_one(conn)
    .await
}

pub async fn set_namespace(e: impl PgExecutor<'_>, user_id: i64, namespace_id: i64) -> Result<()> {
    sqlx::query!("UPDATE users SET namespace_id = $2 WHERE id = $1", user_id, namespace_id).execute(e).await?;
    Ok(())
}

pub async fn find(e: impl PgExecutor<'_>, id: i64) -> Result<Option<User>> {
    sqlx::query_as!(
        User,
        "SELECT id, username, email, name, user_type, is_admin, is_external, state, namespace_id, totp_enabled, created_at,
                  email_verified_at, unconfirmed_email, avatar_at
         FROM users WHERE id = $1",
        id
    )
    .fetch_optional(e)
    .await
}

/// These users (any order; missing ids are skipped).
pub async fn find_many(e: impl PgExecutor<'_>, ids: &[i64]) -> Result<Vec<User>> {
    sqlx::query_as!(
        User,
        "SELECT id, username, email, name, user_type, is_admin, is_external, state, namespace_id, totp_enabled, created_at,
                  email_verified_at, unconfirmed_email, avatar_at
         FROM users WHERE id = ANY($1)",
        ids
    )
    .fetch_all(e)
    .await
}

pub async fn find_by_username(e: impl PgExecutor<'_>, username: &str) -> Result<Option<User>> {
    sqlx::query_as!(
        User,
        "SELECT id, username, email, name, user_type, is_admin, is_external, state, namespace_id, totp_enabled, created_at,
                  email_verified_at, unconfirmed_email, avatar_at
         FROM users WHERE lower(username) = lower($1)",
        username
    )
    .fetch_optional(e)
    .await
}

pub async fn find_by_email(e: impl PgExecutor<'_>, email: &str) -> Result<Option<User>> {
    sqlx::query_as!(
        User,
        "SELECT id, username, email, name, user_type, is_admin, is_external, state, namespace_id, totp_enabled, created_at,
                  email_verified_at, unconfirmed_email, avatar_at
         FROM users WHERE lower(email) = lower($1)",
        email
    )
    .fetch_optional(e)
    .await
}

/// (user, password hash) for a username or email.
pub async fn credentials(e: impl PgExecutor<'_>, login: &str) -> Result<Option<(i64, Option<String>)>> {
    let r = sqlx::query!(
        "SELECT id, password_hash FROM users
         WHERE (lower(username) = lower($1) OR lower(email) = lower($1)) AND user_type = 'human'",
        login
    )
    .fetch_optional(e)
    .await?;
    Ok(r.map(|r| (r.id, r.password_hash)))
}

pub async fn password_hash(e: impl PgExecutor<'_>, id: i64) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!("SELECT password_hash FROM users WHERE id = $1", id).fetch_optional(e).await?.flatten())
}

pub async fn set_password(e: impl PgExecutor<'_>, id: i64, hash: &str) -> Result<()> {
    sqlx::query!("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1", id, hash).execute(e).await?;
    Ok(())
}

pub async fn totp_secret(e: impl PgExecutor<'_>, id: i64) -> Result<Option<String>> {
    Ok(sqlx::query_scalar!("SELECT totp_secret_enc FROM users WHERE id = $1", id).fetch_optional(e).await?.flatten())
}

/// Accept TOTP time step `step` for the account unless it (or a later one)
/// was already used: a code works once.
pub async fn use_totp_step(e: impl PgExecutor<'_>, id: i64, step: i64) -> Result<bool> {
    Ok(sqlx::query!("UPDATE users SET totp_last_step = $2 WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)", id, step)
        .execute(e)
        .await?
        .rows_affected()
        == 1)
}

pub async fn set_totp(e: impl PgExecutor<'_>, id: i64, secret_enc: Option<&str>, enabled: bool) -> Result<()> {
    sqlx::query!("UPDATE users SET totp_secret_enc = $2, totp_enabled = $3, updated_at = now() WHERE id = $1", id, secret_enc, enabled)
        .execute(e)
        .await?;
    Ok(())
}

pub async fn set_admin(e: impl PgExecutor<'_>, id: i64, admin: bool) -> Result<()> {
    sqlx::query!("UPDATE users SET is_admin = $2, updated_at = now() WHERE id = $1", id, admin).execute(e).await?;
    Ok(())
}

pub async fn set_state(e: impl PgExecutor<'_>, id: i64, state: &str) -> Result<()> {
    sqlx::query!("UPDATE users SET state = $2, updated_at = now() WHERE id = $1", id, state).execute(e).await?;
    Ok(())
}

pub async fn update_profile(e: impl PgExecutor<'_>, id: i64, name: &str, email: &str) -> Result<()> {
    sqlx::query!("UPDATE users SET name = $2, email = $3, updated_at = now() WHERE id = $1", id, name, email).execute(e).await?;
    Ok(())
}

/// `match_email` lets the search match email addresses (admins only: otherwise it
/// would reveal which addresses have accounts).
pub async fn list(
    e: impl PgExecutor<'_>,
    search: &str,
    include_bots: bool,
    match_email: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<User>> {
    sqlx::query_as!(
        User,
        "SELECT id, username, email, name, user_type, is_admin, is_external, state, namespace_id, totp_enabled, created_at,
                  email_verified_at, unconfirmed_email, avatar_at
         FROM users
         WHERE ($1 = '' OR username ILIKE '%' || $1 || '%' OR name ILIKE '%' || $1 || '%' OR ($5 AND email ILIKE '%' || $1 || '%'))
           AND ($2 OR user_type = 'human')
         ORDER BY username LIMIT $3 OFFSET $4",
        search,
        include_bots,
        limit,
        offset,
        match_email
    )
    .fetch_all(e)
    .await
}

pub async fn count_humans(e: impl PgExecutor<'_>) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM users WHERE user_type = 'human'"#).fetch_one(e).await
}

pub async fn delete(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM users WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// Every way into the account but its password: access tokens, SSH keys and
/// provider identities (for an address that turns out to be someone else's).
pub async fn forget_credentials(c: &mut PgConnection, user_id: i64) -> Result<()> {
    sqlx::query!("UPDATE personal_access_tokens SET revoked = true WHERE user_id = $1", user_id).execute(&mut *c).await?;
    sqlx::query!("DELETE FROM ssh_keys WHERE user_id = $1", user_id).execute(&mut *c).await?;
    sqlx::query!("DELETE FROM identities WHERE user_id = $1", user_id).execute(&mut *c).await?;
    Ok(())
}

pub async fn find_identity(e: impl PgExecutor<'_>, provider: &str, subject: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!("SELECT user_id FROM identities WHERE provider = $1 AND subject = $2", provider, subject).fetch_optional(e).await
}

pub async fn add_identity(e: impl PgExecutor<'_>, user_id: i64, provider: &str, subject: &str) -> Result<()> {
    sqlx::query!(
        "INSERT INTO identities (user_id, provider, subject) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        user_id,
        provider,
        subject
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Bot users created for a project or namespace access token.
pub async fn bots_for(e: impl PgExecutor<'_>, source_type: &str, source_id: i64) -> Result<Vec<User>> {
    sqlx::query_as!(
        User,
        "SELECT id, username, email, name, user_type, is_admin, is_external, state, namespace_id, totp_enabled, created_at,
                  email_verified_at, unconfirmed_email, avatar_at
         FROM users WHERE bot_source_type = $1 AND bot_source_id = $2 ORDER BY id",
        source_type,
        source_id
    )
    .fetch_all(e)
    .await
}

pub async fn set_email_verified(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE users SET email_verified_at = now(), updated_at = now() WHERE id = $1", id).execute(e).await?;
    Ok(())
}

pub async fn set_unconfirmed_email(e: impl PgExecutor<'_>, id: i64, email: Option<&str>) -> Result<()> {
    sqlx::query!("UPDATE users SET unconfirmed_email = $2, updated_at = now() WHERE id = $1", id, email).execute(e).await?;
    Ok(())
}

/// The confirmed address becomes the account's email.
pub async fn confirm_email(e: impl PgExecutor<'_>, id: i64, email: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE users SET email = $2, unconfirmed_email = NULL, email_verified_at = now(), updated_at = now() WHERE id = $1",
        id,
        email
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Users by username or email (case-insensitive), for "forgot password".
pub async fn find_by_login(e: impl PgExecutor<'_>, login: &str) -> Result<Option<User>> {
    sqlx::query_as!(
        User,
        "SELECT id, username, email, name, user_type, is_admin, is_external, state, namespace_id, totp_enabled, created_at,
                  email_verified_at, unconfirmed_email, avatar_at
         FROM users WHERE lower(username) = lower($1) OR lower(email) = lower($1)
         ORDER BY (lower(username) = lower($1)) DESC LIMIT 1",
        login
    )
    .fetch_optional(e)
    .await
}

// ------------------------------------------------------------------ email links

pub struct UserToken {
    pub user_id: i64,
    pub email: Option<String>,
}

pub async fn add_token(
    e: impl PgExecutor<'_>,
    hash: &[u8],
    user_id: i64,
    purpose: &str,
    email: Option<&str>,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO user_tokens (token_hash, user_id, purpose, email, expires_at) VALUES ($1, $2, $3, $4, $5)",
        hash,
        user_id,
        purpose,
        email,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Use a token once: it is deleted, and returned only if unexpired.
pub async fn take_token(e: impl PgExecutor<'_>, hash: &[u8], purpose: &str) -> Result<Option<UserToken>> {
    let row = sqlx::query!(
        r#"DELETE FROM user_tokens WHERE token_hash = $1 AND purpose = $2
           RETURNING user_id, email, expires_at > now() AS "live!""#,
        hash,
        purpose
    )
    .fetch_optional(e)
    .await?;
    Ok(row.filter(|r| r.live).map(|r| UserToken { user_id: r.user_id, email: r.email }))
}

pub async fn delete_tokens(e: impl PgExecutor<'_>, user_id: i64, purpose: &str) -> Result<()> {
    sqlx::query!("DELETE FROM user_tokens WHERE user_id = $1 AND purpose = $2", user_id, purpose).execute(e).await?;
    Ok(())
}

/// Mark the avatar as set now, or removed.
pub async fn set_avatar(e: impl PgExecutor<'_>, id: i64, present: bool) -> Result<()> {
    sqlx::query!("UPDATE users SET avatar_at = CASE WHEN $2 THEN now() END WHERE id = $1", id, present).execute(e).await?;
    Ok(())
}
