use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use forgust_db::ssh_keys::SshKey;
use forgust_db::tokens::AccessToken;
use russh::keys::ssh_key::{HashAlg, PublicKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::Paging;
use super::dto::{NewToken, ProjectDto, UserDto};
use crate::auth::{SignedIn, session};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::services::{self, paths, users};
use crate::state::AppState;

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
    if !u.email_verified() && !state.cfg.email.smtp_url.is_empty() {
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
    Json(req): Json<UpdateMe>,
) -> ApiResult<Json<UserDto>> {
    require_interactive(&u, &p)?;
    let new_email = !req.email.trim().eq_ignore_ascii_case(&u.email)
        && u.unconfirmed_email.as_deref().is_none_or(|x| !x.eq_ignore_ascii_case(req.email.trim()));
    if new_email {
        reauth(&state, &jar, u.id, &req.current_password).await?;
    }
    paths::validate_name(&req.name)?;
    paths::validate_email(&req.email)?;
    if let Some(other) = forgust_db::users::find_by_email(&state.db, &req.email).await?
        && other.id != u.id
    {
        return Err(ApiError::conflict("email is already in use"));
    }
    // a new address takes over only once confirmed from its own inbox
    forgust_db::users::update_profile(&state.db, u.id, &req.name, &u.email).await?;
    if !req.email.eq_ignore_ascii_case(&u.email) {
        forgust_db::users::set_unconfirmed_email(&state.db, u.id, Some(req.email.trim())).await?;
        services::accounts::send_confirmation(&state, &u, req.email.trim()).await?;
        if new_email {
            // the current inbox hears of it: a change it did not ask for is a warning
            services::notify::send_to(
                &state,
                &u.email,
                "Your email address is being changed",
                &format!(
                    "Hello {},\n\nSomeone signed in as you asked to move your Forgust account to {}. It moves once that address confirms.\n\nIf it was not you, change your password now: that ends every other session.\n",
                    u.username,
                    req.email.trim()
                ),
            )
            .await;
        }
    } else if u.unconfirmed_email.is_some() {
        forgust_db::users::set_unconfirmed_email(&state.db, u.id, None).await?;
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

/// Checks of the current password or a 2FA code inside a session share the
/// sign-in limit, per account: a stolen session cannot guess the password.
fn reauth_allowed(state: &AppState, user_id: i64) -> ApiResult<()> {
    if state.login_limiter.allow(&format!("reauth|{user_id}")) {
        Ok(())
    } else {
        Err(ApiError::forbidden("too many attempts, try again in a minute"))
    }
}

/// An account without a password (signed up through OIDC) proves itself by a
/// recent sign-in instead: a stolen session must not be able to add a password
/// or remove the second factor.
const FRESH_SIGN_IN: chrono::Duration = chrono::Duration::minutes(10);

async fn reauth(state: &AppState, jar: &axum_extra::extract::CookieJar, user_id: i64, password: &str) -> ApiResult<()> {
    reauth_allowed(state, user_id)?;
    match forgust_db::users::password_hash(&state.db, user_id).await? {
        Some(hash) => {
            if !crypto::check_password(password, Some(hash)).await {
                return Err(ApiError::forbidden("password is wrong"));
            }
        }
        None => {
            let token = crate::auth::session_cookie(&state.cfg, jar).map(crypto::token_hash).unwrap_or_default();
            let fresh = forgust_db::sessions::created_at(&state.db, &token).await?.is_some_and(|t| Utc::now() - t < FRESH_SIGN_IN);
            if !fresh {
                return Err(ApiError::forbidden("sign out and sign in again, then retry within 10 minutes"));
            }
        }
    }
    Ok(())
}

pub async fn change_password(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    Json(req): Json<ChangePassword>,
) -> ApiResult<Json<Value>> {
    require_interactive(&u, &p)?;
    reauth(&state, &jar, u.id, &req.current).await.map_err(|e| match e {
        ApiError::Forbidden(m) if m == "password is wrong" => ApiError::forbidden("current password is wrong"),
        e => e,
    })?;
    users::validate_password(&req.new)?;
    forgust_db::users::set_password(&state.db, u.id, &crypto::hash_password_gated(&req.new).await?).await?;
    // whoever else is signed in (perhaps with the old password) is signed out
    let current = crate::auth::session_cookie(&state.cfg, &jar).map(crypto::token_hash).unwrap_or_default();
    forgust_db::sessions::delete_others(&state.db, u.id, &current).await?;
    services::audit(&state, &p, "password_changed", Some(("user", u.id)), json!({}), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ SSH keys

/// Parse an OpenSSH public key line; returns (normalized key, SHA256 fingerprint).
pub fn parse_public_key(line: &str) -> ApiResult<(String, String)> {
    let key = PublicKey::from_openssh(line.trim()).map_err(|_| ApiError::bad("not a valid OpenSSH public key"))?;
    let fp = key.fingerprint(HashAlg::Sha256).to_string();
    let normalized = key.to_openssh().map_err(|_| ApiError::bad("not a valid OpenSSH public key"))?;
    Ok((normalized, fp))
}

#[derive(Deserialize, ToSchema)]
pub struct AddKey {
    pub title: String,
    pub key: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[utoipa::path(get, path = "/api/v1/user/keys", responses((status = 200, body = [SshKey])), tag = "user")]
pub async fn list_keys(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Vec<SshKey>>> {
    Ok(Json(forgust_db::ssh_keys::list_for_user(&state.db, u.id).await?))
}

pub async fn add_key(State(state): State<AppState>, SignedIn(u, p): SignedIn, Json(req): Json<AddKey>) -> ApiResult<Json<SshKey>> {
    // a key outlives any token: a leaked token must not be able to plant one
    require_interactive(&u, &p)?;
    require_confirmed(&state, &u)?;
    let (key, fp) = parse_public_key(&req.key)?;
    let title = if req.title.trim().is_empty() { req.key.split_whitespace().nth(2).unwrap_or("key").to_string() } else { req.title };
    let id = forgust_db::ssh_keys::insert(&state.db, "user", Some(u.id), &title, &key, &fp, req.expires_at)
        .await
        .map_err(|e| if forgust_db::is_unique_violation(&e) { ApiError::conflict("this key is already in use") } else { e.into() })?;
    services::audit(&state, &p, "ssh_key_added", Some(("user", u.id)), json!({ "fingerprint": fp }), None).await;
    let keys = forgust_db::ssh_keys::list_for_user(&state.db, u.id).await?;
    Ok(Json(keys.into_iter().find(|k| k.id == id).ok_or(ApiError::NotFound)?))
}

pub async fn delete_key(State(state): State<AppState>, SignedIn(u, p): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
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

/// Start enrolment: store a new (not yet enabled) secret.
pub async fn totp_setup(State(state): State<AppState>, SignedIn(u, p): SignedIn) -> ApiResult<Json<TotpSetup>> {
    require_interactive(&u, &p)?;
    if u.totp_enabled {
        return Err(ApiError::conflict("two-factor authentication is already enabled"));
    }
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

pub async fn totp_enable(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    Json(req): Json<TotpCode>,
) -> ApiResult<Json<UserDto>> {
    require_interactive(&u, &p)?;
    reauth(&state, &jar, u.id, &req.password).await?;
    if !session::verify_totp(&state, &u, &req.code).await? {
        return Err(ApiError::bad("the code is not valid; check your device clock"));
    }
    let sealed = forgust_db::users::totp_secret(&state.db, u.id).await?.ok_or_else(|| ApiError::bad("run setup first"))?;
    forgust_db::users::set_totp(&state.db, u.id, Some(&sealed), true).await?;
    services::audit(&state, &p, "two_factor_enabled", Some(("user", u.id)), json!({}), None).await;
    let u = forgust_db::users::find(&state.db, u.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(UserDto::from(&u, true)))
}

#[derive(Deserialize, ToSchema)]
pub struct TotpDisable {
    pub password: String,
}

pub async fn totp_disable(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    jar: axum_extra::extract::CookieJar,
    Json(req): Json<TotpDisable>,
) -> ApiResult<Json<UserDto>> {
    require_interactive(&u, &p)?;
    reauth(&state, &jar, u.id, &req.password).await?;
    forgust_db::users::set_totp(&state.db, u.id, None, false).await?;
    services::audit(&state, &p, "two_factor_disabled", Some(("user", u.id)), json!({}), None).await;
    let u = forgust_db::users::find(&state.db, u.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(UserDto::from(&u, true)))
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

/// User picker for member dialogs.
pub async fn search_users(
    State(state): State<AppState>,
    SignedIn(u, _): SignedIn,
    Query(pg): Query<Paging>,
) -> ApiResult<Json<Vec<UserDto>>> {
    let users = forgust_db::users::list(&state.db, &pg.search, false, u.is_admin, pg.limit(), pg.offset()).await?;
    Ok(Json(users.iter().map(|x| UserDto::from(x, u.is_admin)).collect()))
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
    let n = forgust_db::todos::mark_done(&state.db, u.id, Some(id)).await?;
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({ "done": n })))
}

/// Mark every pending to-do done.
pub async fn todos_done(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "done": forgust_db::todos::mark_done(&state.db, u.id, None).await? })))
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
