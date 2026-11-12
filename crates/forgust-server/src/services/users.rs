use forgust_db::users::{NewUser, User};
use forgust_db::visibility;
use sqlx::PgConnection;

use super::paths;
use crate::crypto;
use crate::error::{ApiError, ApiResult};

pub struct CreateUser<'a> {
    pub username: &'a str,
    pub email: &'a str,
    pub name: &'a str,
    pub password: Option<&'a str>,
    pub is_admin: bool,
    /// False for self-registration: the address must be confirmed.
    pub email_verified: bool,
}

pub fn validate_password(p: &str) -> ApiResult<()> {
    if p.len() < 8 || p.len() > 1024 {
        return Err(ApiError::bad("password must be at least 8 characters"));
    }
    Ok(())
}

/// A human user with a personal namespace at `/<username>`.
pub async fn create(conn: &mut PgConnection, req: CreateUser<'_>) -> ApiResult<User> {
    paths::validate_top_level(req.username)?;
    paths::validate_email(req.email)?;
    if let Some(p) = req.password {
        validate_password(p)?;
    }
    if forgust_db::routes::path_taken(&mut *conn, req.username).await? {
        return Err(ApiError::conflict("username is already taken"));
    }
    if forgust_db::users::find_by_email(&mut *conn, req.email).await?.is_some() {
        return Err(ApiError::conflict("email is already in use"));
    }
    let hash = match req.password {
        Some(p) => Some(crypto::hash_password_gated(p).await?),
        None => None,
    };
    let name = if req.name.trim().is_empty() { req.username } else { req.name };
    let mut user = forgust_db::users::insert(
        &mut *conn,
        NewUser {
            username: req.username,
            email: req.email,
            name,
            password_hash: hash.as_deref(),
            user_type: "human",
            bot_source: None,
            is_admin: req.is_admin,
            email_verified: req.email_verified,
        },
    )
    .await?;
    let ns = forgust_db::namespaces::insert(
        &mut *conn,
        forgust_db::namespaces::NewNamespace {
            kind: "user",
            name,
            path: req.username,
            parent_id: None,
            visibility: visibility::PRIVATE,
            description: "",
            owner_id: Some(user.id),
        },
    )
    .await?;
    forgust_db::routes::insert(&mut *conn, "namespace", ns, req.username).await?;
    forgust_db::users::set_namespace(&mut *conn, user.id, ns).await?;
    forgust_db::members::refresh_authorizations(&mut *conn, &[user.id]).await?;
    user.namespace_id = Some(ns);
    Ok(user)
}

/// A bot user behind a project or group access token (ADR-0015). Bots get no
/// personal namespace and cannot sign in.
pub async fn create_bot(conn: &mut PgConnection, source_type: &str, source_id: i64, token_name: &str) -> ApiResult<User> {
    let suffix = crypto::random_urlsafe(6).to_lowercase().replace(['-', '_'], "x");
    let prefix = if source_type == "project" { "project" } else { "group" };
    let username = format!("{prefix}_{source_id}_bot_{suffix}");
    let email = format!("{username}@noreply.forgust.local");
    let user = forgust_db::users::insert(
        &mut *conn,
        NewUser {
            username: &username,
            email: &email,
            name: token_name,
            password_hash: None,
            user_type: if source_type == "project" { "project_bot" } else { "group_bot" },
            bot_source: Some((source_type, source_id)),
            is_admin: false,
            email_verified: true,
        },
    )
    .await?;
    Ok(user)
}
