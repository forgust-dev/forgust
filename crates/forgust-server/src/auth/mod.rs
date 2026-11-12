//! Principals and request authentication (ADR-0015).

pub mod authz;
pub mod oidc;
pub mod session;

use axum::extract::FromRequestParts;
use axum::http::Method;
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;
use forgust_db::tokens::DeployToken;
use forgust_db::users::User;

use crate::crypto;
use crate::error::ApiError;
use crate::state::AppState;

/// The session cookie over plain http (development, a LAN demo).
pub const SESSION_COOKIE: &str = "forgust_session";
/// Over https: `__Host-` makes browsers refuse it unless it is Secure, set by
/// this host, for `/`, without a Domain, so a sibling subdomain (a Pages site,
/// say) can neither plant nor replace it.
pub const SECURE_SESSION_COOKIE: &str = "__Host-forgust_session";

/// The session cookie's name for this instance.
pub fn session_cookie_name(cfg: &crate::config::Config) -> &'static str {
    if cfg.secure_cookies() { SECURE_SESSION_COOKIE } else { SESSION_COOKIE }
}

/// The request's session cookie value, if any (only the name this instance uses).
pub fn session_cookie<'a>(cfg: &crate::config::Config, jar: &'a CookieJar) -> Option<&'a str> {
    jar.get(session_cookie_name(cfg)).map(|c| c.value())
}
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Who is making the request.
/// The scope an impersonation session carries: `impersonated_by:<admin id>`.
pub const IMPERSONATED_BY: &str = "impersonated_by:";

#[derive(Debug, Clone)]
pub enum Principal {
    Anonymous,
    /// A human or bot user. `scopes` is None for browser sessions (full access).
    User {
        user: User,
        scopes: Option<Vec<String>>,
    },
    /// An SSH deploy key (git over SSH only).
    DeployKey {
        key_id: i64,
    },
    DeployToken {
        token: DeployToken,
    },
    /// A CI job token: acts for the triggering user, limited to its project.
    Job {
        token_id: i64,
        project_id: i64,
        user: User,
    },
}

impl Principal {
    pub fn user(&self) -> Option<&User> {
        match self {
            Principal::User { user, .. } | Principal::Job { user, .. } => Some(user),
            _ => None,
        }
    }

    /// A signed-in human or bot user acting directly (not through a job).
    pub fn direct_user(&self) -> Option<&User> {
        match self {
            Principal::User { user, .. } => Some(user),
            _ => None,
        }
    }

    /// Sees internal projects: a signed-in person, not an external user or a bot.
    pub fn sees_internal(&self) -> bool {
        self.direct_user().is_some_and(|u| !u.is_bot() && !u.is_external)
    }

    pub fn is_admin(&self) -> bool {
        self.direct_user().is_some_and(|u| u.is_admin)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Principal::Anonymous => "anonymous",
            Principal::User { user, .. } if user.is_bot() => "bot",
            Principal::User { .. } => "user",
            Principal::DeployKey { .. } => "deploy_key",
            Principal::DeployToken { .. } => "deploy_token",
            Principal::Job { .. } => "job",
        }
    }

    pub fn id(&self) -> Option<i64> {
        match self {
            Principal::Anonymous => None,
            Principal::User { user, .. } => Some(user.id),
            Principal::DeployKey { key_id } => Some(*key_id),
            Principal::DeployToken { token } => Some(token.id),
            Principal::Job { token_id, .. } => Some(*token_id),
        }
    }

    /// Refuse what hands out lasting access (keys, tokens, runners) to an
    /// administrator signed in as someone else.
    /// Credentials that outlive the request (deploy keys and tokens, runners)
    /// are made from the person's own browser session: not by a token (a
    /// leaked or short-lived one would leave a lasting way in), not by an
    /// administrator signed in as them.
    pub fn may_mint_credentials(&self) -> Result<(), ApiError> {
        if self.impersonator().is_some() {
            return Err(ApiError::forbidden("not while signed in as someone else: credentials stay theirs to create"));
        }
        if !matches!(self, Principal::User { scopes: None, .. }) {
            return Err(ApiError::forbidden("credentials can only be created from a signed-in browser session, not with a token"));
        }
        Ok(())
    }

    /// The administrator signed in as this user, if any.
    pub fn impersonator(&self) -> Option<i64> {
        match self {
            Principal::User { scopes: Some(s), .. } => s.iter().find_map(|x| x.strip_prefix(IMPERSONATED_BY)?.parse().ok()),
            _ => None,
        }
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        match self {
            Principal::User { scopes: None, .. } => true,
            Principal::User { scopes: Some(s), .. } => {
                s.iter().any(|x| x == scope || (x == "api" && !matches!(scope, "sudo" | "admin_mode")))
            }
            Principal::DeployToken { token } => token.scopes.iter().any(|x| x == scope),
            _ => false,
        }
    }

    /// Serialized for git hooks: `kind:id`.
    pub fn encode(&self) -> String {
        format!("{}:{}", self.kind(), self.id().unwrap_or(0))
    }
}

