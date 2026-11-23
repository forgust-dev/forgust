use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use chrono::{Duration, Utc};
use forgust_db::webauthn::CredentialKind;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, Passkey, PublicKeyCredential, SecurityKey, SecurityKeyAuthentication, WebauthnError,
};

use super::dto::UserDto;
use crate::auth::{ClientIp, Principal, Viewer, account_open, login_allowed, login_checked, session, session_cookie, webauthn};
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
    /// Whether the instance sends email: without it, nothing promises or
    /// waits for an emailed link.
    pub email_enabled: bool,
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
    method: &str,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let ip = ip.text();
    let s = session::create(state, user.id, ip.as_deref(), user_agent(headers).as_deref()).await?;
    services::audit(
        state,
        &Principal::User { user: user.clone(), scopes: None },
        "login",
        Some(("user", user.id)),
        json!({ "method": method }),
        ip.as_deref(),
    )
    .await;
    Ok((
        jar.add(s.cookie),
        Json(LoginResponse {
            session: Some(SessionInfo {
                user: UserDto::from(user, true),
                csrf_token: s.csrf,
                impersonator: None,
                email_enabled: state.cfg.email.enabled(),
            }),
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
        login_failed(&state, id, json!({ "reason": "password" }), ip).await;
        return Err(ApiError::Unauthorized);
    }
    login_checked(&state, ip, &req.login, Some(id), true);
    let user = forgust_db::users::find(&state.db, id).await?.ok_or(ApiError::Unauthorized)?;
    if !user.is_active() {
        blocked_sign_in(&state, &user, "password", ip).await;
        return Err(ApiError::forbidden("this account is blocked"));
    }
    if user.two_factor() {
        let challenge = crypto::random_urlsafe(32);
        forgust_db::sessions::create_challenge(
            &state.db,
            &crypto::token_hash(&challenge),
            user.id,
            None,
            Utc::now() + Duration::minutes(5),
        )
        .await?;
        return Ok((jar, Json(LoginResponse { session: None, two_factor_required: true, challenge: Some(challenge) })));
    }
    start_session(&state, jar, &headers, ip, &user, "password").await
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
    second_factor(state, jar, headers, ip, &req.challenge, Factor::Totp(&req.code)).await
}

#[derive(Deserialize, ToSchema)]
pub struct RecoveryLogin {
    pub challenge: String,
    /// One of the account's recovery codes (case, dashes and spaces do not matter).
    pub code: String,
}

/// The second step with a recovery code instead of the authenticator: the code
/// is used up, and the account's inbox hears of it.
#[utoipa::path(post, path = "/api/v1/auth/login/recovery", request_body = RecoveryLogin, responses((status = 200, body = LoginResponse)), tag = "auth")]
pub async fn login_recovery(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Json(req): Json<RecoveryLogin>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    second_factor(state, jar, headers, ip, &req.challenge, Factor::Recovery(&req.code)).await
}

#[derive(Clone, Copy)]
enum Factor<'a> {
    Totp(&'a str),
    Recovery(&'a str),
    SecurityKey(&'a PublicKeyCredential),
}

impl Factor<'_> {
    fn method(&self) -> &'static str {
        match self {
            Factor::Totp(_) => "totp",
            Factor::Recovery(_) => "recovery_code",
            Factor::SecurityKey(_) => "security_key",
        }
    }
}

/// Finish a sign-in whose password (or provider) is accepted. The challenge
/// stays locked while the answer is checked, so it completes one sign-in at
/// most; wrong answers, whatever the factor, share one limit per account.
async fn second_factor(
    state: AppState,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    challenge: &str,
    factor: Factor<'_>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let hash = crypto::token_hash(challenge);
    let mut tx = state.db.begin().await?;
    let forgust_db::sessions::Challenge { user_id, provider_sync, webauthn_state } =
        forgust_db::sessions::lock_challenge(&mut tx, &hash).await?.ok_or(ApiError::ChallengeExpired)?;
    second_step_allowed(&state, user_id)?;
    let user = forgust_db::users::find(&mut *tx, user_id).await?.ok_or(ApiError::Unauthorized)?;
    // wrong codes: 5 per 10 minutes per account; then the challenge is gone
    // and the password is asked again. Each answer is counted before it is
    // checked (and given back when right): answers sent at once, on several
    // challenges, share the limit too
    let acct = format!("totp|{}", user.id);
    if !state.totp_failures.allow(&acct) {
        forgust_db::sessions::delete_challenge(&mut *tx, &hash).await?;
        tx.commit().await?;
        return Err(ApiError::forbidden("too many wrong codes: sign in again in a few minutes"));
    }
    let checked: ApiResult<bool> = async {
        Ok(match factor {
            // a secret still being set up is no second factor
            Factor::Totp(code) => user.totp_enabled && session::verify_totp(&state, &mut tx, &user, code).await?,
            Factor::Recovery(code) => {
                user.two_factor() && forgust_db::recovery_codes::consume(&mut *tx, user.id, &session::recovery_code_hash(code)).await?
            }
            Factor::SecurityKey(answer) => {
                // what the keys were asked to sign is answered once, right or wrong
                forgust_db::webauthn::set_challenge_state(&mut *tx, &hash, None).await?;
                security_key_signed(&state, &mut tx, &user, webauthn_state, answer).await?
            }
        })
    }
    .await;
    // a check that could not be made (ours to fix) is no wrong answer
    let ok = checked.inspect_err(|_| state.totp_failures.undo(&acct))?;
    if !ok {
        // a wrong answer changes nothing but what it used up
        tx.commit().await?;
        login_failed(&state, user.id, json!({ "reason": "second_factor", "method": factor.method() }), ip).await;
        return Err(ApiError::Unauthorized);
    }
    let codes_left = match factor {
        Factor::Recovery(_) => Some(forgust_db::recovery_codes::remaining(&mut *tx, user.id).await?),
        _ => None,
    };
    forgust_db::sessions::delete_challenge(&mut *tx, &hash).await?;
    tx.commit().await?;
    state.totp_failures.undo(&acct);
    if let Some(left) = codes_left {
        recovery_code_used(&state, &user, left, ip.text().as_deref()).await;
    }
    // a sign-in through a provider, now complete: what it says about the account
    let user = match provider_sync.map(serde_json::from_value::<PendingSync>) {
        Some(Ok(s)) => sync_from_provider(&state, &s.provider, &s.identity, user, ip.text().as_deref()).await?,
        Some(Err(e)) => {
            tracing::warn!(user = user.id, error = %e, "what the provider said about the account could not be read: not applied");
            user
        }
        None => user,
    };
    start_session(&state, jar, &headers, ip, &user, factor.method()).await
}

/// A recovery code stood in for the authenticator: audited, and the account's
/// inbox is told (a code it did not use is a stolen one).
async fn recovery_code_used(state: &AppState, user: &forgust_db::users::User, left: i64, ip: Option<&str>) {
    let p = Principal::User { user: user.clone(), scopes: None };
    services::audit(state, &p, "recovery_code_used", Some(("user", user.id)), json!({ "remaining": left }), ip).await;
    services::notify::send_to(
        state,
        &user.email,
        "A recovery code was used to sign in",
        &format!(
            "Hello {},\n\nSomeone signed in to your Forgust account with your password and one of your recovery codes ({left} left).\n\nIf it was not you, change your password and create new recovery codes (Profile > Two-factor authentication) now.\n",
            user.username
        ),
    )
    .await;
}

#[derive(Deserialize, ToSchema)]
pub struct PendingLogin {
    pub challenge: String,
}

/// The second factors a pending sign-in can be finished with.
#[derive(Serialize, ToSchema)]
pub struct SecondFactors {
    /// A code from the authenticator app.
    pub totp: bool,
    /// A security key: `/auth/login/webauthn/start`, then `/auth/login/webauthn`.
    pub security_key: bool,
    /// A recovery code (some are left).
    pub recovery_code: bool,
}

/// One more answer to a pending sign-in's second step: 10 a minute per account.
fn second_step_allowed(state: &AppState, user_id: i64) -> ApiResult<()> {
    second_step_budget(state, &format!("totp|{user_id}"))
}

/// One more question about a pending sign-in (its factors, a security key's
/// challenge): 10 a minute per account, apart from the answers, since a wrong
/// security key answer is followed by a new question.
fn second_step_question_allowed(state: &AppState, user_id: i64) -> ApiResult<()> {
    second_step_budget(state, &format!("totp-ask|{user_id}"))
}

fn second_step_budget(state: &AppState, key: &str) -> ApiResult<()> {
    if state.login_limiter.allow(key) { Ok(()) } else { Err(ApiError::forbidden("too many attempts, try again in a minute")) }
}

/// What can finish a sign-in whose password (or provider) was accepted.
#[utoipa::path(post, path = "/api/v1/auth/login/second_factor", request_body = PendingLogin, responses((status = 200, body = SecondFactors)), tag = "auth")]
pub async fn second_factors(State(state): State<AppState>, Json(req): Json<PendingLogin>) -> ApiResult<Json<SecondFactors>> {
    let user_id =
        forgust_db::sessions::challenge_user(&state.db, &crypto::token_hash(&req.challenge)).await?.ok_or(ApiError::ChallengeExpired)?;
    second_step_question_allowed(&state, user_id)?;
    let user = forgust_db::users::find(&state.db, user_id).await?.ok_or(ApiError::Unauthorized)?;
    Ok(Json(SecondFactors {
        totp: user.totp_enabled,
        security_key: user.security_keys_enabled && state.webauthn.is_some(),
        recovery_code: forgust_db::recovery_codes::remaining(&state.db, user.id).await? > 0,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct WebauthnChallenge {
    /// Options for `navigator.credentials.get` (WebAuthn).
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
}

/// Ask the account's security keys to sign for a pending sign-in (asking
/// again replaces the question); the answer goes to `/auth/login/webauthn`.
#[utoipa::path(post, path = "/api/v1/auth/login/webauthn/start", request_body = PendingLogin, responses((status = 200, body = WebauthnChallenge)), tag = "auth")]
pub async fn security_key_start(State(state): State<AppState>, Json(req): Json<PendingLogin>) -> ApiResult<Json<WebauthnChallenge>> {
    let rp = webauthn::of(&state)?;
    let hash = crypto::token_hash(&req.challenge);
    let user_id = forgust_db::sessions::challenge_user(&state.db, &hash).await?.ok_or(ApiError::ChallengeExpired)?;
    second_step_question_allowed(&state, user_id)?;
    let keys = forgust_db::webauthn::credentials(&state.db, user_id, CredentialKind::SecurityKey, &rp.id).await?;
    let keys: Vec<SecurityKey> = keys.into_iter().map(|k| webauthn::from_json(k.credential)).collect::<ApiResult<_>>()?;
    if keys.is_empty() {
        return Err(ApiError::conflict("this account has no security key"));
    }
    let (options, asked) = rp.start_securitykey_authentication(&keys).map_err(webauthn::internal)?;
    if !forgust_db::webauthn::set_challenge_state(&state.db, &hash, Some(&webauthn::to_json(&asked)?)).await? {
        return Err(ApiError::ChallengeExpired);
    }
    Ok(Json(WebauthnChallenge { options: webauthn::to_json(&options)? }))
}

#[derive(Deserialize, ToSchema)]
pub struct WebauthnLogin {
    pub challenge: String,
    /// What `navigator.credentials.get` returned (`PublicKeyCredential`, as JSON).
    #[schema(value_type = Object)]
    pub credential: PublicKeyCredential,
}

/// The second step with a security key, on what `/auth/login/webauthn/start` asked it to sign.
#[utoipa::path(post, path = "/api/v1/auth/login/webauthn", request_body = WebauthnLogin, responses((status = 200, body = LoginResponse)), tag = "auth")]
pub async fn login_webauthn(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Json(req): Json<WebauthnLogin>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    second_factor(state, jar, headers, ip, &req.challenge, Factor::SecurityKey(&req.credential)).await
}

/// Check a security key's answer for a pending sign-in: it signed what it was
/// asked to, and it is one of this account's security keys. Its counter moves on.
async fn security_key_signed(
    state: &AppState,
    tx: &mut sqlx::PgConnection,
    user: &forgust_db::users::User,
    asked: Option<serde_json::Value>,
    answer: &PublicKeyCredential,
) -> ApiResult<bool> {
    let rp = webauthn::of(state)?;
    let Some(asked) = asked.and_then(|a| webauthn::from_json::<SecurityKeyAuthentication>(a).ok()) else { return Ok(false) };
    let Some(stored) = forgust_db::webauthn::find_by_credential_id(&mut *tx, answer.get_credential_id(), &rp.id).await? else {
        return Ok(false);
    };
    if stored.user_id != user.id || stored.kind != CredentialKind::SecurityKey {
        return Ok(false);
    }
    let result = match rp.finish_securitykey_authentication(answer, &asked) {
        Ok(r) => r,
        Err(e) => {
            credential_refused(state, user, stored.id, &e).await;
            return Ok(false);
        }
    };
    let mut key: SecurityKey = webauthn::from_json(stored.credential)?;
    let moved = key.update_credential(&result);
    webauthn::record_use(tx, stored.id, &key, moved).await?;
    Ok(true)
}

/// A credential's answer was refused. A counter that went back means a copy
/// of the key may exist: audited, for the administrators to see.
async fn credential_refused(state: &AppState, user: &forgust_db::users::User, credential: i64, e: &WebauthnError) {
    tracing::info!(user = user.id, credential, error = %e, "a WebAuthn answer was refused");
    if matches!(e, WebauthnError::CredentialPossibleCompromise) {
        let p = Principal::User { user: user.clone(), scopes: None };
        services::audit(state, &p, "webauthn_clone_suspected", Some(("user", user.id)), json!({ "credential": credential }), None).await;
    }
}

#[derive(Serialize, ToSchema)]
pub struct PasskeyStart {
    /// Send back with the answer.
    pub ceremony: String,
    /// Options for `navigator.credentials.get` (WebAuthn).
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
}

/// The client's budget of passkey sign-ins, under a login no account can have.
const PASSKEY_BUDGET: &str = " passkey";

/// Start a sign-in with a passkey: no username, the authenticator offers the
/// passkeys it holds for this site.
#[utoipa::path(post, path = "/api/v1/auth/passkey/start", responses((status = 200, body = PasskeyStart)), tag = "auth")]
pub async fn passkey_start(State(state): State<AppState>, ip: ClientIp) -> ApiResult<Json<PasskeyStart>> {
    let rp = webauthn::of(&state)?;
    if !login_allowed(&state, ip, PASSKEY_BUDGET) {
        return Err(ApiError::forbidden("too many attempts, try again later"));
    }
    let (mut options, asked) = rp.start_discoverable_authentication().map_err(webauthn::internal)?;
    // asked for with a button, not offered in the username field's autofill
    options.mediation = None;
    let ceremony = crypto::random_urlsafe(32);
    forgust_db::webauthn::save_ceremony(
        &state.db,
        &crypto::token_hash(&ceremony),
        forgust_db::webauthn::PASSKEY_LOGIN,
        None,
        None,
        &webauthn::to_json(&asked)?,
        Utc::now() + Duration::minutes(5),
    )
    .await?;
    Ok(Json(PasskeyStart { ceremony, options: webauthn::to_json(&options)? }))
}

#[derive(Deserialize, ToSchema)]
pub struct PasskeyFinish {
    pub ceremony: String,
    /// What `navigator.credentials.get` returned (`PublicKeyCredential`, as JSON).
    #[schema(value_type = Object)]
    pub credential: PublicKeyCredential,
}

/// Sign in with a passkey. It verified its user (PIN, biometrics): no password
/// and no second factor are asked. Every refusal looks the same.
#[utoipa::path(post, path = "/api/v1/auth/passkey/finish", request_body = PasskeyFinish, responses((status = 200, body = LoginResponse)), tag = "auth")]
pub async fn passkey_finish(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Json(req): Json<PasskeyFinish>,
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let rp = webauthn::of(&state)?;
    if ip.client().is_some_and(|c| state.login_ip_failures.exceeded(&c)) {
        return Err(ApiError::forbidden("too many attempts, try again later"));
    }
    let refused = |user_id: Option<i64>| {
        login_checked(&state, ip, PASSKEY_BUDGET, user_id, false);
        ApiError::Unauthorized
    };
    // used up whatever comes next: an answer is checked once
    let ceremony = forgust_db::webauthn::take_ceremony(&state.db, &crypto::token_hash(&req.ceremony)).await?;
    let Some(c) = ceremony.filter(|c| c.kind == forgust_db::webauthn::PASSKEY_LOGIN) else { return Err(refused(None)) };
    let Ok(asked) = webauthn::from_json::<DiscoverableAuthentication>(c.state) else { return Err(refused(None)) };
    let Ok((handle, credential_id)) = rp.identify_discoverable_authentication(&req.credential) else { return Err(refused(None)) };
    let stored = forgust_db::webauthn::find_by_credential_id(&state.db, credential_id, &rp.id).await?;
    let Some(stored) = stored.filter(|s| s.kind == CredentialKind::Passkey) else { return Err(refused(None)) };
    // the authenticator's idea of whose passkey it is must be the account's
    if stored.user_handle != Some(handle) {
        passkey_refused(&state, stored.user_id, stored.id, ip).await;
        return Err(refused(Some(stored.user_id)));
    }
    if !account_open(&state, ip, stored.user_id).await? {
        return Err(ApiError::forbidden("too many attempts, try again later"));
    }
    let user = forgust_db::users::find(&state.db, stored.user_id).await?.ok_or(ApiError::Unauthorized)?;
    let mut passkey: Passkey = webauthn::from_json(stored.credential)?;
    let result = match rp.finish_discoverable_authentication_v2(&req.credential, asked, &passkey) {
        Ok(r) if r.user_verified() => r,
        Ok(_) => {
            passkey_refused(&state, user.id, stored.id, ip).await;
            return Err(refused(Some(user.id)));
        }
        Err(e) => {
            credential_refused(&state, &user, stored.id, &e).await;
            passkey_refused(&state, user.id, stored.id, ip).await;
            return Err(refused(Some(user.id)));
        }
    };
    if !user.is_active() || user.is_bot() {
        blocked_sign_in(&state, &user, "passkey", ip).await;
        return Err(ApiError::forbidden("this account is blocked"));
    }
    let moved = passkey.update_credential(&result);
    webauthn::record_use(&mut *state.db.acquire().await?, stored.id, &passkey, moved).await?;
    login_checked(&state, ip, PASSKEY_BUDGET, Some(user.id), true);
    start_session(&state, jar, &headers, ip, &user, "passkey").await
}

/// A refused sign-in to this account, in the audit log.
async fn login_failed(state: &AppState, user_id: i64, details: serde_json::Value, ip: ClientIp) {
    services::audit(state, &Principal::Anonymous, "login_failed", Some(("user", user_id)), details, ip.text().as_deref()).await;
}

/// A passkey that did not sign in (wrong account, not verified, refused).
async fn passkey_refused(state: &AppState, user_id: i64, credential: i64, ip: ClientIp) {
    login_failed(state, user_id, json!({ "reason": "passkey", "credential": credential }), ip).await;
}

/// Someone got past the first step for a blocked account, or a bot's (bots
/// do not sign in).
async fn blocked_sign_in(state: &AppState, user: &forgust_db::users::User, method: &str, ip: ClientIp) {
    let reason = if user.is_bot() { "bot" } else { "blocked" };
    login_failed(state, user.id, json!({ "reason": reason, "method": method }), ip).await;
}

/// A provider sign-in waiting for its second factor (`login_challenges.provider_sync`).
#[derive(Serialize, Deserialize)]
struct PendingSync {
    provider: String,
    identity: crate::auth::oidc::OidcIdentity,
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
    services::audit(&state, &p, "impersonation_started", Some(("user", u.id)), json!({}), ip_text.as_deref()).await;
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

pub async fn logout(
    State(state): State<AppState>,
    Viewer(p): Viewer,
    ip: ClientIp,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<serde_json::Value>)> {
    if let Some(c) = session_cookie(&state.cfg, &jar) {
        session::destroy(&state, c).await?;
        if let Some(u) = p.direct_user() {
            services::audit(&state, &p, "logout", Some(("user", u.id)), json!({}), ip.text().as_deref()).await;
        }
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
    if !state.login_limiter.allow(&format!("register|{}", ip.client().unwrap_or_default())) {
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
    start_session(&state, jar, &headers, ip, &user, "registration").await
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
    Ok(Json(SessionInfo { user: UserDto::from(&user, true), csrf_token: csrf, impersonator, email_enabled: state.cfg.email.enabled() }))
}

#[derive(Serialize, ToSchema)]
pub struct Provider {
    pub name: String,
    pub display_name: String,
}

pub async fn providers(State(state): State<AppState>) -> Json<Vec<Provider>> {
    Json(state.oidc.providers.iter().map(|(k, v)| Provider { name: k.clone(), display_name: v.display_name.clone() }).collect())
}

/// A sign-in through a provider that did not work out: back to the sign-in
/// page, which shows why (the browser is on a link, not in the app).
fn sign_in_failed(e: ApiError) -> Redirect {
    let why = match &e {
        ApiError::Internal(cause) => {
            tracing::error!(error = ?cause, "provider sign-in failed");
            "signing in through the provider failed; try again, or tell an administrator".to_string()
        }
        e => e.to_string(),
    };
    Redirect::to(&format!("/login?error={}", query_value(&why)))
}

pub async fn oidc_start(State(state): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Response {
    match crate::auth::oidc::start(&state, &name, None).await {
        Ok((url, login_state)) => (jar.add(crate::auth::oidc::state_cookie(&state, login_state)), Redirect::to(&url)).into_response(),
        Err(e) => sign_in_failed(e).into_response(),
    }
}

/// What the provider says about the account, taken over at each sign-in:
/// administrator rights (`admin_groups`), name and email (`sync_profile`).
/// Only once the sign-in is complete: with two-factor authentication on, the
/// provider alone must not be able to move the account to another inbox.
async fn sync_from_provider(
    state: &AppState,
    provider_name: &str,
    identity: &crate::auth::oidc::OidcIdentity,
    user: forgust_db::users::User,
    ip: Option<&str>,
) -> ApiResult<forgust_db::users::User> {
    let Some(provider) = state.oidc.providers.get(provider_name) else { return Ok(user) };
    let me = Principal::User { user: user.clone(), scopes: None };
    let mut changed = false;
    if !provider.admin_groups.is_empty() && !user.is_bot() {
        let admin = identity.groups.iter().any(|g| provider.admin_groups.contains(g));
        // never an external one in, never the last administrator out
        let allowed =
            admin != user.is_admin && if admin { !user.is_external } else { forgust_db::admin::active_admins(&state.db).await? > 1 };
        if allowed {
            forgust_db::users::set_admin(&state.db, user.id, admin).await?;
            let action = if admin { "admin_granted" } else { "admin_revoked" };
            services::audit(state, &me, action, Some(("user", user.id)), json!({ "provider": provider_name }), ip).await;
            changed = true;
        }
    }
    if provider.sync_profile {
        let name = identity.name.as_deref().map(str::trim).filter(|n| *n != user.name && services::paths::validate_name(n).is_ok());
        let mut email =
            identity.email.as_deref().filter(|e| !e.eq_ignore_ascii_case(&user.email) && services::paths::validate_email(e).is_ok());
        // another account's, or one of this account's other addresses
        if let Some(e) = email
            && forgust_db::user_emails::reserved_by(&state.db, e).await?.is_some()
        {
            tracing::warn!(user = user.id, provider = provider_name, "the provider's email is already in use: kept the old one");
            email = None;
        }
        if let Some(n) = name {
            forgust_db::users::update_profile(&state.db, user.id, n, &user.email).await?;
        }
        if let Some(e) = email {
            // the provider returned it as verified
            forgust_db::users::confirm_email(&state.db, user.id, e).await?;
            // links sent to the old address prove nothing about this one
            forgust_db::users::delete_tokens(&state.db, user.id, "email").await?;
            forgust_db::users::delete_tokens(&state.db, user.id, "password_reset").await?;
        }
        if name.is_some() || email.is_some() {
            let details = json!({ "provider": provider_name, "name": name.is_some(), "email": email.is_some() });
            services::audit(state, &me, "profile_synced", Some(("user", user.id)), details, ip).await;
            changed = true;
        }
    }
    if !changed {
        return Ok(user);
    }
    forgust_db::users::find(&state.db, user.id).await?.ok_or(ApiError::Unauthorized)
}

#[derive(Deserialize)]
pub struct OidcCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn query_value(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// Where a link started from the settings ends, with its outcome.
fn link_outcome(name: &str, done: ApiResult<()>) -> Redirect {
    match done {
        Ok(()) => Redirect::to(&format!("/profile/security?linked={}", query_value(name))),
        Err(e) => {
            if let ApiError::Internal(e) = &e {
                tracing::error!(error = ?e, "linking a provider failed");
            }
            Redirect::to(&format!("/profile/security?link_error={}", query_value(&e.to_string())))
        }
    }
}

/// Link the provider identity to the account that asked for it in its settings.
/// The browser must still be signed in as that account (not an administrator
/// acting as it): the callback alone proves nothing about who is at the keyboard.
async fn link_identity(
    state: &AppState,
    jar: &CookieJar,
    name: &str,
    code: &str,
    pending: forgust_db::sessions::OidcState,
    ip: Option<&str>,
) -> ApiResult<()> {
    let session = match session_cookie(&state.cfg, jar) {
        Some(c) => session::lookup(state, c).await?,
        None => None,
    };
    let user = match session {
        Some((u, _, None)) if Some(u.id) == pending.link_user_id => u,
        _ => return Err(ApiError::forbidden("you are no longer signed in as the account that started this; start again")),
    };
    let provider = state.oidc.providers.get(name).ok_or(ApiError::NotFound)?;
    let identity = crate::auth::oidc::finish(state, name, code, pending).await?;
    provider.admits(&identity)?;
    use forgust_db::users::Linked;
    match forgust_db::users::link_identity(&state.db, user.id, name, &identity.subject).await? {
        Linked::Done => {}
        Linked::AlreadyYours => return Ok(()),
        Linked::IdentityTaken => {
            return Err(ApiError::conflict(format!("this {} account is already linked to another account here", provider.display_name)));
        }
        Linked::ProviderTaken => {
            return Err(ApiError::conflict(format!("another {} account is already linked; unlink it first", provider.display_name)));
        }
    }
    services::audit(
        state,
        &Principal::User { user: user.clone(), scopes: None },
        "identity_linked",
        Some(("user", user.id)),
        json!({ "provider": name }),
        ip,
    )
    .await;
    Ok(())
}

pub async fn oidc_callback(
    State(state): State<AppState>,
    Path(name): Path<String>,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    Query(q): Query<OidcCallback>,
) -> Response {
    finish_sign_in(state, name, jar, headers, ip, q).await.unwrap_or_else(|e| sign_in_failed(e).into_response())
}

async fn finish_sign_in(
    state: AppState,
    name: String,
    jar: CookieJar,
    headers: HeaderMap,
    ip: ClientIp,
    q: OidcCallback,
) -> ApiResult<Response> {
    let started_here = |csrf: &str| jar.get(crate::auth::oidc::STATE_COOKIE).is_some_and(|c| crypto::eq_ct(c.value(), csrf));
    let state_cookie = || crate::auth::oidc::spent_state_cookie(&state);
    if let Some(e) = q.error {
        // refused or cancelled at the provider: back to where it was started
        if let Some(csrf) = q.state.as_deref().filter(|s| started_here(s))
            && let Some(pending) = forgust_db::sessions::take_oidc_state(&state.db, csrf).await?
            && pending.link_user_id.is_some()
        {
            let to = link_outcome(&name, Err(ApiError::bad(format!("the provider answered: {e}"))));
            return Ok((jar.remove(state_cookie()), to).into_response());
        }
        return Err(ApiError::bad(e));
    }
    let csrf = q.state.ok_or_else(|| ApiError::bad("missing state"))?;
    let code = q.code.ok_or_else(|| ApiError::bad("missing code"))?;
    if !started_here(&csrf) {
        return Err(ApiError::bad("this sign-in was not started in this browser; start it again"));
    }
    let pending = crate::auth::oidc::pending(&state, &csrf).await?;
    let jar = jar.remove(state_cookie());
    if pending.link_user_id.is_some() {
        let done = link_identity(&state, &jar, &name, &code, pending, ip.text().as_deref()).await;
        return Ok((jar, link_outcome(&name, done)).into_response());
    }
    let identity = crate::auth::oidc::finish(&state, &name, &code, pending).await?;
    let provider = state.oidc.providers.get(&name).ok_or(ApiError::NotFound)?;
    provider.admits(&identity)?;
    let user = match forgust_db::users::find_identity(&state.db, &name, &identity.subject).await? {
        Some(uid) => forgust_db::users::find(&state.db, uid).await?.ok_or(ApiError::Unauthorized)?,
        None => {
            // link to an existing account by verified email, else create one if allowed
            let by_email = match &identity.email {
                Some(e) => forgust_db::users::find_by_email(&state.db, e).await?,
                None => None,
            };
            // one of an account's other addresses, confirmed: never linked by (only its own
            // address is). An unconfirmed listing of it holds back nobody: it is dropped
            // when the account is created with it.
            let other_address = match (&by_email, &identity.email) {
                (None, Some(e)) => forgust_db::user_emails::reserved_by(&state.db, e).await?.is_some(),
                _ => false,
            };
            let user = match by_email {
                // only an account whose own address was confirmed: never hand one to
                // whoever controls this email at the provider (account pre-hijacking)
                Some(u) if provider.link_existing_by_email && u.email_verified() => u,
                Some(_) => {
                    return Err(ApiError::forbidden(format!(
                        "an account with this email already exists: sign in to it with your password, then link {} under Your settings > Password & 2FA",
                        provider.display_name
                    )));
                }
                None if other_address => {
                    return Err(ApiError::forbidden(format!(
                        "an account confirmed this email as one of its other addresses: if it is yours, sign in to it with your password, then link {} under Your settings > Password & 2FA",
                        provider.display_name
                    )));
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
                None => {
                    return Err(ApiError::forbidden(format!(
                        "no account matches this identity: sign in to yours and link {} in your settings, or ask an administrator for an account",
                        provider.display_name
                    )));
                }
            };
            forgust_db::users::add_identity(&state.db, user.id, &name, &identity.subject).await?;
            user
        }
    };
    if !user.is_active() {
        blocked_sign_in(&state, &user, "oidc", ip).await;
        return Err(ApiError::forbidden("this account is blocked"));
    }
    // the account's second factor still applies: the provider's sign-in
    // stands in for the password only. The challenge travels in the fragment
    // (never sent to a server, never in a Referer).
    if user.two_factor() {
        let challenge = crypto::random_urlsafe(32);
        // what the provider says about the account waits for the code too
        let sync = serde_json::to_value(PendingSync { provider: name, identity }).map_err(|e| ApiError::Internal(e.into()))?;
        forgust_db::sessions::create_challenge(
            &state.db,
            &crypto::token_hash(&challenge),
            user.id,
            Some(&sync),
            Utc::now() + Duration::minutes(5),
        )
        .await?;
        return Ok((jar, Redirect::to(&format!("/login#totp={challenge}"))).into_response());
    }
    let user = sync_from_provider(&state, &name, &identity, user, ip.text().as_deref()).await?;
    let (jar, _) = start_session(&state, jar, &headers, ip, &user, "oidc").await?;
    Ok((jar, Redirect::to("/")).into_response())
}

#[derive(Serialize, ToSchema)]
pub struct SetupStatus {
    pub needs_setup: bool,
    /// Whether the instance sends email (confirmation and reset links,
    /// notifications). Without it, nothing should promise or wait for one.
    pub email_enabled: bool,
    /// Security keys and passkeys work here (served over https): the sign-in
    /// page offers passkeys.
    pub webauthn: bool,
}

pub async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<SetupStatus>> {
    Ok(Json(SetupStatus {
        needs_setup: forgust_db::users::count_humans(&state.db).await? == 0,
        email_enabled: state.cfg.email.enabled(),
        webauthn: state.webauthn.is_some(),
    }))
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
    start_session(&state, jar, &headers, ip, &user, "setup").await
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
    if !state.login_limiter.allow(&format!("forgot|{}", ip.client().unwrap_or_default()))
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
    if !state.login_limiter.allow(&format!("reset|{}", ip.client().unwrap_or_default())) {
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
    let email = services::accounts::confirm(&state, req.token.trim(), me).await?;
    Ok(Json(json!({ "email": email })))
}
