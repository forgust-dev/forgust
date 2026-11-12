use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::dto::UserDto;
use crate::auth::{ClientIp, Principal, Viewer, account_open, login_allowed, login_checked, session, session_cookie};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::services::{self, users};
use crate::state::AppState;

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Username or email.
    pub login: String,
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct SessionInfo {
    pub user: UserDto,
    /// Send back as `X-CSRF-Token` on POST/PUT/DELETE.
    pub csrf_token: String,
    /// The administrator signed in as `user` (impersonation), if any.
    pub impersonator: Option<UserDto>,
}

#[derive(Serialize, ToSchema)]
pub struct LoginResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionInfo>,
    pub two_factor_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
}

fn user_agent(h: &HeaderMap) -> Option<String> {
    h.get(axum::http::header::USER_AGENT).and_then(|v| v.to_str().ok()).map(|s| s.chars().take(255).collect())
}

async fn start_session(
    state: &AppState,
    jar: CookieJar,
    headers: &HeaderMap,
    ip: ClientIp,
    user: &forgust_db::users::User,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let ip = ip.text();
    let s = session::create(state, user.id, ip.as_deref(), user_agent(headers).as_deref()).await?;
    services::audit(
        state,
        &Principal::User { user: user.clone(), scopes: None },
        "login",
        Some(("user", user.id)),
        json!({}),
        ip.as_deref(),
    )
    .await;
    Ok((
        jar.add(s.cookie),
        Json(LoginResponse {
            session: Some(SessionInfo { user: UserDto::from(user, true), csrf_token: s.csrf, impersonator: None }),
            two_factor_required: false,
            challenge: None,
        }),
    ))
}

#[utoipa::path(post, path = "/api/v1/auth/login", request_body = LoginRequest, responses((status = 200, body = LoginResponse)), tag = "auth")]
pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Json(req): Json<LoginRequest>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let ip_text = ip.text().unwrap_or_default();
    if !login_allowed(&state, ip, &req.login) {
        return Err(ApiError::forbidden("too many attempts, try again later"));
    }
    let Some((id, Some(hash))) = forgust_db::users::credentials(&state.db, &req.login).await? else {
        crypto::check_password(&req.password, None).await;
        login_checked(&state, ip, &req.login, None, false);
        return Err(ApiError::Unauthorized);
    };
    if !account_open(&state, ip, id).await? {
        return Err(ApiError::forbidden("too many attempts, try again later"));
    }
    if !crypto::check_password(&req.password, Some(hash)).await {
        login_checked(&state, ip, &req.login, Some(id), false);
        services::audit(&state, &Principal::Anonymous, "login_failed", Some(("user", id)), json!({}), Some(&ip_text)).await;
        return Err(ApiError::Unauthorized);
    }
    login_checked(&state, ip, &req.login, Some(id), true);
    let user = forgust_db::users::find(&state.db, id).await?.ok_or(ApiError::Unauthorized)?;
    if !user.is_active() {
        return Err(ApiError::forbidden("this account is blocked"));
    }
    if user.totp_enabled {
        let challenge = crypto::random_urlsafe(32);
        forgust_db::sessions::create_challenge(&state.db, &crypto::token_hash(&challenge), user.id, Utc::now() + Duration::minutes(5))
            .await?;
        return Ok((jar, Json(LoginResponse { session: None, two_factor_required: true, challenge: Some(challenge) })));
    }
    start_session(&state, jar, &headers, ip, &user).await
}

#[derive(Deserialize, ToSchema)]
pub struct TotpLogin {
    pub challenge: String,
    pub code: String,
}