/// Resolve a token presented as a password or bearer token.
pub async fn principal_from_token(state: &AppState, token: &str) -> Result<Option<Principal>, ApiError> {
    let hash = crypto::token_hash(token);
    if token.starts_with(crypto::prefix::PERSONAL) {
        if let Some((t, user)) = forgust_db::tokens::find_pat_with_user(&state.db, &hash).await? {
            if !user.is_active() {
                return Ok(None);
            }
            // a write only when the last use is a minute old (as the query checks too)
            if t.last_used_at.is_none_or(|at| chrono::Utc::now() - at > chrono::Duration::minutes(1)) {
                forgust_db::tokens::touch_pat(&state.db, t.id).await?;
            }
            return Ok(Some(Principal::User { user, scopes: Some(t.scopes) }));
        }
    } else if token.starts_with(crypto::prefix::DEPLOY) {
        if let Some(t) = forgust_db::tokens::find_deploy_token(&state.db, &hash).await? {
            return Ok(Some(Principal::DeployToken { token: t }));
        }
    } else if token.starts_with(crypto::prefix::JOB)
        && let Some((t, user)) = forgust_db::tokens::find_job_token(&state.db, &hash).await?
    {
        // the job acts as its user: a blocked user's jobs lose access too
        if !user.is_active() {
            return Ok(None);
        }
        return Ok(Some(Principal::Job { token_id: t.id, project_id: t.project_id, user }));
    }
    Ok(None)
}

/// Extractor: the request's principal (Anonymous when no credentials).
/// Sessions must send the CSRF header on unsafe methods; tokens need `api` (or
/// `read_api` for reads).
pub struct Viewer(pub Principal);

impl FromRequestParts<AppState> for Viewer {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let read_only = matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
        let header_token = parts.headers.get("private-token").and_then(|v| v.to_str().ok()).map(str::to_string).or_else(|| {
            parts
                .headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_string)
        });
        if let Some(token) = header_token {
            let p = principal_from_token(state, token.trim()).await?.ok_or(ApiError::Unauthorized)?;
            let ok = if read_only { p.has_scope("read_api") || p.has_scope("api") } else { p.has_scope("api") };
            if !ok || matches!(p, Principal::DeployToken { .. } | Principal::Job { .. }) {
                return Err(ApiError::forbidden("token scope does not allow this request"));
            }
            return Ok(Viewer(p));
        }

        let jar = CookieJar::from_headers(&parts.headers);
        if let Some(c) = session_cookie(&state.cfg, &jar)
            && let Some((user, csrf, impersonator)) = session::lookup(state, c).await?
        {
            if !read_only {
                let sent = parts.headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
                if !crypto::eq_ct(sent, &csrf) {
                    return Err(ApiError::forbidden("missing or invalid CSRF token"));
                }
            }
            // an administrator signed in as this user: everything the user may do
            // through the API, nothing that mints credentials or changes the
            // account (those need an unscoped session), audited as the admin's
            let scopes = impersonator.map(|admin| vec!["api".to_string(), format!("{IMPERSONATED_BY}{admin}")]);
            let principal = Principal::User { user, scopes };
            // every change made while impersonating is in the audit log
            if impersonator.is_some() && !read_only {
                crate::services::audit(
                    state,
                    &principal,
                    "impersonated_request",
                    None,
                    serde_json::json!({
                        "method": parts.method.as_str(),
                        // the full path (nested routers see theirs only)
                        "path": parts.extensions.get::<axum::extract::OriginalUri>().map_or(parts.uri.path(), |u| u.0.path()),
                    }),
                    None,
                )
                .await;
            }
            return Ok(Viewer(principal));
        }
        Ok(Viewer(Principal::Anonymous))
    }
}

