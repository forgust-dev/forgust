//! OAuth 2.1 sign-in for MCP clients (ADR-0025), as MCP's
//! authorization specification asks: protected-resource and authorization-server
//! metadata (RFC 9728, RFC 8414), dynamic client registration (RFC 7591, public
//! clients only), the authorization code flow with PKCE S256, and rotating
//! refresh tokens. An access token is a personal access token of the person who
//! consented (scopes `mcp`, plus `mcp_write` when they allowed changes: the
//! REST API refuses both), listed with
//! their tokens: revoking it there ends the client's access, refresh included.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower_http::cors::{Any, CorsLayer};
use utoipa::{IntoParams, ToSchema};

use crate::api::user::{require_confirmed, require_interactive};
use crate::auth::{ClientIp, SignedIn};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::services;
use crate::state::{AppState, RateLimiter};

/// Access tokens live an hour; refresh tokens a month (each use renews it).
const ACCESS_TTL: i64 = 3600;
const REFRESH_DAYS: i64 = 30;
const CODE_MINUTES: i64 = 10;
/// Where MCP clients connect (GitLab's path first).
pub const MCP_PATHS: &[&str] = &["/api/v4/mcp", "/api/mcp"];

/// The MCP endpoint and the OAuth endpoints a client calls (from anywhere:
/// browser-based clients need CORS; no cookies are read here).
pub fn router() -> Router<AppState> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::HeaderName::from_static("mcp-protocol-version"),
            header::HeaderName::from_static("private-token"),
        ])
        .expose_headers([header::WWW_AUTHENTICATE]);
    let mut r = Router::new()
        .route("/.well-known/oauth-protected-resource", get(resource_metadata))
        .route("/.well-known/oauth-authorization-server", get(server_metadata))
        .route("/oauth/register", post(register))
        .route("/oauth/token", post(token));
    for p in MCP_PATHS {
        r = r
            .route(p, post(crate::mcp::post).get(crate::mcp::get))
            .route(&format!("/.well-known/oauth-protected-resource{p}"), get(resource_metadata));
    }
    r.layer(cors)
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    no_store((status, Json(json!({ "error": error, "error_description": description }))).into_response())
}

/// RFC 9728: which resource this is and who issues its tokens.
async fn resource_metadata(State(state): State<AppState>, OriginalUri(uri): OriginalUri) -> Json<Value> {
    let base = &state.cfg.server.external_url;
    let path = uri.path().strip_prefix("/.well-known/oauth-protected-resource").filter(|p| MCP_PATHS.contains(p)).unwrap_or(MCP_PATHS[0]);
    Json(json!({
        "resource": format!("{base}{path}"),
        "authorization_servers": [base],
        "scopes_supported": ["mcp", "mcp_write"],
        "bearer_methods_supported": ["header"],
        "resource_name": "Forgust",
    }))
}

/// RFC 8414.
async fn server_metadata(State(state): State<AppState>) -> Json<Value> {
    let base = &state.cfg.server.external_url;
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["mcp", "mcp_write"],
    }))
}

/// A redirect URI a public client may register: https, a loopback http
/// address (any port, RFC 8252), or an app's own scheme.
fn redirect_allowed(u: &str) -> bool {
    let Ok(url) = url::Url::parse(u) else { return false };
    if url.fragment().is_some() || u.len() > 2000 {
        return false;
    }
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => is_loopback(&url),
        "javascript" | "data" | "file" | "vbscript" | "blob" | "about" | "ftp" | "ws" | "wss" => false,
        _ => true,
    }
}

fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The redirect URI matches one registered: exactly, or a loopback one on
/// another port (native apps pick a free port each time).
fn redirect_matches(registered: &[String], asked: &str) -> bool {
    if registered.iter().any(|r| r == asked) {
        return true;
    }
    let Ok(mut a) = url::Url::parse(asked) else { return false };
    if a.scheme() != "http" || !is_loopback(&a) {
        return false;
    }
    let _ = a.set_port(None);
    registered.iter().filter_map(|r| url::Url::parse(r).ok()).any(|mut r| {
        let _ = r.set_port(None);
        r.scheme() == "http" && r == a
    })
}

#[derive(Deserialize)]
struct Registration {
    #[serde(default)]
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    token_endpoint_auth_method: Option<String>,
}

