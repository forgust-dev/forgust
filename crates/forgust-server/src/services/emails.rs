//! More addresses per account, besides its own: a confirmed one links the
//! commits, imported people and signed commits that carry it, like the
//! account's own address. Each is confirmed by a link mailed to it; on an
//! instance that sends no email, by an administrator vouching for it, or at
//! once when the instance trusts its people's addresses (the
//! `auto_confirm_emails` setting, only while sign-up is closed: with open
//! sign-up anyone could claim someone's address and become their code
//! owner). An address is one account's only, confirmed or not.

use chrono::{Duration, Utc};
use forgust_db::user_emails::{self as edb, UserEmail};
use forgust_db::users::User;
use serde_json::json;

use crate::auth::Principal;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// Extra addresses per account: unconfirmed ones reserve their address too.
pub const MAX_PER_ACCOUNT: i64 = 20;
const CONFIRM_HOURS: i64 = 24;

/// How an address `user` adds is confirmed as they add it, if it is: only on
/// an instance that sends no email (else its link does it). An
/// administrator answers for their own; anyone else, when the instance
/// trusts its people's addresses, sign-up is closed and their own address is
/// confirmed (an account left from when sign-up was open is not trusted).
pub async fn confirmed_as_added(state: &AppState, user: &User) -> ApiResult<Option<&'static str>> {
    if state.cfg.email.enabled() {
        return Ok(None);
    }
    if user.is_admin {
        return Ok(Some("admin"));
    }
    let s = forgust_db::settings::get(&state.db).await?;
    Ok((s.auto_confirm_emails && !s.signup_enabled && user.email_verified()).then_some("auto"))
}

/// Refused when `email` is somebody's already (`user`'s included), as their
/// own address or an extra one, confirmed or not.
pub async fn check_free(state: &AppState, user: &User, email: &str) -> ApiResult<()> {
    match edb::taken_by(&state.db, email).await? {
        None => Ok(()),
        Some(id) if id == user.id && email.eq_ignore_ascii_case(&user.email) => {
            Err(ApiError::conflict("this is your account's own address already"))
        }
        Some(id) if id == user.id => Err(ApiError::conflict("this address is in your list already")),
        Some(_) => Err(ApiError::conflict("email is already in use")),
    }
}

/// Add `email` to `user`'s addresses: confirmed at once as `confirmed_by`
/// says, else waiting for the link mailed to it (none without email: an
/// administrator confirms it).
pub async fn add(state: &AppState, by: &Principal, user: &User, email: &str, confirmed_by: Option<&str>) -> ApiResult<UserEmail> {
    super::paths::validate_email(email)?;
    if edb::count(&state.db, user.id).await? >= MAX_PER_ACCOUNT {
        return Err(ApiError::bad(format!("an account has {MAX_PER_ACCOUNT} other addresses at most: remove one first")));
    }
    check_free(state, user, email).await?;
    let row = edb::insert(&state.db, user.id, email, confirmed_by)
        .await
        .map_err(|e| if forgust_db::is_unique_violation(&e) { ApiError::conflict("email is already in use") } else { e.into() })?;
    super::audit(state, by, "email_added", Some(("user", user.id)), json!({ "email_id": row.id, "confirmed_by": confirmed_by }), None)
        .await;
    if row.confirmed_at.is_none() {
        send_link(state, user, &row).await?;
    }
    Ok(row)
}

/// Mail the link confirming `row` (an earlier one for it stops working).
/// Nothing when the instance sends no email.
pub async fn send_link(state: &AppState, user: &User, row: &UserEmail) -> ApiResult<()> {
    if !state.cfg.email.enabled() {
        return Ok(());
    }
    let token = crypto::random_urlsafe(32);
    let mut tx = state.db.begin().await?;
    edb::delete_tokens(&mut *tx, user.id, &row.email).await?;
    forgust_db::users::add_token(
        &mut *tx,
        &crypto::token_hash(&token),
        user.id,
        "extra_email",
        Some(&row.email),
        Utc::now() + Duration::hours(CONFIRM_HOURS),
    )
    .await?;
    tx.commit().await?;
    super::notify::send_to(
        state,
        &row.email,
        "Confirm your email address",
        &format!(
            "Hello {},\n\nConfirm that {} is one of your addresses on Forgust: the commits made with it will show as yours.\n\n{}\n\nThe link works once, within {CONFIRM_HOURS} hours. If you did not ask for this, ignore this email.\n",
            user.username,
            row.email,
            state.cfg.web_url(&format!("verify-email?token={token}"))
        ),
    )
    .await;
    Ok(())
}

/// Confirm `user`'s address `id` without a link (an administrator vouches
/// for it).
pub async fn confirm_by_admin(state: &AppState, by: &Principal, user: &User, id: i64) -> ApiResult<UserEmail> {
    let row = edb::find(&state.db, user.id, id).await?.ok_or(ApiError::NotFound)?;
    if row.confirmed_at.is_some() {
        return Err(ApiError::conflict("this address is already confirmed"));
    }
    let mut tx = state.db.begin().await?;
    edb::confirm(&mut *tx, row.id, "admin").await?;
    edb::delete_tokens(&mut *tx, user.id, &row.email).await?;
    tx.commit().await?;
    super::audit(state, by, "email_confirmed", Some(("user", user.id)), json!({ "by": "administrator", "email_id": row.id }), None).await;
    edb::find(&state.db, user.id, id).await?.ok_or(ApiError::NotFound)
}

/// Confirm the extra address a link was mailed for (`accounts::confirm`
/// checked who is signed in). None: the account no longer has it.
pub async fn confirm_by_link(e: &mut sqlx::PgConnection, user: &User, email: &str) -> ApiResult<Option<UserEmail>> {
    let Some(row) = edb::find_by_email(&mut *e, user.id, email).await? else { return Ok(None) };
    edb::confirm(&mut *e, row.id, "link").await?;
    Ok(Some(row))
}

/// Remove `user`'s address `id`: commits made with it stop showing as
/// theirs (imported issues and merge requests it matched stay theirs).
pub async fn remove(state: &AppState, by: &Principal, user: &User, id: i64) -> ApiResult<()> {
    let mut tx = state.db.begin().await?;
    let row = edb::delete(&mut *tx, user.id, id).await?.ok_or(ApiError::NotFound)?;
    edb::delete_tokens(&mut *tx, user.id, &row.email).await?;
    tx.commit().await?;
    let admin = by.direct_user().is_none_or(|u| u.id != user.id);
    super::audit(
        state,
        by,
        "email_removed",
        Some(("user", user.id)),
        json!({ "email_id": row.id, "by": admin.then_some("administrator") }),
        None,
    )
    .await;
    Ok(())
}