/// Extractor for the few routes CI jobs may call with GitLab's `JOB-TOKEN`
/// header (releases, uploads): that job's principal, otherwise as [`Viewer`].
pub struct JobOrViewer(pub Principal);

impl FromRequestParts<AppState> for JobOrViewer {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if let Some(token) = parts.headers.get("job-token").and_then(|v| v.to_str().ok()) {
            return match principal_from_token(state, token.trim()).await? {
                Some(p @ Principal::Job { .. }) => Ok(JobOrViewer(p)),
                _ => Err(ApiError::Unauthorized),
            };
        }
        Viewer::from_request_parts(parts, state).await.map(|Viewer(p)| JobOrViewer(p))
    }
}

/// Extractor: like [`Viewer`] but rejects anonymous requests.
pub struct SignedIn(pub User, pub Principal);

impl FromRequestParts<AppState> for SignedIn {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let Viewer(p) = Viewer::from_request_parts(parts, state).await?;
        match p.direct_user() {
            Some(u) => Ok(SignedIn(u.clone(), p)),
            None => Err(ApiError::Unauthorized),
        }
    }
}

/// Extractor: instance administrators only.
pub struct Admin(pub User, pub Principal);

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let SignedIn(u, p) = SignedIn::from_request_parts(parts, state).await?;
        if !u.is_admin {
            return Err(ApiError::forbidden("administrator access required"));
        }
        // as GitLab's admin mode: an administrator's token reaches the admin
        // API only when made for it (an `api` token given to some application,
        // an MCP client say, must not be able to create administrators)
        if matches!(&p, Principal::User { scopes: Some(_), .. }) && !p.has_scope("admin_mode") {
            return Err(ApiError::forbidden("the admin API needs a token with the admin_mode scope (or a signed-in session)"));
        }
        Ok(Admin(u, p))
    }
}

/// The client's address (rate limits, audit, sessions): the TCP peer or, when
/// the peer is one of `server.trusted_proxies`, the last `X-Forwarded-For` hop
/// a trusted proxy did not add. Headers from anyone else are ignored, since
/// the client writes them.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClientIp(pub Option<std::net::IpAddr>);

impl ClientIp {
    pub fn resolve(peer: Option<std::net::IpAddr>, headers: &axum::http::HeaderMap, trusted: &[ipnet::IpNet]) -> Option<std::net::IpAddr> {
        let trusts = |ip: &std::net::IpAddr| trusted.iter().any(|n| n.contains(ip));
        let mut ip = peer?.to_canonical();
        if !trusts(&ip) {
            return Some(ip);
        }
        // every proxy appends the address it saw: walk back from the nearest
        let xff: Vec<&str> = headers.get_all("x-forwarded-for").iter().filter_map(|v| v.to_str().ok()).collect();
        let xff = xff.join(",");
        if xff.trim().is_empty() {
            let real = headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
            return Some(real.map_or(ip, |r: std::net::IpAddr| r.to_canonical()));
        }
        for hop in xff.rsplit(',') {
            let Ok(hop) = hop.trim().parse::<std::net::IpAddr>() else { break };
            ip = hop.to_canonical();
            if !trusts(&ip) {
                break;
            }
        }
        Some(ip)
    }

    pub fn of(state: &AppState, headers: &axum::http::HeaderMap, extensions: &axum::http::Extensions) -> Self {
        let peer = extensions.get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
        ClientIp(Self::resolve(peer, headers, &state.cfg.server.trusted_proxies))
    }

    /// As stored in audit records and sessions.
    pub fn text(&self) -> Option<String> {
        self.0.map(|ip| ip.to_string())
    }
}

