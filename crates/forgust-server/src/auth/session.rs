//! Browser sessions, stored in Postgres (no Redis), and the TOTP second factor.

use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::{Duration, Utc};
use forgust_db::users::User;
use totp_rs::{Algorithm, Secret, TOTP};

use super::session_cookie_name;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub struct NewSession {
    pub cookie: Cookie<'static>,
    pub csrf: String,
}

pub async fn create(state: &AppState, user_id: i64, ip: Option<&str>, user_agent: Option<&str>) -> ApiResult<NewSession> {
    let token = crypto::random_token(crypto::prefix::SESSION);
    let csrf = crypto::random_urlsafe(24);
    let ttl = Duration::hours(state.cfg.security.session_ttl_hours);
    forgust_db::sessions::create(&state.db, &crypto::token_hash(&token), user_id, &csrf, ip, user_agent, Utc::now() + ttl).await?;
    // every sign-in (password, two-factor, SSO) starts here
    if let Err(e) = forgust_db::admin::record_sign_in(&state.db, user_id).await {
        tracing::warn!(user_id, error = %e, "last sign-in not recorded");
    }
    Ok(NewSession { cookie: cookie(state, token, ttl), csrf })
}

fn cookie(state: &AppState, token: String, ttl: Duration) -> Cookie<'static> {
    Cookie::build((session_cookie_name(&state.cfg), token))
        .path("/")
        .http_only(true)
        .secure(state.cfg.secure_cookies())
        .same_site(SameSite::Lax)
        .max_age(time_duration(ttl))
        .build()
}

fn time_duration(d: Duration) -> time::Duration {
    time::Duration::seconds(d.num_seconds())
}

pub fn clear_cookie(state: &AppState) -> Cookie<'static> {
    Cookie::build((session_cookie_name(&state.cfg), "")).path("/").secure(state.cfg.secure_cookies()).max_age(time::Duration::ZERO).build()
}

/// (user, csrf token) for a live session cookie value.
/// The session's user, its CSRF token and, when an administrator is signed in
/// as that user, the administrator's id.
pub async fn lookup(state: &AppState, token: &str) -> ApiResult<Option<(User, String, Option<i64>)>> {
    let Some((s, user)) = forgust_db::sessions::find_with_user(&state.db, &crypto::token_hash(token)).await? else { return Ok(None) };
    if !user.is_active() {
        return Ok(None);
    }
    // an impersonation lasts only while its administrator is one (and active)
    if let Some(admin) = s.impersonator_id
        && !forgust_db::users::find(&state.db, admin).await?.is_some_and(|a| a.is_active() && a.is_admin)
    {
        forgust_db::sessions::delete(&state.db, &crypto::token_hash(token)).await?;
        return Ok(None);
    }
    if Utc::now() - s.last_seen_at > Duration::minutes(5) {
        forgust_db::sessions::touch(&state.db, s.id).await?;
    }
    Ok(Some((user, s.csrf_token, s.impersonator_id)))
}

/// How long an administrator stays signed in as someone else.
pub const IMPERSONATION_MINUTES: i64 = 60;

/// A session of `user` for administrator `admin` (impersonation).
pub async fn create_impersonation(state: &AppState, user_id: i64, admin_id: i64, ip: Option<&str>) -> ApiResult<NewSession> {
    let token = crypto::random_token(crypto::prefix::SESSION);
    let csrf = crypto::random_urlsafe(24);
    let ttl = Duration::minutes(IMPERSONATION_MINUTES);
    forgust_db::sessions::create_impersonation(&state.db, &crypto::token_hash(&token), user_id, admin_id, &csrf, ip, Utc::now() + ttl)
        .await?;
    Ok(NewSession { cookie: cookie(state, token, ttl), csrf })
}

pub async fn destroy(state: &AppState, token: &str) -> ApiResult<()> {
    forgust_db::sessions::delete(&state.db, &crypto::token_hash(token)).await?;
    Ok(())
}

pub fn totp_for(secret_b32: &str, account: &str) -> ApiResult<TOTP> {
    let bytes = Secret::Encoded(secret_b32.to_string()).to_bytes().map_err(|_| ApiError::bad("bad TOTP secret"))?;
    TOTP::new(Algorithm::SHA1, 6, 1, 30, bytes, Some("Forgust".into()), account.to_string())
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))
}

pub fn new_totp_secret() -> String {
    Secret::generate_secret().to_encoded().to_string()
}

/// Check a code against the user's stored (encrypted) TOTP seed.
pub async fn verify_totp(state: &AppState, user: &User, code: &str) -> ApiResult<bool> {
    let Some(sealed) = forgust_db::users::totp_secret(&state.db, user.id).await? else { return Ok(false) };
    let secret = String::from_utf8(state.secrets.decrypt(&sealed)?).map_err(|e| ApiError::Internal(e.into()))?;
    let totp = totp_for(&secret, &user.username)?;
    let code = code.trim();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| ApiError::Internal(e.into()))?.as_secs();
    // the step before, this one and the next (clock skew), each once
    let Some(step) = [now.saturating_sub(totp.step), now, now + totp.step]
        .into_iter()
        .find(|t| crate::crypto::eq_ct(&totp.generate(*t), code))
        .map(|t| (t / totp.step) as i64)
    else {
        return Ok(false);
    };
    Ok(forgust_db::users::use_totp_step(&state.db, user.id, step).await?)
}