#[utoipa::path(post, path = "/api/v1/auth/login/totp", request_body = TotpLogin, responses((status = 200, body = LoginResponse)), tag = "auth")]
pub async fn login_totp(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Json(req): Json<TotpLogin>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let hash = crypto::token_hash(&req.challenge);
    let user_id = forgust_db::sessions::find_challenge(&state.db, &hash).await?.ok_or(ApiError::Unauthorized)?;
    if !state.login_limiter.allow(&format!("totp|{user_id}")) {
        return Err(ApiError::forbidden("too many attempts, try again in a minute"));
    }
    let user = forgust_db::users::find(&state.db, user_id).await?.ok_or(ApiError::Unauthorized)?;
    // wrong codes: 5 per 10 minutes per account; then the challenge is gone
    // and the password is asked again
    let acct = format!("totp|{}", user.id);
    if state.totp_failures.exceeded(&acct) {
        forgust_db::sessions::delete_challenge(&state.db, &hash).await?;
        return Err(ApiError::forbidden("too many wrong codes: sign in again in a few minutes"));
    }
    if !session::verify_totp(&state, &user, &req.code).await? {
        state.totp_failures.allow(&acct);
        return Err(ApiError::Unauthorized);
    }
    forgust_db::sessions::delete_challenge(&state.db, &hash).await?;
    start_session(&state, jar, &headers, ip, &user).await
}

/// An administrator signs in as someone else, for an hour (Admin > Users), to
/// see what they see. Only an active person who is not an administrator; only
/// from the administrator's own browser session. The administrator's session
/// ends; stopping starts a new one. While it lasts, nothing that mints
/// credentials or changes the account works, and the audit log names the
/// administrator on every change.
pub async fn impersonate(
    State(state): State<AppState>,
    crate::auth::Admin(admin, p): crate::auth::Admin,
    Path(id): Path<i64>,
    ip: ClientIp,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<serde_json::Value>)> {
    if !matches!(p, Principal::User { scopes: None, .. }) {
        return Err(ApiError::forbidden("start it from your own browser session"));
    }
    let u = forgust_db::users::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    if u.id == admin.id {
        return Err(ApiError::bad("you are yourself already"));
    }
    if u.is_bot() || u.is_admin || !u.is_active() {
        return Err(ApiError::bad("only an active person who is not an administrator can be impersonated"));
    }
    let ip_text = ip.text();
    let s = session::create_impersonation(&state, u.id, admin.id, ip_text.as_deref()).await?;
    if let Some(c) = session_cookie(&state.cfg, &jar) {
        session::destroy(&state, c).await?;
    }
    services::audit(&state, &p, "impersonation_started", Some(("user", u.id)), json!({ "username": u.username }), ip_text.as_deref()).await;
    Ok((jar.add(s.cookie), Json(json!({ "impersonating": u.username }))))
}

/// End an impersonation: back to the administrator's own (new) session.
pub async fn stop_impersonating(
    State(state): State<AppState>,
    Viewer(p): Viewer,
    ip: ClientIp,
    headers: HeaderMap,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<serde_json::Value>)> {
    let Some(admin_id) = p.impersonator() else { return Err(ApiError::bad("you are not signed in as someone else")) };
    let ip_text = ip.text();
    services::audit(&state, &p, "impersonation_stopped", p.direct_user().map(|u| ("user", u.id)), json!({}), ip_text.as_deref()).await;
    if let Some(c) = session_cookie(&state.cfg, &jar) {
        session::destroy(&state, c).await?;
    }
    match forgust_db::users::find(&state.db, admin_id).await?.filter(|a| a.is_active() && a.is_admin) {
        Some(a) => {
            let s = session::create(&state, a.id, ip_text.as_deref(), user_agent(&headers).as_deref()).await?;
            // not a fresh sign-in: changes that need one ask for the password again
            forgust_db::sessions::mark_not_recent(&state.db, &crypto::token_hash(s.cookie.value())).await?;
            Ok((jar.add(s.cookie), Json(json!({ "back_as": a.username }))))
        }
        None => Ok((jar.add(session::clear_cookie(&state)), Json(json!({ "back_as": null })))),
    }
}

