//! Password reset and email confirmation by emailed link (M6).
//!
//! Links carry a random token; only its hash is stored, it works once, and
//! expires (1 hour for a reset, 24 hours for a confirmation). "Forgot password"
//! answers the same whether or not the account exists. A reset signs out every
//! session of the account.

use chrono::{Duration, Utc};
use forgust_db::users::{self as udb, User};
use serde_json::json;

use crate::auth::Principal;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

const RESET_HOURS: i64 = 1;
const CONFIRM_HOURS: i64 = 24;

fn link(state: &AppState, page: &str, token: &str) -> String {
    state.cfg.web_url(&format!("{page}?token={token}"))
}

/// Email a link confirming `email` (the account's address or a new one).
pub async fn send_confirmation(state: &AppState, user: &User, email: &str) -> ApiResult<()> {
    let token = crypto::random_urlsafe(32);
    let mut tx = state.db.begin().await?;
    udb::delete_tokens(&mut *tx, user.id, "email").await?;
    udb::add_token(&mut *tx, &crypto::token_hash(&token), user.id, "email", Some(email), Utc::now() + Duration::hours(CONFIRM_HOURS))
        .await?;
    tx.commit().await?;
    // straight to the address being confirmed (not filtered like notifications)
    super::notify::send_to(
        state,
        email,
        "Confirm your email address",
        &format!(
            "Hello {},\n\nConfirm that {email} is your address on Forgust:\n\n{}\n\nThe link works once, within {CONFIRM_HOURS} hours. If you did not ask for this, ignore this email.\n",
            user.username,
            link(state, "verify-email", &token)
        ),
    )
    .await;
    Ok(())
}

/// "Forgot password": a reset link to the account's address, if there is such
/// an active account. Says nothing either way.
pub async fn forgot(state: &AppState, login: &str) -> ApiResult<()> {
    let Some(user) = udb::find_by_login(&state.db, login.trim()).await? else { return Ok(()) };
    if !user.is_active() || user.is_bot() {
        return Ok(());
    }
    let token = crypto::random_urlsafe(32);
    let mut tx = state.db.begin().await?;
    udb::delete_tokens(&mut *tx, user.id, "password_reset").await?;
    udb::add_token(&mut *tx, &crypto::token_hash(&token), user.id, "password_reset", None, Utc::now() + Duration::hours(RESET_HOURS))
        .await?;
    tx.commit().await?;
    super::notify::send_to(
        state,
        &user.email,
        "Reset your password",
        &format!(
            "Hello {},\n\nSomeone asked to reset the password of your Forgust account. To choose a new one, open:\n\n{}\n\nThe link works once, within {RESET_HOURS} hour. If it was not you, ignore this email: your password stays as it is.\n",
            user.username,
            link(state, "reset-password", &token)
        ),
    )
    .await;
    super::audit(state, &Principal::Anonymous, "password_reset_requested", Some(("user", user.id)), json!({}), None).await;
    Ok(())
}

/// Set a new password with a reset link; every session of the account ends.
pub async fn reset(state: &AppState, token: &str, password: &str) -> ApiResult<User> {
    super::users::validate_password(password)?;
    let mut tx = state.db.begin().await?;
    let t = udb::take_token(&mut *tx, &crypto::token_hash(token), "password_reset")
        .await?
        .ok_or_else(|| ApiError::bad("this link is invalid or has expired; ask for a new one"))?;
    // only a real link costs a hash: anonymous guesses must not buy CPU
    let hash = crypto::hash_password_gated(password).await?;
    let user =
        udb::find(&mut *tx, t.user_id).await?.filter(|u| u.is_active()).ok_or_else(|| ApiError::bad("this account cannot sign in"))?;
    udb::set_password(&mut *tx, user.id, &hash).await?;
    forgust_db::sessions::delete_for_user(&mut *tx, user.id).await?;
    // the reset link went to the address: it is the user's, and whatever was
    // set up before anyone proved that (by whoever registered it) goes
    if !user.email_verified() {
        udb::set_email_verified(&mut *tx, user.id).await?;
        udb::forget_credentials(&mut tx, user.id).await?;
    }
    tx.commit().await?;
    super::audit(state, &Principal::Anonymous, "password_reset", Some(("user", user.id)), json!({}), None).await;
    Ok(user)
}

/// Confirm an address with its link: the account's own, or a new one that
/// replaces it. Only the account itself, signed in, may: someone who receives
/// the link for an account another person registered with their address
/// cannot hand that account their address by clicking it.
pub async fn confirm(state: &AppState, token: &str, signed_in: Option<i64>) -> ApiResult<User> {
    let mut tx = state.db.begin().await?;
    let t = udb::take_token(&mut *tx, &crypto::token_hash(token), "email")
        .await?
        .ok_or_else(|| ApiError::bad("this link is invalid or has expired; ask for a new one"))?;
    let user = udb::find(&mut *tx, t.user_id).await?.ok_or(ApiError::NotFound)?;
    if signed_in != Some(user.id) {
        // the link stays usable (the transaction rolls back)
        return Err(ApiError::forbidden(format!(
            "sign in as @{} to confirm this address; if you did not create that account, ignore the email",
            user.username
        )));
    }
    let email = t.email.unwrap_or_default();
    if email.eq_ignore_ascii_case(&user.email) {
        udb::set_email_verified(&mut *tx, user.id).await?;
    } else if user.unconfirmed_email.as_deref().is_some_and(|u| u.eq_ignore_ascii_case(&email)) {
        if udb::find_by_email(&mut *tx, &email).await?.is_some_and(|o| o.id != user.id) {
            return Err(ApiError::conflict("this address is now used by another account"));
        }
        udb::confirm_email(&mut *tx, user.id, &email).await?;
    } else {
        return Err(ApiError::bad("this link is for an address the account no longer uses"));
    }
    tx.commit().await?;
    super::audit(
        state,
        &Principal::User { user: user.clone(), scopes: None },
        "email_confirmed",
        Some(("user", user.id)),
        json!({ "email": email }),
        None,
    )
    .await;
    Ok(udb::find(&state.db, user.id).await?.unwrap_or(user))
}
