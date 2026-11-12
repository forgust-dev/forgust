//! The container registry (ADR-0022): the OCI Distribution API at `/v2/`, with
//! GitLab's token flow at `/jwt/auth`.

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use forgust_db::projects::Project;
use forgust_db::registry::{self as rdb, Tag};
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::auth::authz::{ProjectAction, can_project};
use crate::auth::{ClientIp, Principal};
use crate::error::ApiError;
use crate::state::AppState;

pub const SERVICE: &str = "container_registry";
const API_VERSION: &str = "registry/2.0";
/// Largest manifest accepted (Distribution's limit is 4 MB too).
const MAX_MANIFEST: usize = 4 << 20;

pub fn router() -> Router<AppState> {
    Router::new().route("/jwt/auth", get(token)).route("/v2/", any(base)).route("/v2", any(base)).route("/v2/{*rest}", any(dispatch))
}

// ------------------------------------------------------------------ errors

/// A registry error: `{"errors": [{"code", "message"}]}` with the status the spec asks for.
struct RegError {
    status: StatusCode,
    code: &'static str,
    message: String,
    /// Ask for credentials (with this scope) on 401.
    challenge: Option<String>,
}

impl RegError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        RegError { status, code, message: message.into(), challenge: None }
    }
    fn name_unknown() -> Self {
        RegError::new(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "repository name not known to registry")
    }
    fn blob_unknown() -> Self {
        RegError::new(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", "blob unknown to registry")
    }
    fn manifest_unknown() -> Self {
        RegError::new(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN", "manifest unknown")
    }
    fn upload_unknown() -> Self {
        RegError::new(StatusCode::NOT_FOUND, "BLOB_UPLOAD_UNKNOWN", "blob upload unknown to registry")
    }
    fn unsupported() -> Self {
        RegError::new(StatusCode::METHOD_NOT_ALLOWED, "UNSUPPORTED", "the operation is unsupported")
    }
}

impl From<ApiError> for RegError {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::Unauthorized => RegError::new(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "authentication required"),
            ApiError::Forbidden(m) => RegError::new(StatusCode::FORBIDDEN, "DENIED", m),
            ApiError::NotFound => RegError::name_unknown(),
            ApiError::TooLarge(m) => RegError::new(StatusCode::PAYLOAD_TOO_LARGE, "SIZE_INVALID", m),
            ApiError::BadRequest(m) => RegError::new(StatusCode::BAD_REQUEST, "UNSUPPORTED", m),
            other => {
                tracing::error!(error = %other, "registry");
                RegError::new(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "internal error")
            }
        }
    }
}

impl From<anyhow::Error> for RegError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e).into()
    }
}

impl From<sqlx::Error> for RegError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::from(e).into()
    }
}

type RegResult<T> = Result<T, RegError>;

fn with_api_version(mut r: Response) -> Response {
    r.headers_mut().insert("Docker-Distribution-API-Version", HeaderValue::from_static(API_VERSION));
    r
}

fn error_response(state: &AppState, e: RegError) -> Response {
    let mut r = (e.status, axum::Json(json!({ "errors": [{ "code": e.code, "message": e.message, "detail": null }] }))).into_response();
    if e.status == StatusCode::UNAUTHORIZED {
        let mut v = format!("Bearer realm=\"{}/jwt/auth\",service=\"{SERVICE}\"", state.cfg.server.external_url);
        if let Some(scope) = &e.challenge {
            v.push_str(&format!(",scope=\"{scope}\""));
        }
        if let Ok(h) = HeaderValue::from_str(&v) {
            r.headers_mut().insert(header::WWW_AUTHENTICATE, h);
        }
    }
    with_api_version(r)
}

// ------------------------------------------------------------------ tokens

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Access {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    /// The user the catalog is listed for (by id: a username can change hands).
    #[serde(default)]
    uid: Option<i64>,
    iat: i64,
    exp: i64,
    access: Vec<Access>,
}

/// A compact JWT signed with the server's link key (HS256 over a prefixed
/// input: only Forgust itself reads these tokens).
fn sign(state: &AppState, claims: &Claims) -> String {
    let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("serializable"));
    let sig = state.secrets.sign(&format!("registry-jwt:{head}.{body}"));
    format!("{head}.{body}.{sig}")
}

fn verify(state: &AppState, token: &str) -> Option<Claims> {
    let mut it = token.split('.');
    let (head, body, sig) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || !state.secrets.verify(&format!("registry-jwt:{head}.{body}"), sig) {
        return None;
    }
    let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
    (claims.aud == SERVICE && claims.exp > chrono::Utc::now().timestamp()).then_some(claims)
}

/// `group/project[/name...]` → the project and the image name below it.
async fn resolve(state: &AppState, name: &str) -> RegResult<Option<(Project, String)>> {
    if !valid_name(name) {
        return Ok(None);
    }
    let segments: Vec<&str> = name.split('/').collect();
    let mut conn = state.db.acquire().await?;
    let shortest = segments.len().saturating_sub(4).max(1);
    for k in (shortest..=segments.len()).rev() {
        let path = segments[..k].join("/");
        if let Some(hit) = forgust_db::routes::lookup(&mut conn, &path).await?
            && hit.source_type == "project"
            && !hit.redirected
            && let Some(p) = forgust_db::projects::find(&mut *conn, hit.source_id).await?
        {
            return Ok(Some((p, segments[k..].join("/"))));
        }
    }
    Ok(None)
}