pub async fn logout(State(state): State<AppState>, Viewer(_): Viewer, jar: CookieJar) -> ApiResult<(CookieJar, Json<serde_json::Value>)> {
    if let Some(c) = session_cookie(&state.cfg, &jar) {
        session::destroy(&state, c).await?;
    }
    Ok((jar.add(session::clear_cookie(&state)), Json(json!({ "ok": true }))))
}

#[derive(Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub username: String,
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub password: String,
}

pub async fn register(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let settings = forgust_db::settings::get(&state.db).await?;
    if !settings.signup_enabled {
        return Err(ApiError::forbidden("sign-up is disabled; ask an administrator for an account"));
    }
    // each sign-up costs a password hash and sends an email
    if !state.login_limiter.allow(&format!("register|{}", ip.text().unwrap_or_default())) {
        return Err(ApiError::TooMany("too many sign-ups from this address; wait a minute".into()));
    }
    let mut tx = state.db.begin().await?;
    let user = users::create(
        &mut tx,
        users::CreateUser {
            username: &req.username,
            email: &req.email,
            name: &req.name,
            password: Some(&req.password),
            is_admin: false,
            email_verified: false,
        },
    )
    .await?;
    tx.commit().await?;
    services::accounts::send_confirmation(&state, &user, &user.email).await?;
    start_session(&state, jar, &headers, ip, &user).await
}

/// Current session (for the web app on load).
#[utoipa::path(get, path = "/api/v1/session", responses((status = 200, body = SessionInfo)), tag = "auth")]
pub async fn session(State(state): State<AppState>, jar: CookieJar) -> ApiResult<Json<SessionInfo>> {
    let c = session_cookie(&state.cfg, &jar).ok_or(ApiError::Unauthorized)?;
    let (user, csrf, impersonator) = session::lookup(&state, c).await?.ok_or(ApiError::Unauthorized)?;
    let impersonator = match impersonator {
        Some(id) => forgust_db::users::find(&state.db, id).await?.map(|a| UserDto::from(&a, false)),
        None => None,
    };
    Ok(Json(SessionInfo { user: UserDto::from(&user, true), csrf_token: csrf, impersonator }))
}

#[derive(Serialize, ToSchema)]
pub struct Provider {
    pub name: String,
    pub display_name: String,
}

pub async fn providers(State(state): State<AppState>) -> Json<Vec<Provider>> {
    Json(state.oidc.providers.iter().map(|(k, v)| Provider { name: k.clone(), display_name: v.display_name.clone() }).collect())
}

