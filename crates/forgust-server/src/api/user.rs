use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use forgust_db::ssh_keys::SshKey;
use forgust_db::tokens::AccessToken;
use forgust_db::user_emails::UserEmail;
use russh::keys::ssh_key::{HashAlg, PublicKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::Paging;
use super::dto::{NewToken, ProjectDto, UserDto};
use crate::auth::{ClientIp, SignedIn, session, webauthn};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::services::{self, paths, users};
use crate::state::AppState;
use forgust_db::webauthn::CredentialKind;

/// Account-security changes (email, password, 2FA, new tokens, SSH keys) need a browser
/// session of a human user: a leaked token must not be able to persist access
/// beyond its own revocation.
pub(crate) fn require_interactive(u: &forgust_db::users::User, p: &crate::auth::Principal) -> ApiResult<()> {
    if u.is_bot() || !matches!(p, crate::auth::Principal::User { scopes: None, .. }) {
        return Err(ApiError::forbidden("this change requires signing in with a browser session"));
    }
    Ok(())
}

/// Keys and tokens outlive a password reset: with email on, only once the
/// address is confirmed (an account registered with someone else's address
/// must not keep a way in after they take it back).
pub(crate) fn require_confirmed(state: &AppState, u: &forgust_db::users::User) -> ApiResult<()> {
    if !u.email_verified() && state.cfg.email.enabled() {
        return Err(ApiError::forbidden("confirm your email address first (see the link we sent)"));
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/v1/user", responses((status = 200, body = UserDto)), tag = "user")]
pub async fn me(SignedIn(u, _): SignedIn) -> Json<UserDto> {
    Json(UserDto::from(&u, true))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateMe {
    pub name: String,
    pub email: String,
    /// Needed to change the email address (a borrowed session must not be
    /// able to move the account to another inbox).
    #[serde(default)]
    pub current_password: String,
}

pub async fn update_me(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Json(req): Json<UpdateMe>,
) -> ApiResult<Json<UserDto>> {
    require_interactive(&u, &p)?;
    let email = req.email.trim();
    let changes = !email.eq_ignore_ascii_case(&u.email);
    let pending = u.unconfirmed_email.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(email));
    // without email there is no link to confirm a new address with: it changes at once
    let at_once = changes && !state.cfg.email.enabled();
    let reauthed = if changes && (!pending || at_once) { Some(reauth(&state, &jar, ip, u.id, &req.current_password).await?) } else { None };
    paths::validate_name(&req.name)?;
    paths::validate_email(email)?;
    if changes {
        match forgust_db::user_emails::reserved_by(&state.db, email).await? {
            Some(id) if id == u.id => return Err(ApiError::conflict("this address is in your other addresses: remove it there first")),
            Some(_) => return Err(ApiError::conflict("email is already in use")),
            None => {}
        }
    }
    // a new address takes over only once confirmed from its own inbox
    forgust_db::users::update_profile(&state.db, u.id, &req.name, &u.email).await?;
    if at_once {
        // an administrator vouches for addresses (the accounts they create are confirmed)
        services::accounts::set_email(&state, &p, &u, email, u.is_admin).await?;
    } else if changes {
        forgust_db::users::set_unconfirmed_email(&state.db, u.id, Some(email)).await?;
        // the address already waiting asked for no password: saving the profile
        // again sends it nothing (`resend_confirmation` does, within its budget)
        if !pending {
            services::accounts::send_confirmation(&state, &u, email).await?;
            // the current inbox hears of it: a change it did not ask for is a warning
            services::notify::send_to(
                &state,
                &u.email,
                "Your email address is being changed",
                &format!(
                    "Hello {},\n\nSomeone signed in as you asked to move your Forgust account to {email}. It moves once that address confirms.\n\nIf it was not you, change your password now: that ends every other session.\n",
                    u.username
                ),
            )
            .await;
        }
    } else if u.unconfirmed_email.is_some() {
        forgust_db::users::set_unconfirmed_email(&state.db, u.id, None).await?;
    }
    if let Some(r) = reauthed {
        r.spend(&state).await?;
    }
    services::audit(&state, &p, "profile_updated", Some(("user", u.id)), json!({}), None).await;
    let u = forgust_db::users::find(&state.db, u.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(UserDto::from(&u, true)))
}

#[derive(Deserialize, ToSchema)]
pub struct ChangePassword {
    #[serde(default)]
    pub current: String,
    pub new: String,
}

/// Checks of the current password inside a session share the sign-in limit,
/// per account: a stolen session cannot guess the password. Past it, as at
/// sign-in ([`account_open`](crate::auth::account_open)), only addresses the
/// account signed in from before may keep trying: someone guessing cannot
/// lock its owner out.
async fn reauth_allowed(state: &AppState, ip: ClientIp, user_id: i64) -> ApiResult<()> {
    if crate::auth::account_open(state, ip, user_id).await? && state.login_limiter.allow(&format!("reauth|{user_id}")) {
        Ok(())
    } else {
        Err(ApiError::forbidden("too many attempts, try again in a minute"))
    }
}

/// An account without a password (signed up through OIDC) proves itself by a
/// recent sign-in instead: a stolen session must not be able to add a password
/// or remove the second factor.
const FRESH_SIGN_IN: chrono::Duration = chrono::Duration::minutes(10);

/// The account proved itself again. Without a password it did so with its
/// recent sign-in, which [`Reauthed::spend`] uses up once the change is made:
/// one sign-in, one sensitive change.
#[must_use = "spend it once the change is made"]
struct Reauthed {
    /// The session whose recent sign-in stood in for the password.
    fresh_session: Option<Vec<u8>>,
}

impl Reauthed {
    async fn spend(self, state: &AppState) -> ApiResult<()> {
        if let Some(token) = self.fresh_session {
            forgust_db::sessions::mark_not_recent(&state.db, &token).await?;
        }
        Ok(())
    }
}

async fn reauth(state: &AppState, jar: &axum_extra::extract::CookieJar, ip: ClientIp, user_id: i64, password: &str) -> ApiResult<Reauthed> {
    reauth_allowed(state, ip, user_id).await?;
    match forgust_db::users::password_hash(&state.db, user_id).await? {
        Some(hash) => {
            if !crypto::check_password(password, Some(hash)).await {
                state.login_failures.allow(&format!("acct|{user_id}"));
                return Err(ApiError::forbidden("password is wrong"));
            }
            Ok(Reauthed { fresh_session: None })
        }
        None => {
            let token = crate::auth::session_cookie(&state.cfg, jar).map(crypto::token_hash).unwrap_or_default();
            let fresh = forgust_db::sessions::own_sign_in(&state.db, &token)
                .await?
                .is_some_and(|(owner, at)| owner == user_id && Utc::now() - at < FRESH_SIGN_IN);
            if !fresh {
                return Err(ApiError::forbidden("sign out and sign in again, then retry within 10 minutes"));
            }
            Ok(Reauthed { fresh_session: Some(token) })
        }
    }
}

pub async fn change_password(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Json(req): Json<ChangePassword>,
) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    let reauthed = reauth(&state, &jar, ip, u.id, &req.current).await.map_err(|e| match e {
        ApiError::Forbidden(m) if m == "password is wrong" => ApiError::forbidden("current password is wrong"),
        e => e,
    })?;
    users::validate_password(&req.new)?;
    forgust_db::users::set_password(&state.db, u.id, &crypto::hash_password_gated(&req.new).await?).await?;
    reauthed.spend(&state).await?;
    // whoever else is signed in (perhaps with the old password) is signed out
    let current = crate::auth::session_cookie(&state.cfg, &jar).map(crypto::token_hash).unwrap_or_default();
    forgust_db::sessions::delete_others(&state.db, u.id, &current).await?;
    services::audit(&state, &p, "password_changed", Some(("user", u.id)), json!({}), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ sign-in providers

/// A configured sign-in provider, and whether this account is linked to it.
#[derive(Serialize, ToSchema)]
pub struct LinkedProvider {
    pub name: String,
    pub display_name: String,
    /// When the account was linked (None: not linked).
    pub linked_at: Option<DateTime<Utc>>,
}

#[utoipa::path(get, path = "/api/v1/user/identities", responses((status = 200, body = [LinkedProvider])), tag = "user")]
pub async fn list_identities(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Vec<LinkedProvider>>> {
    let linked = forgust_db::users::identities(&state.db, u.id).await?;
    Ok(Json(
        state
            .oidc
            .providers
            .iter()
            .map(|(name, p)| LinkedProvider {
                name: name.clone(),
                display_name: p.display_name.clone(),
                linked_at: linked.iter().find(|i| &i.provider == name).map(|i| i.created_at),
            })
            .collect(),
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct ConfirmPassword {
    /// The account's password (accounts without one: empty, within 10 minutes of signing in).
    #[serde(default)]
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct LinkStart {
    /// Send the browser here: the provider's sign-in page.
    pub url: String,
}

/// Start linking a provider to the signed-in account: the provider's sign-in,
/// whose callback adds the identity to this account. Asked for with the
/// password, like any new way into the account.
pub async fn link_identity(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Path(name): Path<String>,
    Json(req): Json<ConfirmPassword>,
) -> ApiResult<(axum_extra::extract::CookieJar, Json<LinkStart>)> {
    require_interactive(&u, &p)?;
    require_confirmed(&state, &u)?;
    if !state.oidc.providers.contains_key(&name) {
        return Err(ApiError::NotFound);
    }
    let reauthed = reauth(&state, &jar, ip, u.id, &req.password).await?;
    let (url, login_state) = crate::auth::oidc::start(&state, &name, Some(u.id)).await?;
    reauthed.spend(&state).await?;
    Ok((jar.add(crate::auth::oidc::state_cookie(&state, login_state)), Json(LinkStart { url })))
}

/// Unlink a provider. Never the last way in: an account without a password
/// keeps at least one provider.
pub async fn unlink_identity(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Path(name): Path<String>,
    Json(req): Json<ConfirmPassword>,
) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    let reauthed = reauth(&state, &jar, ip, u.id, &req.password).await?;
    let linked = forgust_db::users::identities(&state.db, u.id).await?;
    if !linked.iter().any(|i| i.provider == name) {
        return Err(ApiError::NotFound);
    }
    let other_way_in = forgust_db::users::password_hash(&state.db, u.id).await?.is_some() || linked.iter().any(|i| i.provider != name);
    if !other_way_in {
        return Err(ApiError::forbidden("this is the only way into your account: set a password first"));
    }
    forgust_db::users::remove_identities(&state.db, u.id, &name).await?;
    reauthed.spend(&state).await?;
    services::audit(&state, &p, "identity_unlinked", Some(("user", u.id)), json!({ "provider": name }), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ SSH keys

/// Parse an OpenSSH public key line; returns (normalized key, SHA256 fingerprint).
pub fn parse_public_key(line: &str) -> ApiResult<(String, String)> {
    let line = line.trim();
    // the parser takes all that follows the key as its comment, other keys too
    if line.contains(['\n', '\r']) {
        return Err(ApiError::bad("paste one public key, on a single line"));
    }
    let key = PublicKey::from_openssh(line).map_err(|_| ApiError::bad("not a valid OpenSSH public key"))?;
    let fp = key.fingerprint(HashAlg::Sha256).to_string();
    let normalized = key.to_openssh().map_err(|_| ApiError::bad("not a valid OpenSSH public key"))?;
    Ok((normalized, fp))
}

#[derive(Deserialize, ToSchema)]
pub struct AddKey {
    pub title: String,
    pub key: String,
    pub expires_at: Option<DateTime<Utc>>,
    /// What the key is for; default: both.
    pub usage_type: Option<forgust_db::ssh_keys::KeyUsage>,
}

/// The comment of an OpenSSH public key line, all of it (`ssh-ed25519 AAAA… work laptop`
/// → `work laptop`): the default title, as the profile page fills it in.
fn key_comment(line: &str) -> Option<String> {
    let comment = line.split_whitespace().skip(2).collect::<Vec<_>>().join(" ");
    (!comment.is_empty()).then_some(comment)
}

#[utoipa::path(get, path = "/api/v1/user/keys", responses((status = 200, body = [SshKey])), tag = "user")]
pub async fn list_keys(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Vec<SshKey>>> {
    Ok(Json(forgust_db::ssh_keys::list_for_user(&state.db, u.id).await?))
}

pub async fn add_key(State(state): State<AppState>, SignedIn(u, p): SignedIn, Json(req): Json<AddKey>) -> ApiResult<Json<SshKey>> {
    // a key outlives any token: a leaked token must not be able to plant one
    require_interactive(&u, &p)?;
    require_confirmed(&state, &u)?;
    let usage = req.usage_type.unwrap_or(forgust_db::ssh_keys::KeyUsage::AuthAndSigning);
    let (key, fp) = parse_public_key(&req.key)?;
    let title = if req.title.trim().is_empty() { key_comment(&req.key).unwrap_or_else(|| "key".into()) } else { req.title };
    let id = forgust_db::ssh_keys::insert(&state.db, "user", Some(u.id), &title, &key, &fp, req.expires_at, usage)
        .await
        .map_err(|e| if forgust_db::is_unique_violation(&e) { ApiError::conflict("this key is already in use") } else { e.into() })?;
    services::audit(&state, &p, "ssh_key_added", Some(("user", u.id)), json!({ "fingerprint": fp, "usage_type": usage }), None).await;
    let keys = forgust_db::ssh_keys::list_for_user(&state.db, u.id).await?;
    Ok(Json(keys.into_iter().find(|k| k.id == id).ok_or(ApiError::NotFound)?))
}

pub async fn delete_key(State(state): State<AppState>, SignedIn(u, p): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    if !forgust_db::ssh_keys::delete_for_user(&state.db, u.id, id).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(&state, &p, "ssh_key_removed", Some(("user", u.id)), json!({ "key_id": id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ personal access tokens

pub const TOKEN_SCOPES: &[&str] = &["api", "read_api", "read_user", "read_repository", "write_repository", "mcp", "admin_mode"];

#[derive(Deserialize, ToSchema)]
pub struct CreateToken {
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub fn check_scopes(scopes: &[String]) -> ApiResult<()> {
    if scopes.is_empty() {
        return Err(ApiError::bad("choose at least one scope"));
    }
    for s in scopes {
        if !TOKEN_SCOPES.contains(&s.as_str()) {
            return Err(ApiError::bad(format!("unknown scope `{s}`")));
        }
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/v1/user/tokens", responses((status = 200, body = [AccessToken])), tag = "user")]
pub async fn list_tokens(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Vec<AccessToken>>> {
    Ok(Json(forgust_db::tokens::list_pats(&state.db, u.id).await?))
}

pub async fn create_token(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    Json(req): Json<CreateToken>,
) -> ApiResult<Json<NewToken>> {
    // a token cannot mint more tokens
    require_interactive(&u, &p)?;
    require_confirmed(&state, &u)?;
    paths::validate_name(&req.name)?;
    check_scopes(&req.scopes)?;
    if req.scopes.iter().any(|s| s == "admin_mode") && !u.is_admin {
        return Err(ApiError::bad("admin_mode is for administrators' tokens"));
    }
    let token = crypto::random_token(crypto::prefix::PERSONAL);
    let id = forgust_db::tokens::create_pat(&state.db, u.id, &req.name, &crypto::token_hash(&token), &req.scopes, req.expires_at).await?;
    services::audit(&state, &p, "token_created", Some(("user", u.id)), json!({ "name": req.name, "scopes": req.scopes }), None).await;
    Ok(Json(NewToken { id, token, username: Some(u.username) }))
}

pub async fn revoke_token(State(state): State<AppState>, SignedIn(u, p): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    if !forgust_db::tokens::revoke_pat(&state.db, u.id, id).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(&state, &p, "token_revoked", Some(("user", u.id)), json!({ "token_id": id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ TOTP

#[derive(Serialize, ToSchema)]
pub struct TotpSetup {
    pub secret: String,
    pub otpauth_url: String,
}

/// Start enrolment: store a new (not yet enabled) secret. Asked for with the
/// password, like enabling it: a borrowed session must not be able to put its
/// own secret on the account. An account without a password keeps its recent
/// sign-in for the enabling.
pub async fn totp_setup(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Json(req): Json<ConfirmPassword>,
) -> ApiResult<Json<TotpSetup>> {
    require_interactive(&u, &p)?;
    if u.totp_enabled {
        return Err(ApiError::conflict("two-factor authentication is already enabled"));
    }
    // spent by the enabling, which asks again
    let _ = reauth(&state, &jar, ip, u.id, &req.password).await?;
    let secret = session::new_totp_secret();
    let sealed = state.secrets.encrypt(secret.as_bytes())?;
    forgust_db::users::set_totp(&state.db, u.id, Some(&sealed), false).await?;
    let url = session::totp_for(&secret, &u.username)?.to_url().map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
    Ok(Json(TotpSetup { secret, otpauth_url: url }))
}

#[derive(Deserialize, ToSchema)]
pub struct TotpCode {
    pub code: String,
    /// The account's password: a borrowed session must not be able to put its
    /// own second factor on the account.
    #[serde(default)]
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct TotpEnabled {
    pub user: UserDto,
    /// Shown this once: each stands in for the authenticator at sign-in, once.
    /// Empty when the account already had codes (from its security keys).
    pub recovery_codes: Vec<String>,
}

pub async fn totp_enable(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Json(req): Json<TotpCode>,
) -> ApiResult<Json<TotpEnabled>> {
    require_interactive(&u, &p)?;
    if u.totp_enabled {
        return Err(ApiError::conflict("two-factor authentication is already enabled"));
    }
    let reauthed = reauth(&state, &jar, ip, u.id, &req.password).await?;
    // the app and its recovery codes come together, or not at all
    let mut tx = state.db.begin().await?;
    if !session::verify_totp(&state, &mut tx, &u, &req.code).await? {
        return Err(ApiError::bad("the code is not valid; check your device clock"));
    }
    let sealed = forgust_db::users::totp_secret(&mut *tx, u.id).await?.ok_or_else(|| ApiError::bad("run setup first"))?;
    forgust_db::users::set_totp(&mut *tx, u.id, Some(&sealed), true).await?;
    let recovery_codes = session::first_recovery_codes(&mut tx, u.id).await?;
    tx.commit().await?;
    reauthed.spend(&state).await?;
    services::audit(&state, &p, "two_factor_enabled", Some(("user", u.id)), json!({}), None).await;
    let u = forgust_db::users::find(&state.db, u.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(TotpEnabled { user: UserDto::from(&u, true), recovery_codes }))
}

#[derive(Deserialize, ToSchema)]
pub struct TotpDisable {
    pub password: String,
}

pub async fn totp_disable(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Json(req): Json<TotpDisable>,
) -> ApiResult<Json<UserDto>> {
    require_interactive(&u, &p)?;
    let reauthed = reauth(&state, &jar, ip, u.id, &req.password).await?;
    let mut tx = state.db.begin().await?;
    forgust_db::users::disable_totp(&mut tx, u.id).await?;
    tx.commit().await?;
    reauthed.spend(&state).await?;
    services::audit(&state, &p, "two_factor_disabled", Some(("user", u.id)), json!({}), None).await;
    let u = forgust_db::users::find(&state.db, u.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(UserDto::from(&u, true)))
}

#[derive(Serialize, ToSchema)]
pub struct RecoveryCodesLeft {
    /// How many unused codes are left.
    pub remaining: i64,
}

pub async fn recovery_codes(State(state): State<AppState>, SignedIn(u, p): SignedIn) -> ApiResult<Json<RecoveryCodesLeft>> {
    require_interactive(&u, &p)?;
    Ok(Json(RecoveryCodesLeft { remaining: forgust_db::recovery_codes::remaining(&state.db, u.id).await? }))
}

#[derive(Serialize, ToSchema)]
pub struct NewRecoveryCodes {
    /// Shown this once: each stands in for the authenticator at sign-in, once.
    pub recovery_codes: Vec<String>,
}

/// New recovery codes; the old ones stop working.
pub async fn recovery_codes_reset(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Json(req): Json<ConfirmPassword>,
) -> ApiResult<Json<NewRecoveryCodes>> {
    require_interactive(&u, &p)?;
    let reauthed = reauth(&state, &jar, ip, u.id, &req.password).await?;
    if !u.two_factor() {
        return Err(ApiError::conflict("turn on two-factor authentication first"));
    }
    let mut tx = state.db.begin().await?;
    let recovery_codes = session::reset_recovery_codes(&mut tx, u.id).await?;
    tx.commit().await?;
    reauthed.spend(&state).await?;
    services::audit(&state, &p, "recovery_codes_reset", Some(("user", u.id)), json!({}), None).await;
    Ok(Json(NewRecoveryCodes { recovery_codes }))
}

// ------------------------------------------------------------------ security keys and passkeys

/// More would only be a list nobody reviews.
const MAX_WEBAUTHN: usize = 20;

#[derive(Serialize, ToSchema)]
pub struct WebauthnCredentials {
    /// Security keys and passkeys work on this instance (served over https).
    pub available: bool,
    pub credentials: Vec<forgust_db::webauthn::CredentialInfo>,
}

pub async fn webauthn_list(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<WebauthnCredentials>> {
    Ok(Json(WebauthnCredentials { available: state.webauthn.is_some(), credentials: forgust_db::webauthn::list(&state.db, u.id).await? }))
}

#[derive(Deserialize, ToSchema)]
pub struct WebauthnRegisterStart {
    /// A second factor, or a sign-in of its own.
    pub kind: CredentialKind,
    /// The account's password: a borrowed session must not be able to add its
    /// own way in.
    #[serde(default)]
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct WebauthnRegistration {
    /// Send back with the new credential.
    pub ceremony: String,
    /// Options for `navigator.credentials.create` (WebAuthn).
    #[schema(value_type = Object)]
    pub options: Value,
}

/// The browser session making this request (its id).
async fn current_session(state: &AppState, jar: &axum_extra::extract::CookieJar) -> ApiResult<i64> {
    let token = crate::auth::session_cookie(&state.cfg, jar).map(crypto::token_hash).unwrap_or_default();
    forgust_db::sessions::id_by_token(&state.db, &token).await?.ok_or(ApiError::Unauthorized)
}

/// Start adding a security key or a passkey. The authenticator is asked not to
/// make a second credential for the account; a passkey must be one it can
/// offer with no username (discoverable) and that verifies its user.
pub async fn webauthn_register_start(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Json(req): Json<WebauthnRegisterStart>,
) -> ApiResult<Json<WebauthnRegistration>> {
    require_interactive(&u, &p)?;
    require_confirmed(&state, &u)?;
    let rp = webauthn::of(&state)?;
    let reauthed = reauth(&state, &jar, ip, u.id, &req.password).await?;
    let exclude = forgust_db::webauthn::credential_ids(&state.db, u.id).await?;
    if exclude.len() >= MAX_WEBAUTHN {
        return Err(ApiError::conflict(format!("an account has at most {MAX_WEBAUTHN} security keys and passkeys")));
    }
    let session_id = current_session(&state, &jar).await?;
    let handle = forgust_db::webauthn::handle(&state.db, u.id, uuid::Uuid::new_v4()).await?;
    let display = if u.name.is_empty() { &u.username } else { &u.name };
    let (options, pending) = match req.kind {
        CredentialKind::Passkey => {
            let (mut options, reg) =
                rp.start_passkey_registration(handle, &u.username, display, Some(exclude)).map_err(webauthn::internal)?;
            webauthn::require_resident_key(&mut options);
            (webauthn::to_json(&options)?, webauthn::to_json(&reg)?)
        }
        CredentialKind::SecurityKey => {
            let (mut options, reg) =
                rp.start_securitykey_registration(handle, &u.username, display, Some(exclude), None, None).map_err(webauthn::internal)?;
            webauthn::without_cred_protect(&mut options);
            (webauthn::to_json(&options)?, webauthn::to_json(&reg)?)
        }
    };
    let ceremony = crypto::random_urlsafe(32);
    forgust_db::webauthn::save_ceremony(
        &state.db,
        &crypto::token_hash(&ceremony),
        req.kind.registration(),
        Some(u.id),
        Some(session_id),
        &pending,
        Utc::now() + chrono::Duration::minutes(5),
    )
    .await?;
    reauthed.spend(&state).await?;
    Ok(Json(WebauthnRegistration { ceremony, options }))
}

#[derive(Deserialize, ToSchema)]
pub struct WebauthnRegisterFinish {
    pub ceremony: String,
    /// How the owner tells it apart ("YubiKey 5C", "Phone").
    pub name: String,
    /// What `navigator.credentials.create` returned (`PublicKeyCredential`, as JSON).
    #[schema(value_type = Object)]
    pub credential: webauthn_rs::prelude::RegisterPublicKeyCredential,
}

#[derive(Serialize, ToSchema)]
pub struct WebauthnRegistered {
    pub credential: forgust_db::webauthn::CredentialInfo,
    pub user: UserDto,
    /// The account's first second factor: its recovery codes, shown this once
    /// (empty otherwise).
    pub recovery_codes: Vec<String>,
}

fn credential_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    paths::validate_name(name)?;
    if name.chars().count() > 64 {
        return Err(ApiError::bad("give it a name of at most 64 characters"));
    }
    Ok(name.to_string())
}

/// Finish adding a security key or a passkey, from the session that started it.
pub async fn webauthn_register_finish(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    Json(req): Json<WebauthnRegisterFinish>,
) -> ApiResult<Json<WebauthnRegistered>> {
    require_interactive(&u, &p)?;
    require_confirmed(&state, &u)?;
    let name = credential_name(&req.name)?;
    let rp = webauthn::of(&state)?;
    let session_id = current_session(&state, &jar).await?;
    let expired = || ApiError::bad("this registration expired: start again");
    let c = forgust_db::webauthn::take_ceremony(&state.db, &crypto::token_hash(&req.ceremony))
        .await?
        .filter(|c| c.user_id == Some(u.id) && c.session_id == Some(session_id))
        .ok_or_else(expired)?;
    let kind = CredentialKind::of_registration(&c.kind).ok_or_else(expired)?;
    let refused = |e: webauthn_rs::prelude::WebauthnError| {
        use webauthn_rs::prelude::WebauthnError as E;
        // webauthn-rs 0.6.2 reports a credential of the exclude list (one this
        // account has already) as CredentialAlteredAlgFromRequest
        if matches!(e, E::CredentialAlteredAlgFromRequest | E::CredentialExcludedFromRequest | E::CredentialAlreadyExists) {
            return ApiError::conflict("this authenticator is already registered");
        }
        tracing::info!(user = u.id, error = %e, "a WebAuthn registration was refused");
        ApiError::bad("the authenticator's answer was not accepted: try again")
    };
    let (credential_id, credential) = match kind {
        CredentialKind::Passkey => {
            // the browser says the authenticator could not keep it: it would never
            // be offered at sign-in (an unsigned hint, only to spare the user)
            if req.credential.extensions.cred_props.as_ref().and_then(|c| c.rk) == Some(false) {
                return Err(ApiError::bad("this authenticator cannot keep a passkey: add it as a security key instead"));
            }
            let key = rp.finish_passkey_registration(&req.credential, &webauthn::from_json(c.state)?).map_err(refused)?;
            (key.cred_id().to_vec(), webauthn::to_json(&key)?)
        }
        CredentialKind::SecurityKey => {
            let key = rp.finish_securitykey_registration(&req.credential, &webauthn::from_json(c.state)?).map_err(refused)?;
            (key.cred_id().to_vec(), webauthn::to_json(&key)?)
        }
    };
    let mut tx = state.db.begin().await?;
    let credential = forgust_db::webauthn::insert(&mut tx, u.id, kind, &credential_id, &credential, &name, &rp.id).await.map_err(|e| {
        if forgust_db::is_unique_violation(&e) { ApiError::conflict("this authenticator is already registered") } else { e.into() }
    })?;
    let recovery_codes = if kind == CredentialKind::SecurityKey { session::first_recovery_codes(&mut tx, u.id).await? } else { Vec::new() };
    let after = forgust_db::users::find(&mut *tx, u.id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    services::audit(
        &state,
        &p,
        "webauthn_registered",
        Some(("user", u.id)),
        json!({ "kind": kind, "name": name, "credential": credential.id }),
        None,
    )
    .await;
    webauthn_notice(&state, &u, kind, "added", &name).await;
    Ok(Json(WebauthnRegistered { credential, user: UserDto::from(&after, true), recovery_codes }))
}

/// The account's inbox hears of every change to its ways in.
async fn webauthn_notice(state: &AppState, u: &forgust_db::users::User, kind: CredentialKind, done: &str, name: &str) {
    let label = kind.label();
    services::notify::send_to(
        state,
        &u.email,
        &format!("A {label} was {done}"),
        &format!(
            "Hello {},\n\nThe {label} “{name}” was {done} on your Forgust account.\n\nIf it was not you, change your password now and check Profile > Security.\n",
            u.username
        ),
    )
    .await;
}

#[derive(Deserialize, ToSchema)]
pub struct WebauthnRename {
    pub name: String,
}

pub async fn webauthn_rename(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    Path(id): Path<i64>,
    Json(req): Json<WebauthnRename>,
) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    let name = credential_name(&req.name)?;
    if !forgust_db::webauthn::rename(&state.db, u.id, id, &name).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(&state, &p, "webauthn_renamed", Some(("user", u.id)), json!({ "credential": id, "name": name }), None).await;
    Ok(Json(json!({ "ok": true })))
}

/// Remove a security key or a passkey (the password again). With the last
/// second factor go the recovery codes.
pub async fn webauthn_remove(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Path(id): Path<i64>,
    Json(req): Json<ConfirmPassword>,
) -> ApiResult<Json<UserDto>> {
    require_interactive(&u, &p)?;
    let reauthed = reauth(&state, &jar, ip, u.id, &req.password).await?;
    let mut tx = state.db.begin().await?;
    let (kind, name) = forgust_db::webauthn::delete(&mut tx, u.id, id).await?.ok_or(ApiError::NotFound)?;
    let after = forgust_db::users::find(&mut *tx, u.id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    reauthed.spend(&state).await?;
    services::audit(&state, &p, "webauthn_removed", Some(("user", u.id)), json!({ "kind": kind, "name": name, "credential": id }), None)
        .await;
    webauthn_notice(&state, &u, kind, "removed", &name).await;
    Ok(Json(UserDto::from(&after, true)))
}

// ------------------------------------------------------------------ lists

#[derive(Serialize, ToSchema)]
pub struct MyProject {
    #[serde(flatten)]
    pub project: ProjectDto,
    pub access_level: String,
}

#[utoipa::path(get, path = "/api/v1/user/projects", responses((status = 200, body = [MyProject])), tag = "user")]
pub async fn my_projects(
    State(state): State<AppState>,
    SignedIn(u, _): SignedIn,
    Query(pg): Query<Paging>,
) -> ApiResult<Json<Vec<MyProject>>> {
    let rows = forgust_db::projects::for_user(&state.db, u.id, &pg.search, pg.limit(), pg.offset()).await?;
    Ok(Json(
        rows.iter()
            .map(|(p, l)| MyProject { project: ProjectDto::from(p, &state.cfg), access_level: forgust_db::access::name(*l).into() })
            .collect(),
    ))
}

/// How many of your projects [`my_project_states`] reports on: the first, most
/// active, as many as the projects page lists (`per_page=100`); archived ones have none.
const MY_STATE_LIMIT: i64 = 100;

/// How your projects are doing, as on their own pages (open merge requests, the
/// default branch's latest pipeline): one request for the projects page, not two
/// for each project. Those whose code you may not read are left out.
#[utoipa::path(get, path = "/api/v1/user/project_states", responses((status = 200, body = [forgust_db::projects::ProjectState])), tag = "user")]
pub async fn my_project_states(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
) -> ApiResult<Json<Vec<forgust_db::projects::ProjectState>>> {
    let rows = forgust_db::projects::for_user(&state.db, u.id, "", MY_STATE_LIMIT, 0).await?;
    // a few checks at a time, not one after the other (in the list's order)
    let open: Vec<_> = rows.iter().map(|(x, _)| x).filter(|x| !x.archived()).collect();
    let mut ids = Vec::new();
    for chunk in open.chunks(8) {
        let read = futures::future::try_join_all(
            chunk.iter().map(|x| crate::auth::authz::can_project(&state, &p, crate::auth::authz::ProjectAction::ReadCode, x)),
        )
        .await?;
        ids.extend(chunk.iter().zip(read).filter(|(_, ok)| *ok).map(|(x, _)| x.id));
    }
    Ok(Json(forgust_db::projects::states(&state.db, &ids).await?))
}

/// User picker for member dialogs.
pub async fn search_users(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Query(pg): Query<Paging>,
) -> ApiResult<Json<Vec<UserDto>>> {
    // emails and blocked users for an administrator's session or admin_mode token only
    let admin = p.is_admin();
    let users = forgust_db::users::list(&state.db, &pg.search, false, admin, pg.limit(), pg.offset()).await?;
    Ok(Json(users.iter().map(|x| UserDto::from(x, admin)).collect()))
}

#[derive(Serialize, ToSchema)]
pub struct Work {
    /// Open MRs you were asked to review.
    pub review_requests: Vec<forgust_db::people::WorkMr>,
    /// Open MRs assigned to you.
    pub assigned_merge_requests: Vec<forgust_db::people::WorkMr>,
    /// Your open MRs.
    pub your_merge_requests: Vec<forgust_db::people::WorkMr>,
    /// Open issues assigned to you.
    pub assigned_issues: Vec<forgust_db::people::WorkIssue>,
    pub todos_pending: i64,
}

/// "Your work": what is waiting for you, across projects.
#[utoipa::path(get, path = "/api/v1/user/work", responses((status = 200, body = Work)), tag = "user")]
pub async fn work(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Work>> {
    use forgust_db::people::{work_issues, work_mrs};
    // independent: at once
    let (review_requests, assigned_merge_requests, your_merge_requests, assigned_issues, todos_pending) = tokio::try_join!(
        work_mrs(&state.db, u.id, "review", 50),
        work_mrs(&state.db, u.id, "assigned", 50),
        work_mrs(&state.db, u.id, "authored", 50),
        work_issues(&state.db, u.id, 50),
        forgust_db::todos::pending_count(&state.db, u.id),
    )?;
    Ok(Json(Work { review_requests, assigned_merge_requests, your_merge_requests, assigned_issues, todos_pending }))
}

#[derive(Deserialize)]
pub struct TodoQuery {
    #[serde(default = "pending")]
    state: String,
}

fn pending() -> String {
    "pending".into()
}

#[utoipa::path(get, path = "/api/v1/user/todos", responses((status = 200, body = [forgust_db::todos::Todo])), tag = "user")]
pub async fn todos(
    State(state): State<AppState>,
    SignedIn(u, _): SignedIn,
    Query(q): Query<TodoQuery>,
) -> ApiResult<Json<Vec<forgust_db::todos::Todo>>> {
    if !["pending", "done"].contains(&q.state.as_str()) {
        return Err(ApiError::bad("state must be pending or done"));
    }
    Ok(Json(forgust_db::todos::list(&state.db, u.id, &q.state, 100).await?))
}

/// Mark one to-do done.
pub async fn todo_done(State(state): State<AppState>, SignedIn(u, _): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let d = forgust_db::todos::mark_done(&state.db, u.id, Some(id)).await?;
    if d.todos == 0 {
        return Err(ApiError::NotFound);
    }
    bell_follows(&state, u.id, d);
    Ok(Json(json!({ "done": d.todos })))
}

/// Mark every pending to-do done.
pub async fn todos_done(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Value>> {
    let d = forgust_db::todos::mark_done(&state.db, u.id, None).await?;
    bell_follows(&state, u.id, d);
    Ok(Json(json!({ "done": d.todos })))
}

/// To-dos done read their notifications: the person's bell, in each tab, follows.
pub(crate) fn bell_follows(state: &AppState, user_id: i64, d: forgust_db::todos::Done) {
    if d.read > 0 {
        state.events.publish(crate::events::Event::Inbox { user_id });
    }
}

#[derive(Deserialize)]
pub struct ContributionsQuery {
    /// IANA time zone for day boundaries (the viewer's); UTC by default.
    tz: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Contributions {
    pub username: String,
    pub name: String,
    pub member_since: DateTime<Utc>,
    /// The last 53 weeks, days with at least one contribution (oldest first).
    pub days: Vec<forgust_db::contributions::Day>,
    pub total: i64,
    /// Newest first.
    pub activity: Vec<forgust_db::contributions::Activity>,
}

/// A user's contribution calendar and recent activity, counting only projects
/// the viewer can see.
#[utoipa::path(get, path = "/api/v1/users/{username}/contributions", responses((status = 200, body = Contributions)), tag = "user")]
pub async fn contributions(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(username): Path<String>,
    Query(q): Query<ContributionsQuery>,
) -> ApiResult<Json<Contributions>> {
    use forgust_db::contributions as c;
    let u = forgust_db::users::find_by_username(&state.db, &username).await?.filter(|u| !u.is_bot()).ok_or(ApiError::NotFound)?;
    let tz = match q.tz.as_deref().map(str::trim) {
        Some(tz) if !tz.is_empty() && tz.len() <= 64 && c::valid_time_zone(&state.db, tz).await? => tz.to_string(),
        _ => "UTC".to_string(),
    };
    let viewer = c::Viewer { user_id: v.direct_user().map(|x| x.id), is_admin: v.is_admin(), internal: v.sees_internal() };
    let since = Utc::now() - chrono::Duration::days(7 * 53 + 1);
    let days = c::calendar(&state.db, u.id, viewer, since, &tz).await?;
    let activity = c::recent(&state.db, u.id, viewer, 20).await?;
    Ok(Json(Contributions {
        total: days.iter().map(|d| d.count).sum(),
        username: u.username,
        name: u.name,
        member_since: u.created_at,
        days,
        activity,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct Profile {
    pub id: i64,
    pub username: String,
    pub name: String,
    pub member_since: DateTime<Utc>,
    /// The viewer is this person.
    pub is_you: bool,
    /// Their personal namespace (for its settings, when it is yours).
    pub namespace_id: Option<i64>,
    /// Projects in their personal namespace that the viewer can see.
    pub projects: Vec<ProjectDto>,
    pub avatar_url: Option<String>,
}

/// A person's public profile.
#[utoipa::path(get, path = "/api/v1/users/{username}", responses((status = 200, body = Profile)), tag = "user")]
pub async fn profile(State(state): State<AppState>, SignedIn(_, v): SignedIn, Path(username): Path<String>) -> ApiResult<Json<Profile>> {
    let u = forgust_db::users::find_by_username(&state.db, &username)
        .await?
        .filter(|u| u.is_active() && !u.is_bot())
        .ok_or(ApiError::NotFound)?;
    let projects = match u.namespace_id {
        Some(ns) => {
            let signed_in = v.sees_internal();
            forgust_db::projects::in_namespace_visible(&state.db, ns, v.direct_user().map(|x| x.id), v.is_admin(), signed_in)
                .await?
                .iter()
                .map(|x| ProjectDto::from(x, &state.cfg))
                .collect()
        }
        None => Vec::new(),
    };
    Ok(Json(Profile {
        avatar_url: u.avatar_at.map(|at| format!("/api/v1/users/{}/avatar?v={}", u.username, at.timestamp())),
        is_you: v.direct_user().is_some_and(|x| x.id == u.id),
        id: u.id,
        username: u.username,
        name: u.name,
        member_since: u.created_at,
        namespace_id: u.namespace_id,
        projects,
    }))
}

/// Send the confirmation link again: for the new address waiting, else for
/// the account's own unconfirmed one.
pub async fn resend_confirmation(State(state): State<AppState>, SignedIn(u, p): SignedIn) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    services::accounts::require_email(&state)?;
    if !state.login_limiter.allow(&format!("confirm|{}", u.id)) {
        return Err(ApiError::forbidden("too many emails; try again in a minute"));
    }
    let email = match (&u.unconfirmed_email, u.email_verified()) {
        (Some(e), _) => e.clone(),
        (None, false) => u.email.clone(),
        (None, true) => return Err(ApiError::bad("your address is already confirmed")),
    };
    services::accounts::send_confirmation(&state, &u, &email).await?;
    Ok(Json(json!({ "sent_to": email })))
}

/// Your other email addresses: commits made with a confirmed one show as yours.
#[utoipa::path(get, path = "/api/v1/user/emails", responses((status = 200, body = [UserEmail])), tag = "user")]
pub async fn list_emails(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Vec<UserEmail>>> {
    Ok(Json(forgust_db::user_emails::list(&state.db, u.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct AddEmail {
    pub email: String,
    /// Needed when the address is confirmed as it is added (an instance that
    /// sends no email): a borrowed session must not be able to claim one.
    #[serde(default)]
    pub current_password: String,
}

/// Add an address: a link is mailed to it; without email, an administrator
/// confirms it (or it is confirmed at once: see `services::emails`).
pub async fn add_email(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    ip: ClientIp,
    Json(req): Json<AddEmail>,
) -> ApiResult<Json<UserEmail>> {
    require_interactive(&u, &p)?;
    let email = req.email.trim();
    let confirmed_by = services::emails::confirmed_as_added(&state, &u).await?;
    let reauthed = if confirmed_by.is_some() {
        Some(reauth(&state, &jar, ip, u.id, &req.current_password).await?)
    } else {
        // each one sends a link
        if state.cfg.email.enabled() && !state.login_limiter.allow(&format!("confirm|{}", u.id)) {
            return Err(ApiError::forbidden("too many emails; try again in a minute"));
        }
        None
    };
    let row = services::emails::add(&state, &p, &u, email, confirmed_by).await?;
    if let Some(r) = reauthed {
        r.spend(&state).await?;
    }
    Ok(Json(row))
}

pub async fn remove_email(State(state): State<AppState>, SignedIn(u, p): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    services::emails::remove(&state, &p, &u, id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Mail the link for one of your addresses again.
pub async fn resend_email(State(state): State<AppState>, SignedIn(u, p): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    services::accounts::require_email(&state)?;
    let row = forgust_db::user_emails::find(&state.db, u.id, id).await?.ok_or(ApiError::NotFound)?;
    if row.confirmed_at.is_some() {
        return Err(ApiError::bad("this address is already confirmed"));
    }
    if !state.login_limiter.allow(&format!("confirm|{}", u.id)) {
        return Err(ApiError::forbidden("too many emails; try again in a minute"));
    }
    services::emails::send_link(&state, &u, &row).await?;
    Ok(Json(json!({ "sent_to": row.email })))
}
