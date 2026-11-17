//! Browser sessions, stored in Postgres (no Redis), and the TOTP second factor.

use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::{Duration, Utc};
use forgust_db::users::User;
use totp_rs::{Algorithm, Builder, Secret, Totp};

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

/// SHA-1, 6 digits, 30 s steps, one step of clock skew either way.
pub fn totp_for(secret_b32: &str, account: &str) -> ApiResult<Totp> {
    let secret = Secret::try_from_base32(secret_b32).map_err(|_| ApiError::bad("bad TOTP secret"))?;
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_skew(1)
        .with_step_duration(30)
        .with_secret(secret)
        .with_issuer(Some("Forgust"))
        .with_account_name(account)
        .build()
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))
}

pub fn new_totp_secret() -> String {
    Secret::generate().to_base32()
}

/// Check a code against the user's stored (encrypted) TOTP seed, on `c` (the
/// caller's transaction, when it holds one).
pub async fn verify_totp(state: &AppState, c: &mut sqlx::PgConnection, user: &User, code: &str) -> ApiResult<bool> {
    let Some(sealed) = forgust_db::users::totp_secret(&mut *c, user.id).await? else { return Ok(false) };
    let secret = String::from_utf8(state.secrets.decrypt(&sealed)?).map_err(|e| ApiError::Internal(e.into()))?;
    let totp = totp_for(&secret, &user.username)?;
    let code = code.trim();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| ApiError::Internal(e.into()))?.as_secs();
    // the step before, this one and the next (clock skew), each accepted once
    let Some(step) = totp.check(code, now) else { return Ok(false) };
    Ok(forgust_db::users::use_totp_step(&mut *c, user.id, step as i64).await?)
}

/// Recovery codes given at once: each stands in for the second factor once.
const RECOVERY_CODES: usize = 10;

/// New recovery codes, as shown to the user: 16 characters of base32 (80
/// bits, so a plain hash is enough), in groups of four.
fn new_recovery_codes() -> Vec<String> {
    (0..RECOVERY_CODES)
        .map(|_| {
            let raw = crypto::random_chars(b"abcdefghijklmnopqrstuvwxyz234567", 16);
            format!("{}-{}-{}-{}", &raw[..4], &raw[4..8], &raw[8..12], &raw[12..])
        })
        .collect()
}

/// What is stored for a recovery code, however it was typed (case, dashes, spaces).
pub fn recovery_code_hash(code: &str) -> Vec<u8> {
    let normal: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect();
    crypto::token_hash(&normal)
}

/// Give the account a new set of recovery codes (the old ones stop working),
/// on the caller's transaction; returns them, to show once.
pub async fn reset_recovery_codes(c: &mut sqlx::PgConnection, user_id: i64) -> ApiResult<Vec<String>> {
    let codes = new_recovery_codes();
    let hashes: Vec<Vec<u8>> = codes.iter().map(|c| recovery_code_hash(c)).collect();
    forgust_db::recovery_codes::replace(c, user_id, &hashes).await?;
    Ok(codes)
}

/// Recovery codes for an account getting its first second factor: new ones
/// when it has none left, nothing (empty) when it keeps the ones it has.
pub async fn first_recovery_codes(c: &mut sqlx::PgConnection, user_id: i64) -> ApiResult<Vec<String>> {
    if forgust_db::recovery_codes::remaining(&mut *c, user_id).await? > 0 {
        return Ok(Vec::new());
    }
    reset_recovery_codes(c, user_id).await
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn codes_are_distinct_and_typed_loosely() {
        let codes = new_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        assert_eq!(codes.iter().collect::<std::collections::HashSet<_>>().len(), RECOVERY_CODES);
        let c = &codes[0];
        assert_eq!(c.len(), 19);
        let sloppy = format!(" {} ", c.replace('-', " ").to_uppercase());
        assert_eq!(recovery_code_hash(&sloppy), recovery_code_hash(c));
        assert_ne!(recovery_code_hash(&codes[1]), recovery_code_hash(c));
    }
}

#[cfg(test)]
mod totp_tests {
    use super::*;

    // RFC 6238 test vector: secret "12345678901234567890", SHA-1, T = 59 s
    const RFC_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn codes_match_the_rfc_and_the_window_is_one_step() {
        let t = totp_for(RFC_SECRET, "x").unwrap();
        assert_eq!(t.generate(59).to_string(), "287082");
        // accepted from the step before to the step after, reporting the code's step
        assert_eq!(t.check("287082", 59), Some(1));
        assert_eq!(t.check("287082", 89), Some(1));
        assert_eq!(t.check("287082", 29), Some(1));
        assert_eq!(t.check("287082", 120), None);
        assert_eq!(t.check("nope", 59), None);
    }

    #[test]
    fn new_secrets_are_160_bits_of_base32() {
        let s = new_totp_secret();
        assert_eq!(s.len(), 32);
        assert!(totp_for(&s, "x").is_ok());
    }
}