pub async fn oidc_start(State(state): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> ApiResult<(CookieJar, Redirect)> {
    let (url, login_state) = crate::auth::oidc::start(&state, &name).await?;
    let cookie = axum_extra::extract::cookie::Cookie::build((crate::auth::oidc::STATE_COOKIE, login_state))
        .path("/api/v1/auth/oidc/")
        .http_only(true)
        .secure(state.cfg.secure_cookies())
        // the provider sends the browser back with a top-level GET: Lax carries it
        .same_site(axum_extra::extract::cookie::SameSite::Lax)
        .max_age(time::Duration::minutes(10))
        .build();
    Ok((jar.add(cookie), Redirect::to(&url)))
}

#[derive(Deserialize)]
pub struct OidcCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn oidc_callback(
    State(state): State<AppState>,
    Path(name): Path<String>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Query(q): Query<OidcCallback>,
) -> ApiResult<Response> {
    if let Some(e) = q.error {
        return Ok(Redirect::to(&format!(
            "/login?error={}",
            percent_encoding::utf8_percent_encode(&e, percent_encoding::NON_ALPHANUMERIC)
        ))
        .into_response());
    }
    let (code, csrf) = (q.code.ok_or_else(|| ApiError::bad("missing code"))?, q.state.ok_or_else(|| ApiError::bad("missing state"))?);
    let started_here = jar.get(crate::auth::oidc::STATE_COOKIE).is_some_and(|c| crypto::eq_ct(c.value(), &csrf));
    if !started_here {
        return Err(ApiError::bad("this sign-in was not started in this browser; start it again"));
    }
    let jar = jar.remove(axum_extra::extract::cookie::Cookie::build(crate::auth::oidc::STATE_COOKIE).path("/api/v1/auth/oidc/"));
    let identity = crate::auth::oidc::finish(&state, &name, &code, &csrf).await?;
    let provider = state.oidc.providers.get(&name).ok_or(ApiError::NotFound)?;
    let user = match forgust_db::users::find_identity(&state.db, &name, &identity.subject).await? {
        Some(uid) => forgust_db::users::find(&state.db, uid).await?.ok_or(ApiError::Unauthorized)?,
        None => {
            // link to an existing account by verified email, else create one if allowed
            let by_email = match &identity.email {
                Some(e) => forgust_db::users::find_by_email(&state.db, e).await?,
                None => None,
            };
            let user = match by_email {
                // only an account whose own address was confirmed: never hand one to
                // whoever controls this email at the provider (account pre-hijacking)
                Some(u) if provider.link_existing_by_email && u.email_verified() => u,
                Some(_) => {
                    return Err(ApiError::forbidden(
                        "an account with this email already exists; sign in with it, or ask an administrator to link this provider",
                    ));
                }
                None if provider.auto_create => {
                    let email =
                        identity.email.clone().ok_or_else(|| ApiError::forbidden("the provider did not return a verified email"))?;
                    let base = identity.username.clone().unwrap_or_else(|| email.split('@').next().unwrap_or("user").to_string());
                    let mut username = crate::services::paths::slug(&base);
                    if username.is_empty() || crate::services::paths::validate_top_level(&username).is_err() {
                        username = format!("user-{}", crypto::random_urlsafe(4).to_lowercase());
                    }
                    if forgust_db::routes::path_taken(&state.db, &username).await? {
                        username = format!("{username}-{}", crypto::random_urlsafe(3).to_lowercase().replace(['-', '_'], "x"));
                    }
                    let mut tx = state.db.begin().await?;
                    let u = users::create(
                        &mut tx,
                        users::CreateUser {
                            username: &username,
                            email: &email,
                            name: identity.name.as_deref().unwrap_or(""),
                            password: None,
                            is_admin: false,
                            // the provider returned it as verified
                            email_verified: true,
                        },
                    )
                    .await?;
                    tx.commit().await?;
                    u
                }
                None => return Err(ApiError::forbidden("no account matches this identity; ask an administrator")),
            };
            forgust_db::users::add_identity(&state.db, user.id, &name, &identity.subject).await?;
            user
        }
    };
    if !user.is_active() {
        return Err(ApiError::forbidden("this account is blocked"));
    }
    // the account's second factor still applies: the provider's sign-in
    // stands in for the password only. The challenge travels in the fragment
    // (never sent to a server, never in a Referer).
    if user.totp_enabled {
        let challenge = crypto::random_urlsafe(32);
        forgust_db::sessions::create_challenge(&state.db, &crypto::token_hash(&challenge), user.id, Utc::now() + Duration::minutes(5))
            .await?;
        return Ok((jar, Redirect::to(&format!("/login#totp={challenge}"))).into_response());
    }
    let (jar, _) = start_session(&state, jar, &headers, ip, &user).await?;
    Ok((jar, Redirect::to("/")).into_response())
}

#[derive(Serialize, ToSchema)]
pub struct SetupStatus {
    pub needs_setup: bool,
}

pub async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<SetupStatus>> {
    Ok(Json(SetupStatus { needs_setup: forgust_db::users::count_humans(&state.db).await? == 0 }))
}

#[derive(Deserialize, ToSchema)]
pub struct SetupRequest {
    /// The one-time token printed in the server log on first start.
    pub token: String,
    pub username: String,
    pub email: String,
    pub password: String,
}

/// Create the first administrator (only while there are no users).
pub async fn setup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Json(req): Json<SetupRequest>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let expected = state.setup_token.lock().unwrap().clone();
    let Some(expected) = expected else { return Err(ApiError::forbidden("setup is already complete")) };
    if !crypto::eq_ct(&req.token, &expected) {
        return Err(ApiError::forbidden("invalid setup token"));
    }
    let mut tx = state.db.begin().await?;
    if forgust_db::users::count_humans(&mut *tx).await? > 0 {
        return Err(ApiError::forbidden("setup is already complete"));
    }
    let user = users::create(
        &mut tx,
        users::CreateUser {
            username: &req.username,
            email: &req.email,
            name: "",
            password: Some(&req.password),
            is_admin: true,
            email_verified: true,
        },
    )
    .await?;
    tx.commit().await?;
    *state.setup_token.lock().unwrap() = None;
    services::audit(
        &state,
        &Principal::User { user: user.clone(), scopes: None },
        "first_admin_created",
        Some(("user", user.id)),
        json!({}),
        None,
    )
    .await;
    start_session(&state, jar, &headers, ip, &user).await
}