/// The Distribution spec's repository name grammar (lower case components).
fn valid_name(name: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE
        .get_or_init(|| regex::Regex::new(r"^[a-z0-9]+(?:(?:\.|_|__|-+)[a-z0-9]+)*(?:/[a-z0-9]+(?:(?:\.|_|__|-+)[a-z0-9]+)*)*$").unwrap());
    name.len() <= 255 && re.is_match(name)
}

/// What a principal may do on a project's images.
async fn actions_for(state: &AppState, principal: &Principal, p: &Project) -> RegResult<Vec<String>> {
    let mut actions = Vec::new();
    let (pull, push) = match principal {
        Principal::DeployToken { token } => {
            let applies = token.project_id == Some(p.id) || token.namespace_id.is_some_and(|ns| p.traversal_ids.contains(&ns));
            let has = |s: &str| applies && token.scopes.iter().any(|x| x == s);
            (has("read_registry") || has("write_registry"), has("write_registry"))
        }
        Principal::Job { project_id, user, .. } => {
            let pull = can_project(state, principal, ProjectAction::ReadCode, p).await?;
            // a job pushes to its own project's images when its user may push
            let push = *project_id == p.id
                && (user.is_admin
                    || forgust_db::authz::project_level(&state.db, user.id, p.id).await?.unwrap_or(0) >= forgust_db::access::DEVELOPER);
            (pull, push)
        }
        _ => {
            let pull = can_project(state, principal, ProjectAction::ReadCode, p).await?;
            (pull, pull && can_project(state, principal, ProjectAction::Push, p).await?)
        }
    };
    if pull {
        actions.push("pull".to_string());
    }
    // archived: images stay readable, nothing is pushed or deleted
    if push && !p.archived() {
        actions.extend(["push".to_string(), "delete".to_string()]);
    }
    Ok(actions)
}

#[derive(Deserialize)]
struct TokenQuery {
    service: Option<String>,
}

