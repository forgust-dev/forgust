//! More addresses per account, besides its own (`users.email`). A confirmed
//! one counts like the account's own for linking commits and imported
//! people, signatures and code owners; never for signing in (single sign-on
//! links by the account's own address only) nor for emails sent.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct UserEmail {
    pub id: i64,
    pub email: String,
    /// When it was confirmed (None: not yet).
    pub confirmed_at: Option<DateTime<Utc>>,
    /// How: `link` (from its inbox), `admin` (an administrator vouched for
    /// it) or `auto` (the instance trusted it, sending no email).
    pub confirmed_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub async fn list(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<UserEmail>> {
    sqlx::query_as!(
        UserEmail,
        "SELECT id, email, confirmed_at, confirmed_by, created_at FROM user_emails WHERE user_id = $1 ORDER BY lower(email)",
        user_id
    )
    .fetch_all(e)
    .await
}

pub async fn count(e: impl PgExecutor<'_>, user_id: i64) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM user_emails WHERE user_id = $1"#, user_id).fetch_one(e).await
}

pub async fn find(e: impl PgExecutor<'_>, user_id: i64, id: i64) -> Result<Option<UserEmail>> {
    sqlx::query_as!(
        UserEmail,
        "SELECT id, email, confirmed_at, confirmed_by, created_at FROM user_emails WHERE user_id = $1 AND id = $2",
        user_id,
        id
    )
    .fetch_optional(e)
    .await
}

/// The account's extra address `email` (any case).
pub async fn find_by_email(e: impl PgExecutor<'_>, user_id: i64, email: &str) -> Result<Option<UserEmail>> {
    sqlx::query_as!(
        UserEmail,
        "SELECT id, email, confirmed_at, confirmed_by, created_at FROM user_emails WHERE user_id = $1 AND lower(email) = lower($2)",
        user_id,
        email
    )
    .fetch_optional(e)
    .await
}

/// Add an address, confirmed already when `confirmed_by` says how. Taken
/// (by anyone, as their own or an extra address): a unique violation.
pub async fn insert(e: impl PgExecutor<'_>, user_id: i64, email: &str, confirmed_by: Option<&str>) -> Result<UserEmail> {
    sqlx::query_as!(
        UserEmail,
        "INSERT INTO user_emails (user_id, email, confirmed_at, confirmed_by)
         VALUES ($1, $2, CASE WHEN $3::text IS NOT NULL THEN now() END, $3)
         RETURNING id, email, confirmed_at, confirmed_by, created_at",
        user_id,
        email,
        confirmed_by
    )
    .fetch_one(e)
    .await
}

/// Confirm it (`by`: link, admin or auto). False: it was already, or is gone.
pub async fn confirm(e: impl PgExecutor<'_>, id: i64, by: &str) -> Result<bool> {
    let n = sqlx::query!("UPDATE user_emails SET confirmed_at = now(), confirmed_by = $2 WHERE id = $1 AND confirmed_at IS NULL", id, by)
        .execute(e)
        .await?
        .rows_affected();
    Ok(n > 0)
}

pub async fn delete(e: impl PgExecutor<'_>, user_id: i64, id: i64) -> Result<Option<UserEmail>> {
    sqlx::query_as!(
        UserEmail,
        "DELETE FROM user_emails WHERE user_id = $1 AND id = $2 RETURNING id, email, confirmed_at, confirmed_by, created_at",
        user_id,
        id
    )
    .fetch_optional(e)
    .await
}

/// Whose address `email` is, confirmed or not, as their own or an extra one.
pub async fn taken_by(e: impl PgExecutor<'_>, email: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM users WHERE lower(email) = lower($1)
           UNION ALL
           SELECT user_id FROM user_emails WHERE lower(email) = lower($1)
           LIMIT 1"#,
        email
    )
    .fetch_optional(e)
    .await
}

/// The account that confirmed `email` (its own address or an extra one).
pub async fn confirmed_owner(e: impl PgExecutor<'_>, email: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!(r#"SELECT user_id AS "user_id!" FROM confirmed_emails WHERE email = lower($1) LIMIT 1"#, email)
        .fetch_optional(e)
        .await
}

/// `email` is a confirmed address of `user_id` (its own or an extra one).
pub async fn owns(e: impl PgExecutor<'_>, user_id: i64, email: &str) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM confirmed_emails WHERE user_id = $1 AND email = lower($2)) AS "owns!""#,
        user_id,
        email
    )
    .fetch_one(e)
    .await
}

/// The links sent for the account's extra address `email`.
pub async fn delete_tokens(e: impl PgExecutor<'_>, user_id: i64, email: &str) -> Result<()> {
    sqlx::query!("DELETE FROM user_tokens WHERE user_id = $1 AND purpose = 'extra_email' AND lower(email) = lower($2)", user_id, email)
        .execute(e)
        .await?;
    Ok(())
}