#[derive(Deserialize, ToSchema)]
pub struct ForgotPassword {
    /// Username or email.
    pub login: String,
}

/// Email a reset link when the account exists; the answer is the same either way.
#[utoipa::path(post, path = "/api/v1/auth/password/forgot", request_body = ForgotPassword, responses((status = 200)), tag = "auth")]
pub async fn forgot_password(
    State(state): State<AppState>,
    ip: ClientIp,
    Json(req): Json<ForgotPassword>,
) -> ApiResult<Json<serde_json::Value>> {
    let login = req.login.trim().to_lowercase();
    if login.is_empty() || login.len() > 255 {
        return Err(ApiError::bad("enter your username or email"));
    }
    // per client and per account: no flooding an inbox, no probing at speed
    if !state.login_limiter.allow(&format!("forgot|{}", ip.text().unwrap_or_default()))
        || !state.login_failures.allow(&format!("forgot|{login}"))
    {
        return Err(ApiError::forbidden("too many attempts, try again later"));
    }
    services::accounts::forgot(&state, &login).await?;
    Ok(Json(json!({ "sent": true })))
}

#[derive(Deserialize, ToSchema)]
pub struct ResetPassword {
    pub token: String,
    pub password: String,
}

/// Choose a new password with the emailed link (signs out every session).
#[utoipa::path(post, path = "/api/v1/auth/password/reset", request_body = ResetPassword, responses((status = 200)), tag = "auth")]
pub async fn reset_password(
    State(state): State<AppState>,
    ip: ClientIp,
    Json(req): Json<ResetPassword>,
) -> ApiResult<Json<serde_json::Value>> {
    if !state.login_limiter.allow(&format!("reset|{}", ip.text().unwrap_or_default())) {
        return Err(ApiError::TooMany("too many tries; wait a minute".into()));
    }
    let user = services::accounts::reset(&state, req.token.trim(), &req.password).await?;
    Ok(Json(json!({ "username": user.username })))
}

#[derive(Deserialize, ToSchema)]
pub struct ConfirmEmail {
    pub token: String,
}

/// Confirm an address with the emailed link.
#[utoipa::path(post, path = "/api/v1/auth/email/confirm", request_body = ConfirmEmail, responses((status = 200)), tag = "auth")]
pub async fn confirm_email(
    State(state): State<AppState>,
    crate::auth::Viewer(v): crate::auth::Viewer,
    Json(req): Json<ConfirmEmail>,
) -> ApiResult<Json<serde_json::Value>> {
    let me = v.direct_user().filter(|_| matches!(v, crate::auth::Principal::User { scopes: None, .. })).map(|u| u.id);
    let user = services::accounts::confirm(&state, req.token.trim(), me).await?;
    Ok(Json(json!({ "email": user.email })))
}