/// RFC 7591 dynamic registration: anyone may register a public client (it
/// can do nothing until a person consents), a few an hour per address.
async fn register(State(state): State<AppState>, ClientIp(ip): ClientIp, body: axum::body::Bytes) -> Response {
    static LIMITER: OnceLock<RateLimiter> = OnceLock::new();
    let limiter = LIMITER.get_or_init(|| RateLimiter::new(30, Duration::from_secs(3600)));
    if !limiter.allow(&ip.map(|i| i.to_string()).unwrap_or_default()) {
        return oauth_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many registrations from this address");
    }
    let Ok(reg) = serde_json::from_slice::<Registration>(&body) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "a JSON body with redirect_uris is required");
    };
    if reg.redirect_uris.is_empty() || reg.redirect_uris.len() > 10 {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", "between 1 and 10 redirect_uris");
    }
    if let Some(bad) = reg.redirect_uris.iter().find(|u| !redirect_allowed(u)) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            &format!("{bad}: use https, a loopback http address, or an app scheme"),
        );
    }
    if reg.token_endpoint_auth_method.as_deref().is_some_and(|m| m != "none") {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "only public clients (token_endpoint_auth_method none, with PKCE)",
        );
    }
    let name: String = reg
        .client_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("MCP client")
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let client_id = format!("mcp-{}", crypto::random_urlsafe(18));
    if let Err(e) = forgust_db::oauth::create_client(&state.db, &client_id, &name, &reg.redirect_uris).await {
        return ApiError::from(e).into_response();
    }
    no_store(
        (
            StatusCode::CREATED,
            Json(json!({
                "client_id": client_id, "client_name": name, "redirect_uris": reg.redirect_uris,
                "client_id_issued_at": Utc::now().timestamp(), "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
            })),
        )
            .into_response(),
    )
}

// ------------------------------------------------------------ consent (SPA + /api/v1)

#[derive(Deserialize, IntoParams, ToSchema)]
pub struct AuthorizeRequest {
    pub client_id: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub response_type: String,
    #[serde(default)]
    pub code_challenge: String,
    #[serde(default)]
    pub code_challenge_method: String,
    #[serde(default)]
    pub scope: String,
}

#[derive(Serialize, ToSchema)]
pub struct AuthorizeInfo {
    pub client_name: String,
    /// Where the browser goes back to (the client's host, or its app scheme).
    pub redirect_to: String,
    /// The client asked to change things (`api`), not only read.
    pub write_requested: bool,
}

async fn checked(state: &AppState, req: &AuthorizeRequest) -> ApiResult<forgust_db::oauth::Client> {
    if !forgust_db::settings::mcp_enabled(&state.db).await? {
        return Err(ApiError::forbidden("an administrator turned the MCP server off"));
    }
    let client = forgust_db::oauth::find_client(&state.db, &req.client_id)
        .await?
        .ok_or_else(|| ApiError::bad("unknown client: register it again"))?;
    if !redirect_matches(&client.redirect_uris, &req.redirect_uri) {
        return Err(ApiError::bad("this redirect address is not one the client registered"));
    }
    if req.response_type != "code" {
        return Err(ApiError::bad("response_type must be code"));
    }
    if req.code_challenge_method != "S256" || req.code_challenge.len() < 43 || req.code_challenge.len() > 128 {
        return Err(ApiError::bad("PKCE is required: code_challenge with code_challenge_method S256"));
    }
    Ok(client)
}

fn redirect_label(uri: &str) -> String {
    url::Url::parse(uri)
        .map(|u| match u.host_str() {
            Some(h) if u.scheme() == "https" || u.scheme() == "http" => h.to_string(),
            _ => format!("{}://", u.scheme()),
        })
        .unwrap_or_default()
}