/// Anonymous visitors get a budget of expensive reads (blame, history,
/// archives) per client; signed-in users and unknown clients are not counted.
pub fn heavy_read_allowed(state: &AppState, p: &Principal, ip: ClientIp) -> crate::error::ApiResult<()> {
    if !matches!(p, Principal::Anonymous) {
        return Ok(());
    }
    match ip.0 {
        Some(ip) if !state.anon_heavy_reads.allow(&ip.to_string()) => {
            Err(crate::error::ApiError::TooMany("too many requests from this address; sign in or try again in a minute".into()))
        }
        _ => Ok(()),
    }
}

/// Password guessing limits, shared by the web login and git over HTTP: 10
/// tries a minute per client and login, and 100 failures per 10 minutes per
/// client whatever the account. Longer logins than any username or address
/// are refused outright. The account's own limit is [`account_open`], once
/// the login names one.
pub fn login_allowed(state: &AppState, ip: ClientIp, login: &str) -> bool {
    if login.len() > 255 {
        return false;
    }
    let login = login.to_lowercase();
    let client = ip.text().unwrap_or_default();
    // an unknown client is no one in particular: no shared budget for all of them
    !ip.text().is_some_and(|c| state.login_ip_failures.exceeded(&c)) && state.login_limiter.allow(&format!("{client}|{login}"))
}

/// The account's limit: past 20 failures in 10 minutes (from anyone, by
/// username or address alike), only addresses it signed in from before may
/// keep trying. Someone guessing cannot lock its owner out.
pub async fn account_open(state: &AppState, ip: ClientIp, user_id: i64) -> Result<bool, ApiError> {
    if !state.login_failures.exceeded(&format!("acct|{user_id}")) {
        return Ok(true);
    }
    match ip.text() {
        Some(addr) => Ok(forgust_db::sessions::signed_in_from(&state.db, user_id, &addr).await?),
        None => Ok(false),
    }
}

/// Record the outcome of a password check `login_allowed` let through (the
/// account, when the login named one).
pub fn login_checked(state: &AppState, ip: ClientIp, login: &str, user_id: Option<i64>, ok: bool) {
    let login = login.to_lowercase();
    if ok {
        state.login_limiter.forget(&format!("{}|{login}", ip.text().unwrap_or_default()));
    } else {
        if let Some(id) = user_id {
            state.login_failures.allow(&format!("acct|{id}"));
        }
        if let Some(c) = ip.text() {
            state.login_ip_failures.allow(&c);
        }
    }
}

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        Ok(ClientIp::of(state, &parts.headers, &parts.extensions))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::ClientIp;

    fn resolve(peer: &str, xff: Option<&str>) -> String {
        let mut h = HeaderMap::new();
        if let Some(x) = xff {
            h.insert("x-forwarded-for", x.parse().unwrap());
        }
        let trusted = ["127.0.0.0/8".parse().unwrap(), "10.0.0.0/8".parse().unwrap()];
        ClientIp::resolve(Some(peer.parse().unwrap()), &h, &trusted).unwrap().to_string()
    }

    #[test]
    fn forwarded_headers_only_from_trusted_proxies() {
        // a client talking to us directly cannot pick its address
        assert_eq!(resolve("198.51.100.7", Some("1.2.3.4")), "198.51.100.7");
        // behind a proxy: the hop it saw, not what the client wrote before it
        assert_eq!(resolve("127.0.0.1", Some("1.2.3.4, 198.51.100.7")), "198.51.100.7");
        // through two trusted proxies
        assert_eq!(resolve("127.0.0.1", Some("1.2.3.4, 198.51.100.7, 10.0.0.5")), "198.51.100.7");
        // garbage stops the walk at the last good hop
        assert_eq!(resolve("127.0.0.1", Some("1.2.3.4, nonsense, 10.0.0.5")), "10.0.0.5");
        assert_eq!(resolve("127.0.0.1", None), "127.0.0.1");
        assert_eq!(resolve("::ffff:198.51.100.7", Some("1.2.3.4")), "198.51.100.7");
    }
}