/// `GET /jwt/auth?service=container_registry&scope=repository:<name>:pull,push`
async fn token(State(state): State<AppState>, Query(q): Query<TokenQuery>, ip: ClientIp, uri: Uri, headers: HeaderMap) -> Response {
    let result: RegResult<Response> = async {
        if q.service.as_deref().is_some_and(|s| s != SERVICE) {
            return Err(RegError::new(StatusCode::BAD_REQUEST, "UNSUPPORTED", "unknown service"));
        }
        let principal = crate::git_http::basic_principal(&state, &headers, ip).await.map_err(|_| {
            RegError::new(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "HTTP Basic: Access denied (use a personal access token when two-factor authentication is on)",
            )
        })?;
        // `scope` may come several times
        let scopes: Vec<String> = uri
            .query()
            .unwrap_or("")
            .split('&')
            .filter_map(|kv| kv.strip_prefix("scope="))
            .map(|v| percent_encoding::percent_decode_str(&v.replace('+', " ")).decode_utf8_lossy().into_owned())
            .collect();
        let mut access = Vec::new();
        for scope in scopes.iter().flat_map(|s| s.split(' ')).filter(|s| !s.is_empty()) {
            let mut parts = scope.splitn(3, ':');
            let (kind, name, wanted) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            if kind == "registry" && name == "catalog" {
                // a person, with a session or a token for API reads: not a CI job
                // or a deploy token (their reach is one project), not a narrow token
                if matches!(principal, Principal::User { .. }) && principal.has_scope("read_api") {
                    access.push(Access { kind: kind.into(), name: name.into(), actions: vec!["*".into()] });
                }
                continue;
            }
            if kind != "repository" {
                continue;
            }
            let Some((p, _)) = resolve(&state, name).await? else { continue };
            let allowed = actions_for(&state, &principal, &p).await?;
            let granted: Vec<String> = wanted
                .split(',')
                .flat_map(|a| if a == "*" { allowed.clone() } else { vec![a.to_string()] })
                .filter(|a| allowed.contains(a))
                .collect();
            access.push(Access { kind: "repository".into(), name: name.into(), actions: granted });
        }
        let now = chrono::Utc::now().timestamp();
        let ttl = state.cfg.registry.token_ttl_secs as i64;
        let sub = match &principal {
            Principal::User { user, .. } | Principal::Job { user, .. } => user.username.clone(),
            Principal::DeployToken { token } => token.username.clone(),
            _ => String::new(),
        };
        let uid = match &principal {
            Principal::User { user, .. } => Some(user.id),
            _ => None,
        };
        let claims = Claims { iss: "forgust".into(), aud: SERVICE.into(), sub, uid, iat: now, exp: now + ttl, access };
        let t = sign(&state, &claims);
        let issued = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        Ok(axum::Json(json!({ "token": t, "access_token": t, "expires_in": ttl, "issued_at": issued })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| error_response(&state, e))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim)
}

/// `GET /v2/`: 200 with a valid token, else the challenge.
async fn base(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !state.cfg.registry.enabled {
        return error_response(&state, RegError::new(StatusCode::NOT_FOUND, "UNSUPPORTED", "the container registry is off"));
    }
    match bearer(&headers).and_then(|t| verify(&state, t)) {
        Some(_) => with_api_version(axum::Json(json!({})).into_response()),
        None => error_response(&state, RegError::new(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "authentication required")),
    }
}

// ------------------------------------------------------------------ routing

enum Route {
    Manifest(String),
    Blob(String),
    UploadStart,
    Upload(String),
    Tags,
    Referrers(String),
}

/// `<name>/manifests/<ref>`, `<name>/blobs/uploads/[<id>]`, `<name>/blobs/<digest>`,
/// `<name>/tags/list`, `<name>/referrers/<digest>`.
fn parse(rest: &str) -> Option<(String, Route)> {
    let rest = rest.trim_end_matches('/');
    if let Some(name) = rest.strip_suffix("/tags/list") {
        return Some((name.into(), Route::Tags));
    }
    if let Some(name) = rest.strip_suffix("/blobs/uploads") {
        return Some((name.into(), Route::UploadStart));
    }
    if let Some((name, id)) = rest.rsplit_once("/blobs/uploads/") {
        return Some((name.into(), Route::Upload(id.into())));
    }
    if let Some((name, d)) = rest.rsplit_once("/blobs/") {
        return Some((name.into(), Route::Blob(d.into())));
    }
    if let Some((name, r)) = rest.rsplit_once("/manifests/") {
        return Some((name.into(), Route::Manifest(r.into())));
    }
    if let Some((name, d)) = rest.rsplit_once("/referrers/") {
        return Some((name.into(), Route::Referrers(d.into())));
    }
    None
}

fn valid_digest(d: &str) -> bool {
    d.strip_prefix("sha256:").is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
}

fn blob_key(digest: &str) -> String {
    format!("registry/blobs/{}", digest.replace(':', "/"))
}

fn part_key(upload: uuid::Uuid, n: i32) -> String {
    format!("registry/uploads/{upload}/{n}")
}

/// The request's repository, checked against the token for `action`.
struct Target {
    project: Project,
    name: String,
    image: String,
}

async fn authorize(state: &AppState, headers: &HeaderMap, name: &str, action: &str) -> RegResult<Target> {
    let challenge = |msg: &str| RegError {
        status: StatusCode::UNAUTHORIZED,
        code: "UNAUTHORIZED",
        message: msg.into(),
        challenge: Some(format!("repository:{name}:{}", if action == "pull" { "pull" } else { "pull,push" })),
    };
    let Some(claims) = bearer(headers).and_then(|t| verify(state, t)) else { return Err(challenge("authentication required")) };
    let granted = claims.access.iter().any(|a| a.kind == "repository" && a.name == name && a.actions.iter().any(|x| x == action));
    if !granted {
        // nothing to say about repositories one cannot read
        return Err(if claims.access.iter().any(|a| a.name == name && !a.actions.is_empty()) {
            RegError::new(StatusCode::FORBIDDEN, "DENIED", format!("requested access to the resource is denied ({action})"))
        } else {
            challenge("authentication required")
        });
    }
    let (project, image) = resolve(state, name).await?.ok_or_else(RegError::name_unknown)?;
    // a token issued before the project was archived
    if action != "pull" && project.archived() {
        return Err(RegError::new(StatusCode::FORBIDDEN, "DENIED", crate::auth::authz::ARCHIVED));
    }
    Ok(Target { project, name: name.into(), image })
}

async fn dispatch(
    State(state): State<AppState>,
    axum::extract::Path(rest): axum::extract::Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !state.cfg.registry.enabled {
        return error_response(&state, RegError::new(StatusCode::NOT_FOUND, "UNSUPPORTED", "the container registry is off"));
    }
    let result: RegResult<Response> = async {
        if rest == "_catalog" {
            return catalog(&state, &uri, &headers).await;
        }
        let (name, route) = parse(&rest).ok_or_else(|| RegError::new(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "unknown endpoint"))?;
        let action = match (&method, &route) {
            (&Method::GET | &Method::HEAD, _) => "pull",
            (&Method::DELETE, Route::Manifest(_) | Route::Blob(_)) => "delete",
            _ => "push",
        };
        let t = authorize(&state, &headers, &name, action).await?;
        let head = method == Method::HEAD;
        match (method, route) {
            (Method::GET | Method::HEAD, Route::Manifest(r)) => get_manifest(&state, &t, &r, head).await,
            (Method::PUT, Route::Manifest(r)) => put_manifest(&state, &t, &r, &headers, body).await,
            (Method::DELETE, Route::Manifest(r)) => delete_manifest(&state, &t, &r).await,
            (Method::GET | Method::HEAD, Route::Blob(d)) => get_blob(&state, &t, &d, head).await,
            (Method::DELETE, Route::Blob(d)) => delete_blob(&state, &t, &d).await,
            (Method::POST, Route::UploadStart) => start_upload(&state, &t, &uri, &headers, body).await,
            (Method::GET, Route::Upload(id)) => upload_status(&state, &t, &id).await,
            (Method::PATCH, Route::Upload(id)) => patch_upload(&state, &t, &id, &headers, body).await,
            (Method::PUT, Route::Upload(id)) => finish_upload(&state, &t, &id, &uri, body).await,
            (Method::DELETE, Route::Upload(id)) => cancel_upload(&state, &t, &id).await,
            (Method::GET, Route::Tags) => list_tags(&state, &t, &uri).await,
            (Method::GET, Route::Referrers(d)) => referrers(&state, &t, &d).await,
            _ => Err(RegError::unsupported()),
        }
    }
    .await;
    match result {
        Ok(r) => with_api_version(r),
        Err(e) => error_response(&state, e),
    }
}

async fn repository(state: &AppState, t: &Target) -> RegResult<Option<rdb::Repository>> {
    Ok(rdb::repository(&state.db, t.project.id, &t.image).await?)
}

fn loc(t: &Target, tail: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("/v2/{}/{tail}", t.name)).unwrap_or(HeaderValue::from_static("/v2/"))
}