/// What the consent page shows: the client and what it asks for.
#[utoipa::path(get, path = "/api/v1/oauth/authorize", params(AuthorizeRequest), responses((status = 200, body = AuthorizeInfo)), tag = "user")]
pub async fn authorize_info(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    Query(req): Query<AuthorizeRequest>,
) -> ApiResult<Json<AuthorizeInfo>> {
    require_interactive(&u, &p)?;
    let client = checked(&state, &req).await?;
    Ok(Json(AuthorizeInfo {
        client_name: client.name,
        redirect_to: redirect_label(&req.redirect_uri),
        write_requested: req.scope.split_whitespace().any(|s| s == "api" || s == "mcp_write"),
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct AuthorizeDecision {
    #[serde(flatten)]
    pub request: AuthorizeRequest,
    pub state: Option<String>,
    /// The person allowed the client in (false: denied).
    pub allow: bool,
    /// They also allowed changes (the `api` scope).
    #[serde(default)]
    pub write: bool,
}

#[derive(Serialize, ToSchema)]
pub struct AuthorizeRedirect {
    /// Where to send the browser: the client's redirect URI with a code (or an error).
    pub redirect: String,
}

/// The person's answer: a one-time code for the client, or `access_denied`.
#[utoipa::path(post, path = "/api/v1/oauth/authorize", request_body = AuthorizeDecision, responses((status = 200, body = AuthorizeRedirect)), tag = "user")]
pub async fn authorize(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    Json(d): Json<AuthorizeDecision>,
) -> ApiResult<Json<AuthorizeRedirect>> {
    require_interactive(&u, &p)?;
    require_confirmed(&state, &u)?;
    if p.impersonator().is_some() {
        return Err(ApiError::forbidden("an administrator signed in as someone cannot give an application access to their account"));
    }
    let client = checked(&state, &d.request).await?;
    let mut url = url::Url::parse(&d.request.redirect_uri).map_err(|_| ApiError::bad("invalid redirect_uri"))?;
    let answer = if d.allow {
        let code = crypto::random_urlsafe(32);
        // never `api`: the token writes through MCP's tools only, not the REST API
        let scopes: Vec<String> = if d.write { vec!["mcp".into(), "mcp_write".into()] } else { vec!["mcp".into()] };
        let expires = Utc::now() + chrono::Duration::minutes(CODE_MINUTES);
        forgust_db::oauth::create_code(
            &state.db,
            &crypto::token_hash(&code),
            &client.client_id,
            u.id,
            &d.request.redirect_uri,
            &d.request.code_challenge,
            &scopes,
            expires,
        )
        .await?;
        services::audit(
            &state,
            &p,
            "oauth_authorized",
            Some(("user", u.id)),
            json!({ "client": client.name, "client_id": client.client_id, "scopes": scopes }),
            None,
        )
        .await;
        ("code", code)
    } else {
        ("error", "access_denied".to_string())
    };
    {
        let mut q = url.query_pairs_mut();
        q.append_pair(answer.0, &answer.1);
        if let Some(s) = &d.state {
            q.append_pair("state", s);
        }
        q.append_pair("iss", &state.cfg.server.external_url);
    }
    Ok(Json(AuthorizeRedirect { redirect: url.to_string() }))
}

// ------------------------------------------------------------ token

fn pkce_ok(verifier: &str, challenge: &str) -> bool {
    (43..=128).contains(&verifier.len()) && crypto::eq_ct(&URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())), challenge)
}

fn token_name(client: &str) -> String {
    format!("MCP: {}", client.chars().take(60).collect::<String>())
}

/// `POST /oauth/token` (form-encoded): a code or a refresh token for tokens.
async fn token(State(state): State<AppState>, Form(f): Form<HashMap<String, String>>) -> Response {
    match token_grant(&state, &f).await {
        Ok(v) => no_store(Json(v).into_response()),
        Err((error, description)) => oauth_error(
            if error == "server_error" { StatusCode::INTERNAL_SERVER_ERROR } else { StatusCode::BAD_REQUEST },
            error,
            &description,
        ),
    }
}

type Grant = Result<Value, (&'static str, String)>;

fn server_error(e: impl std::fmt::Display) -> (&'static str, String) {
    tracing::error!("oauth token: {e}");
    ("server_error", "try again".into())
}

async fn token_grant(state: &AppState, f: &HashMap<String, String>) -> Grant {
    let arg = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    // turning the MCP server off also stops its clients' sign-ins and refreshes
    if !forgust_db::settings::mcp_enabled(&state.db).await.unwrap_or(false) {
        return Err(("access_denied", "an administrator turned the MCP server off".into()));
    }
    let invalid = |m: &str| ("invalid_grant", m.to_string());
    let client = forgust_db::oauth::find_client(&state.db, arg("client_id"))
        .await
        .map_err(server_error)?
        .ok_or(("invalid_client", "unknown client_id".to_string()))?;
    let (user_id, scopes, token_id) = match arg("grant_type") {
        "authorization_code" => {
            let code = forgust_db::oauth::take_code(&state.db, &crypto::token_hash(arg("code")))
                .await
                .map_err(server_error)?
                .ok_or_else(|| invalid("unknown, used or expired code"))?;
            if code.client_id != client.client_id || code.redirect_uri != arg("redirect_uri") {
                return Err(invalid("the code was issued to another client or redirect_uri"));
            }
            if !pkce_ok(arg("code_verifier"), &code.code_challenge) {
                return Err(invalid("code_verifier does not match the code_challenge"));
            }
            (code.user_id, code.scopes, None)
        }
        "refresh_token" => {
            let mut conn = state.db.acquire().await.map_err(server_error)?;
            match forgust_db::oauth::use_refresh(&mut conn, &crypto::token_hash(arg("refresh_token"))).await.map_err(server_error)? {
                forgust_db::oauth::RefreshUse::Fresh(r) => {
                    if r.client_id != client.client_id {
                        return Err(invalid("the refresh token was issued to another client"));
                    }
                    (r.user_id, r.scopes, Some(r.access_token_id.ok_or_else(|| invalid("the access token was revoked"))?))
                }
                // used twice: a copy is out there, so nobody keeps this grant
                forgust_db::oauth::RefreshUse::Reused { user_id, access_token_id } => {
                    if let Some(id) = access_token_id {
                        forgust_db::tokens::revoke_pat(&mut *conn, user_id, id).await.map_err(server_error)?;
                        forgust_db::oauth::delete_family(&mut *conn, id).await.map_err(server_error)?;
                        tracing::warn!(user_id, token_id = id, "OAuth refresh token used twice: access revoked");
                    }
                    return Err(invalid("refresh token already used: access revoked, sign in again"));
                }
                forgust_db::oauth::RefreshUse::Unknown => return Err(invalid("unknown or expired refresh token")),
            }
        }
        _ => return Err(("unsupported_grant_type", "authorization_code or refresh_token".into())),
    };
    let user = forgust_db::users::find(&state.db, user_id)
        .await
        .map_err(server_error)?
        .filter(|u| u.is_active())
        .ok_or_else(|| invalid("the account is not active"))?;
    let access = crypto::random_token(crypto::prefix::PERSONAL);
    let expires = Utc::now() + chrono::Duration::seconds(ACCESS_TTL);
    let id = match token_id {
        // the same token, a new secret: one entry in the person's list per client
        Some(id) => {
            if !forgust_db::tokens::rotate_pat(&state.db, id, user.id, &crypto::token_hash(&access), expires).await.map_err(server_error)? {
                return Err(invalid("the access token was revoked"));
            }
            id
        }
        None => forgust_db::tokens::create_pat(
            &state.db,
            user.id,
            &token_name(&client.name),
            &crypto::token_hash(&access),
            &scopes,
            Some(expires),
        )
        .await
        .map_err(server_error)?,
    };
    let refresh = crypto::random_urlsafe(32);
    let refresh_expires = Utc::now() + chrono::Duration::days(REFRESH_DAYS);
    forgust_db::oauth::create_refresh(&state.db, &crypto::token_hash(&refresh), &client.client_id, user.id, &scopes, id, refresh_expires)
        .await
        .map_err(server_error)?;
    Ok(
        json!({ "access_token": access, "token_type": "Bearer", "expires_in": ACCESS_TTL, "refresh_token": refresh, "scope": scopes.join(" ") }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects() {
        assert!(redirect_allowed("https://claude.ai/api/mcp/auth_callback"));
        assert!(redirect_allowed("http://127.0.0.1:33418/callback"));
        assert!(redirect_allowed("http://localhost/cb"));
        assert!(redirect_allowed("cursor://anysphere.cursor-retrieval/oauth/callback"));
        assert!(!redirect_allowed("http://evil.example/cb"));
        assert!(!redirect_allowed("javascript:alert(1)"));
        assert!(!redirect_allowed("https://a.example/cb#frag"));
        let reg = vec!["http://127.0.0.1:5000/callback".to_string(), "https://app.example/cb".to_string()];
        assert!(redirect_matches(&reg, "http://127.0.0.1:61234/callback"));
        assert!(!redirect_matches(&reg, "http://127.0.0.1:61234/other"));
        assert!(redirect_matches(&reg, "https://app.example/cb"));
        assert!(!redirect_matches(&reg, "https://app.example:8443/cb"));
    }

    #[test]
    fn pkce() {
        // RFC 7636, appendix B
        assert!(pkce_ok("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!pkce_ok("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXx", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
    }
}