// ------------------------------------------------------------------ blobs

async fn get_blob(state: &AppState, t: &Target, digest: &str, head: bool) -> RegResult<Response> {
    if !valid_digest(digest) {
        return Err(RegError::new(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "invalid digest"));
    }
    let repo = repository(state, t).await?.ok_or_else(RegError::blob_unknown)?;
    let size = rdb::linked_blob(&state.db, repo.id, digest).await?.ok_or_else(RegError::blob_unknown)?;
    let body = if head {
        Body::empty()
    } else {
        let Some((_, stream)) = state.blobs.get(&blob_key(digest)).await? else { return Err(RegError::blob_unknown()) };
        Body::from_stream(stream)
    };
    let mut r = Response::new(body);
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    h.insert("Docker-Content-Digest", HeaderValue::from_str(digest).expect("hex"));
    h.insert(header::ETAG, HeaderValue::from_str(&format!("\"{digest}\"")).expect("hex"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=31536000"));
    Ok(r)
}

async fn delete_blob(state: &AppState, t: &Target, digest: &str) -> RegResult<Response> {
    let repo = repository(state, t).await?.ok_or_else(RegError::blob_unknown)?;
    if !rdb::unlink_blob(&state.db, repo.id, digest).await? {
        return Err(RegError::blob_unknown());
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

#[derive(Deserialize, Default)]
struct UploadQuery {
    digest: Option<String>,
    mount: Option<String>,
    from: Option<String>,
}

fn upload_response(t: &Target, id: uuid::Uuid, size: i64) -> Response {
    let mut r = StatusCode::ACCEPTED.into_response();
    let h = r.headers_mut();
    h.insert(header::LOCATION, loc(t, &format!("blobs/uploads/{id}")));
    h.insert(header::RANGE, HeaderValue::from_str(&format!("0-{}", (size - 1).max(0))).expect("ascii"));
    h.insert("Docker-Upload-UUID", HeaderValue::from_str(&id.to_string()).expect("ascii"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    r
}

fn created_blob(t: &Target, digest: &str) -> Response {
    let mut r = StatusCode::CREATED.into_response();
    r.headers_mut().insert(header::LOCATION, loc(t, &format!("blobs/{digest}")));
    r.headers_mut().insert("Docker-Content-Digest", HeaderValue::from_str(digest).expect("hex"));
    r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    r
}

/// `POST .../blobs/uploads/`: a new upload, a cross-repository mount, or a
/// whole blob in one request (`?digest=` with the body).
async fn start_upload(state: &AppState, t: &Target, uri: &Uri, headers: &HeaderMap, body: Body) -> RegResult<Response> {
    let q: UploadQuery = Query::try_from_uri(uri).map(|q| q.0).unwrap_or_default();
    // storage quota: nothing new once full, mounts included
    if let Some(r) = crate::services::quotas::room(state, t.project.id).await?.filter(|r| r.full()) {
        return Err(RegError::new(StatusCode::FORBIDDEN, "DENIED", r.message()));
    }
    let repo = rdb::ensure_repository(&state.db, t.project.id, &t.image).await?;
    if let (Some(digest), Some(from)) = (&q.mount, &q.from)
        && valid_digest(digest)
        && let Ok(source) = authorize(state, headers, from, "pull").await
        && let Some(frepo) = rdb::repository(&state.db, source.project.id, &source.image).await?
        && rdb::linked_blob(&state.db, frepo.id, digest).await?.is_some()
    {
        rdb::link_blob(&state.db, repo.id, digest).await?;
        return Ok(created_blob(t, digest));
    }
    let id = uuid::Uuid::new_v4();
    rdb::create_upload(&state.db, id, repo.id).await?;
    if let Some(digest) = q.digest {
        let upload = rdb::upload(&state.db, id, repo.id).await?.ok_or_else(RegError::upload_unknown)?;
        append(state, t.project.id, &upload, body).await?;
        return complete(state, t, id, repo.id, &digest).await;
    }
    Ok(upload_response(t, id, 0))
}

fn upload_id(id: &str) -> RegResult<uuid::Uuid> {
    uuid::Uuid::parse_str(id).map_err(|_| RegError::upload_unknown())
}

async fn upload_of(state: &AppState, t: &Target, id: &str) -> RegResult<rdb::Upload> {
    let repo = repository(state, t).await?.ok_or_else(RegError::upload_unknown)?;
    rdb::upload(&state.db, upload_id(id)?, repo.id).await?.ok_or_else(RegError::upload_unknown)
}

async fn upload_status(state: &AppState, t: &Target, id: &str) -> RegResult<Response> {
    let u = upload_of(state, t, id).await?;
    let mut r = upload_response(t, u.id, u.size);
    *r.status_mut() = StatusCode::NO_CONTENT;
    Ok(r)
}

/// Stream a request body into the upload's next part, hashing it.
async fn append(state: &AppState, project_id: i64, u: &rdb::Upload, body: Body) -> RegResult<i64> {
    let blob_limit = state.cfg.registry.max_blob_bytes;
    // what the quota leaves (the upload's earlier parts are not counted yet)
    let room = crate::services::quotas::room(state, project_id).await?;
    let limit = room.as_ref().map_or(blob_limit, |r| blob_limit.min(u.size.max(0) as u64 + r.left));
    let already = u.size.max(0) as u64;
    let (stream, hashed) = crate::blob::hashing(
        body.into_data_stream().map_err(std::io::Error::other).boxed(),
        Some(limit.saturating_sub(already)),
        "blob too large",
    );
    let key = part_key(u.id, u.parts);
    let written = match state.blobs.put_stream(&key, stream).await {
        Ok(n) => n as i64,
        Err(e) => {
            let _ = state.blobs.delete(&key).await;
            if already + hashed.bytes() > limit {
                if let Some(r) = room.filter(|_| limit < blob_limit) {
                    return Err(RegError::new(StatusCode::FORBIDDEN, "DENIED", r.message()));
                }
                return Err(RegError::new(StatusCode::PAYLOAD_TOO_LARGE, "SIZE_INVALID", format!("a blob can be at most {limit} bytes")));
            }
            return Err(e.into());
        }
    };
    if written == 0 {
        let _ = state.blobs.delete(&key).await;
        return Ok(u.size);
    }
    let digest = format!("sha256:{}", hashed.finish().0);
    if !rdb::add_upload_part(&state.db, u.id, u.size, written, &digest).await? {
        let _ = state.blobs.delete(&key).await;
        return Err(RegError::new(StatusCode::RANGE_NOT_SATISFIABLE, "BLOB_UPLOAD_INVALID", "the upload changed meanwhile"));
    }
    Ok(u.size + written)
}

/// `PATCH`: the next chunk (`Content-Range: <start>-<end>` must start at the upload's size).
async fn patch_upload(state: &AppState, t: &Target, id: &str, headers: &HeaderMap, body: Body) -> RegResult<Response> {
    let u = upload_of(state, t, id).await?;
    if let Some(range) = headers.get(header::CONTENT_RANGE).and_then(|v| v.to_str().ok()) {
        let start = range.trim_start_matches("bytes ").split('-').next().and_then(|s| s.parse::<i64>().ok());
        if start != Some(u.size) {
            let mut r = upload_response(t, u.id, u.size);
            *r.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            return Ok(r);
        }
    }
    let size = append(state, t.project.id, &u, body).await?;
    Ok(upload_response(t, u.id, size))
}

/// `PUT ...?digest=`: the last chunk (if any), then the blob is checked and stored.
async fn finish_upload(state: &AppState, t: &Target, id: &str, uri: &Uri, body: Body) -> RegResult<Response> {
    let q: UploadQuery = Query::try_from_uri(uri).map(|q| q.0).unwrap_or_default();
    let digest = q.digest.ok_or_else(|| RegError::new(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "digest missing"))?;
    let u = upload_of(state, t, id).await?;
    append(state, t.project.id, &u, body).await?;
    complete(state, t, u.id, u.repository_id, &digest).await
}

async fn remove_parts(state: &AppState, id: uuid::Uuid, parts: i32) {
    for n in 0..parts {
        let _ = state.blobs.delete(&part_key(id, n)).await;
    }
}

/// Check the parts against `digest`, put the blob in place, link it.
async fn complete(state: &AppState, t: &Target, id: uuid::Uuid, repository_id: i64, digest: &str) -> RegResult<Response> {
    let u = rdb::upload(&state.db, id, repository_id).await?.ok_or_else(RegError::upload_unknown)?;
    let bad = |state: &AppState| {
        let state = state.clone();
        async move {
            remove_parts(&state, u.id, u.parts).await;
            let _ = rdb::delete_upload(&state.db, u.id).await;
            RegError::new(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "provided digest did not match uploaded content")
        }
    };
    if !valid_digest(digest) {
        return Err(bad(state).await);
    }
    let key = blob_key(digest);
    let exists = rdb::blob_exists(&state.db, digest).await?.is_some() && state.blobs.size(&key).await?.is_some();
    if u.parts == 1 && u.single_digest.as_deref() == Some(digest) {
        if exists {
            let _ = state.blobs.delete(&part_key(u.id, 0)).await;
        } else {
            state.blobs.rename(&part_key(u.id, 0), &key).await?;
        }
    } else {
        // several parts (or none): join them, hashing again
        let joined = format!("registry/uploads/{}/joined", u.id);
        let blobs = state.blobs.clone();
        let keys: Vec<String> = (0..u.parts).map(|n| part_key(u.id, n)).collect();
        let stream = futures::stream::iter(keys)
            .then(move |k| {
                let blobs = blobs.clone();
                async move {
                    match blobs.get(&k).await {
                        Ok(Some((_, s))) => Ok(s),
                        Ok(None) => Err(std::io::Error::other("a part is missing")),
                        Err(e) => Err(std::io::Error::other(e)),
                    }
                }
            })
            .try_flatten()
            .boxed();
        let (stream, hashed) = crate::blob::hashing(stream, None, "");
        state.blobs.put_stream(&joined, stream).await?;
        let got = format!("sha256:{}", hashed.finish().0);
        if got != digest {
            let _ = state.blobs.delete(&joined).await;
            return Err(bad(state).await);
        }
        if exists {
            let _ = state.blobs.delete(&joined).await;
        } else {
            state.blobs.rename(&joined, &key).await?;
        }
        remove_parts(state, u.id, u.parts).await;
    }
    let mut tx = state.db.begin().await?;
    rdb::add_blob(&mut tx, repository_id, digest, u.size).await?;
    rdb::delete_upload(&mut *tx, u.id).await?;
    tx.commit().await?;
    Ok(created_blob(t, digest))
}

async fn cancel_upload(state: &AppState, t: &Target, id: &str) -> RegResult<Response> {
    let u = upload_of(state, t, id).await?;
    remove_parts(state, u.id, u.parts).await;
    rdb::delete_upload(&state.db, u.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ------------------------------------------------------------------ manifests

const MANIFEST_TYPES: &[&str] = &[
    "application/vnd.docker.distribution.manifest.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.index.v1+json",
];

fn is_index(media_type: &str) -> bool {
    media_type.ends_with("manifest.list.v2+json") || media_type.ends_with("image.index.v1+json")
}

fn valid_tag(tag: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9_][A-Za-z0-9._-]{0,127}$").unwrap()).is_match(tag)
}

fn manifest_response(m: &rdb::Manifest, head: bool) -> Response {
    let len = m.payload.len();
    let mut r = Response::new(if head { Body::empty() } else { Body::from(m.payload.clone()) });
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(&m.media_type).unwrap_or(HeaderValue::from_static("application/json")));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    h.insert("Docker-Content-Digest", HeaderValue::from_str(&m.digest).expect("hex"));
    h.insert(header::ETAG, HeaderValue::from_str(&format!("\"{}\"", m.digest)).expect("hex"));
    r
}

async fn find_manifest(state: &AppState, t: &Target, reference: &str) -> RegResult<rdb::Manifest> {
    let repo = repository(state, t).await?.ok_or_else(RegError::manifest_unknown)?;
    let m = if reference.starts_with("sha256:") {
        rdb::manifest(&state.db, repo.id, reference).await?
    } else {
        rdb::manifest_by_tag(&state.db, repo.id, reference).await?
    };
    m.ok_or_else(RegError::manifest_unknown)
}

async fn get_manifest(state: &AppState, t: &Target, reference: &str, head: bool) -> RegResult<Response> {
    Ok(manifest_response(&find_manifest(state, t, reference).await?, head))
}

async fn put_manifest(state: &AppState, t: &Target, reference: &str, headers: &HeaderMap, body: Body) -> RegResult<Response> {
    let content_type =
        headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(|s| s.split(';').next().unwrap_or("").trim().to_string());
    let body = axum::body::to_bytes(body, MAX_MANIFEST)
        .await
        .map_err(|_| RegError::new(StatusCode::PAYLOAD_TOO_LARGE, "MANIFEST_INVALID", "manifest too large"))?;
    let invalid = |m: &str| RegError::new(StatusCode::BAD_REQUEST, "MANIFEST_INVALID", m.to_string());
    let doc: Value = serde_json::from_slice(&body).map_err(|_| invalid("manifest is not JSON"))?;
    let media_type = doc["mediaType"].as_str().map(str::to_string).or(content_type).unwrap_or_default();
    if !MANIFEST_TYPES.contains(&media_type.as_str()) {
        return Err(invalid(&format!("unsupported manifest type `{media_type}`")));
    }
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&body)));
    let is_tag = !reference.starts_with("sha256:");
    if is_tag && !valid_tag(reference) {
        return Err(RegError::new(StatusCode::BAD_REQUEST, "TAG_INVALID", "invalid tag"));
    }
    if !is_tag && reference != digest {
        return Err(RegError::new(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "the manifest's digest does not match the reference"));
    }
    let repo = rdb::ensure_repository(&state.db, t.project.id, &t.image).await?;
    let digests_of = |v: &Value| -> Vec<String> {
        v.as_array().map(|a| a.iter().filter_map(|x| x["digest"].as_str().map(str::to_string)).collect()).unwrap_or_default()
    };
    let (blob_digests, manifest_digests, total_size) = if is_index(&media_type) {
        let children = digests_of(&doc["manifests"]);
        let known = rdb::manifests_of(&state.db, repo.id, &children).await?;
        if let Some(missing) = children.iter().find(|c| !known.iter().any(|(d, _)| d == *c)) {
            return Err(RegError::new(
                StatusCode::BAD_REQUEST,
                "MANIFEST_UNKNOWN",
                format!("manifest {missing} is not in this repository"),
            ));
        }
        let size = body.len() as i64 + known.iter().map(|(_, s)| s).sum::<i64>();
        (Vec::new(), children, size)
    } else {
        let mut blobs = Vec::new();
        if let Some(c) = doc["config"]["digest"].as_str() {
            blobs.push(c.to_string());
        }
        // foreign layers (with `urls`) are fetched from elsewhere by clients
        for l in doc["layers"].as_array().into_iter().flatten() {
            if l.get("urls").is_none()
                && let Some(d) = l["digest"].as_str()
            {
                blobs.push(d.to_string());
            }
        }
        blobs.sort();
        blobs.dedup();
        let linked = rdb::linked_of(&state.db, repo.id, &blobs).await?;
        if let Some(missing) = blobs.iter().find(|b| !linked.contains(b)) {
            return Err(RegError::new(
                StatusCode::BAD_REQUEST,
                "MANIFEST_BLOB_UNKNOWN",
                format!("blob {missing} is not in this repository"),
            ));
        }
        let size = body.len() as i64 + rdb::blob_sizes(&state.db, &blobs).await?;
        (blobs, Vec::new(), size)
    };
    let subject = doc["subject"]["digest"].as_str().map(str::to_string);
    let mut tx = state.db.begin().await?;
    let id = rdb::put_manifest(
        &mut *tx,
        rdb::NewManifest {
            repository_id: repo.id,
            digest: &digest,
            media_type: &media_type,
            payload: &body,
            blob_digests: &blob_digests,
            manifest_digests: &manifest_digests,
            total_size,
            subject: subject.as_deref(),
        },
    )
    .await?;
    if is_tag {
        rdb::set_tag(&mut *tx, repo.id, reference, id).await?;
    }
    tx.commit().await?;
    let mut r = StatusCode::CREATED.into_response();
    let h = r.headers_mut();
    h.insert(header::LOCATION, loc(t, &format!("manifests/{digest}")));
    h.insert("Docker-Content-Digest", HeaderValue::from_str(&digest).expect("hex"));
    if let Some(s) = subject
        && let Ok(v) = HeaderValue::from_str(&s)
    {
        h.insert("OCI-Subject", v);
    }
    Ok(r)
}

async fn delete_manifest(state: &AppState, t: &Target, reference: &str) -> RegResult<Response> {
    let repo = repository(state, t).await?.ok_or_else(RegError::manifest_unknown)?;
    let done = if reference.starts_with("sha256:") {
        rdb::delete_manifest(&state.db, repo.id, reference).await?
    } else {
        rdb::delete_tag(&state.db, repo.id, reference).await?
    };
    if !done {
        return Err(RegError::manifest_unknown());
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

// ------------------------------------------------------------------ tags, referrers, catalog

#[derive(Deserialize, Default)]
struct Page {
    n: Option<i64>,
    last: Option<String>,
}

async fn list_tags(state: &AppState, t: &Target, uri: &Uri) -> RegResult<Response> {
    let q: Page = Query::try_from_uri(uri).map(|q| q.0).unwrap_or_default();
    let n = q.n.unwrap_or(1000).clamp(1, 10_000);
    let repo = repository(state, t).await?.ok_or_else(RegError::name_unknown)?;
    let tags = rdb::tags(&state.db, repo.id, q.last.as_deref().unwrap_or(""), n + 1).await?;
    let more = tags.len() as i64 > n;
    let names: Vec<&str> = tags.iter().take(n as usize).map(|t| t.name.as_str()).collect();
    let mut r = axum::Json(json!({ "name": t.name, "tags": names })).into_response();
    if more && let Some(last) = names.last() {
        let link = format!("</v2/{}/tags/list?n={n}&last={last}>; rel=\"next\"", t.name);
        if let Ok(v) = HeaderValue::from_str(&link) {
            r.headers_mut().insert(header::LINK, v);
        }
    }
    Ok(r)
}

async fn referrers(state: &AppState, t: &Target, digest: &str) -> RegResult<Response> {
    let repo = repository(state, t).await?.ok_or_else(RegError::name_unknown)?;
    let list = rdb::referrers(&state.db, repo.id, digest).await?;
    let manifests: Vec<Value> = list
        .iter()
        .map(|m| {
            let doc: Value = serde_json::from_slice(&m.payload).unwrap_or_default();
            let mut d = json!({ "mediaType": m.media_type, "digest": m.digest, "size": m.payload.len() });
            if let Some(a) = doc.get("artifactType").or_else(|| doc["config"].get("mediaType")) {
                d["artifactType"] = a.clone();
            }
            if let Some(a) = doc.get("annotations") {
                d["annotations"] = a.clone();
            }
            d
        })
        .collect();
    let mut r = axum::Json(json!({ "schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json", "manifests": manifests }))
        .into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/vnd.oci.image.index.v1+json"));
    Ok(r)
}

/// `GET /v2/_catalog`: the repositories the token's owner can pull (a
/// `registry:catalog:*` token), in name order.
async fn catalog(state: &AppState, uri: &Uri, headers: &HeaderMap) -> RegResult<Response> {
    let Some(claims) = bearer(headers).and_then(|t| verify(state, t)) else {
        return Err(RegError {
            challenge: Some("registry:catalog:*".into()),
            ..RegError::new(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "authentication required")
        });
    };
    if !claims.access.iter().any(|a| a.kind == "registry" && a.name == "catalog") {
        return Err(RegError::new(StatusCode::FORBIDDEN, "DENIED", "requested access to the resource is denied"));
    }
    let q: Page = Query::try_from_uri(uri).map(|q| q.0).unwrap_or_default();
    let n = q.n.unwrap_or(100).clamp(1, 1000) as usize;
    let user = match claims.uid {
        Some(id) => forgust_db::users::find(&state.db, id).await?.filter(|u| u.is_active()),
        None => None,
    };
    let principal = user.map(|u| Principal::User { user: u, scopes: None }).unwrap_or(Principal::Anonymous);
    let mut names = Vec::new();
    let last = q.last.unwrap_or_default();
    for (repo, path) in rdb::all_repositories(&state.db, 10_000).await? {
        let name = if repo.name.is_empty() { path.to_ascii_lowercase() } else { format!("{}/{}", path.to_ascii_lowercase(), repo.name) };
        if name <= last {
            continue;
        }
        if let Some(p) = forgust_db::projects::find(&state.db, repo.project_id).await?
            && can_project(state, &principal, ProjectAction::ReadCode, &p).await?
        {
            names.push(name);
            if names.len() >= n {
                break;
            }
        }
    }
    Ok(axum::Json(json!({ "repositories": names })).into_response())
}

// ------------------------------------------------------------------ for the rest of Forgust

/// `host[:port]` of the registry (`CI_REGISTRY`).
pub fn host(state: &AppState) -> String {
    let u = url::Url::parse(&state.cfg.server.external_url).ok();
    match u.as_ref().and_then(|u| u.host_str().map(|h| (h.to_string(), u.port()))) {
        Some((h, Some(p))) => format!("{h}:{p}"),
        Some((h, None)) => h,
        None => "localhost".into(),
    }
}

/// `CI_REGISTRY_IMAGE` of a project.
pub fn image_of(state: &AppState, full_path: &str) -> String {
    format!("{}/{}", host(state), full_path.to_ascii_lowercase())
}

/// The registry's periodic work (with the CI sweep): stale uploads, cleanup
/// policies, then garbage collection.
pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    for (id, parts) in rdb::stale_uploads(&state.db).await? {
        remove_parts(state, id, parts).await;
        rdb::delete_upload(&state.db, id).await?;
    }
    for project_id in rdb::due_cleanups(&state.db).await? {
        if let Err(e) = cleanup(state, project_id).await {
            tracing::warn!(project_id, error = %e, "registry cleanup policy");
        }
    }
    let manifests = rdb::delete_dangling_manifests(&state.db).await?;
    let mut tx = state.db.begin().await?;
    let blobs = rdb::delete_unreferenced_blobs(&mut tx).await?;
    tx.commit().await?;
    if manifests + blobs > 0 {
        tracing::info!(manifests, blobs, "registry garbage collected");
    }
    Ok(())
}

/// Apply a project's cleanup policy, GitLab's way: per repository, take the
/// tags `name_regex` matches, minus `latest` and those `name_regex_keep`
/// matches; keep the newest `keep_n` of them; remove the rest that are older
/// than `older_than_days`.
pub async fn cleanup(state: &AppState, project_id: i64) -> anyhow::Result<u64> {
    let policy = rdb::cleanup_policy(&state.db, project_id).await?;
    let anchored = |r: &str| regex::Regex::new(&format!("^(?:{r})$")).ok();
    let (Some(remove), keep) =
        (anchored(&policy.name_regex), (!policy.name_regex_keep.is_empty()).then(|| anchored(&policy.name_regex_keep)).flatten())
    else {
        return Ok(0);
    };
    let cutoff = policy.older_than_days.map(|d| chrono::Utc::now() - chrono::Duration::days(d as i64));
    let mut removed = 0;
    for repo in rdb::repositories(&state.db, project_id).await? {
        let mut tags: Vec<Tag> = rdb::tags(&state.db, repo.id, "", 100_000)
            .await?
            .into_iter()
            .filter(|t| t.name != "latest" && remove.is_match(&t.name) && !keep.as_ref().is_some_and(|k| k.is_match(&t.name)))
            .collect();
        tags.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
        let keep_n = policy.keep_n.unwrap_or(0).max(0) as usize;
        for tag in tags.iter().skip(keep_n) {
            if cutoff.is_some_and(|c| tag.updated_at > c) {
                continue;
            }
            if rdb::delete_tag(&state.db, repo.id, &tag.name).await? {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_routes() {
        assert!(valid_name("acme/platform/api"));
        assert!(valid_name("acme/api/web-app/v1.2"));
        assert!(!valid_name("Acme/api"));
        assert!(!valid_name("acme//api"));
        assert!(!valid_name("acme/-api"));
        assert!(matches!(parse("a/b/manifests/latest"), Some((n, Route::Manifest(r))) if n == "a/b" && r == "latest"));
        assert!(matches!(parse("a/b/blobs/uploads/"), Some((n, Route::UploadStart)) if n == "a/b"));
        assert!(matches!(parse("a/blobs/uploads/x-y"), Some((n, Route::Upload(i))) if n == "a" && i == "x-y"));
        assert!(matches!(parse("a/b/c/blobs/sha256:00"), Some((n, Route::Blob(d))) if n == "a/b/c" && d == "sha256:00"));
        assert!(matches!(parse("a/b/tags/list"), Some((n, Route::Tags)) if n == "a/b"));
        assert!(valid_digest(&format!("sha256:{}", "a".repeat(64))));
        assert!(!valid_digest(&format!("sha256:{}", "A".repeat(64))));
        assert!(valid_tag("v1.2.3-rc_1") && !valid_tag(".hidden") && !valid_tag(&"x".repeat(129)));
    }
}
