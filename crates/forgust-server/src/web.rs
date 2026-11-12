//! Static web app and the non-API URLs the browser hits directly.

use axum::body::Body;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::ServeDir;

use crate::api::project_for;
use crate::auth::Viewer;
use crate::auth::authz::ProjectAction;
use crate::error::ApiError;
use crate::state::AppState;

/// Everything that is not `/api`, `/health` or `/metrics`.
pub async fn fallback(State(state): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if let Some((repo, route)) = crate::git_http::parse_git_path(&path) {
        return crate::git_http::handle(state, repo, route, req).await;
    }
    // project uploads: /-/project/<id>/uploads/<secret>/<name>
    if let Some(rest) = path.strip_prefix("/-/project/") {
        let parts: Vec<&str> = rest.splitn(4, '/').collect();
        if let [id, "uploads", secret, name] = parts[..] {
            let (id, secret, name) = (id.to_string(), secret.to_string(), percent_decode(name));
            return match upload(&state, req, &id, &secret, &name).await {
                Ok(r) => r,
                Err(e) => e.into_response(),
            };
        }
    }
    // GitLab's form of the same: /<project>/-/uploads/<secret>/<name>
    if let Some((project_path, rest)) = path.trim_start_matches('/').split_once("/-/uploads/")
        && let Some((secret, name)) = rest.split_once('/')
    {
        let (project_path, secret, name) = (project_path.to_string(), secret.to_string(), percent_decode(name));
        return match upload(&state, req, &project_path, &secret, &name).await {
            Ok(r) => r,
            Err(e) => e.into_response(),
        };
    }
    // release assets: /<project>/-/releases/<tag, encoded>/downloads/<path>
    if let Some((project_path, rest)) = path.trim_start_matches('/').split_once("/-/releases/")
        && let Some((tag, asset)) = rest.split_once("/downloads/")
    {
        let (project_path, tag, asset) = (project_path.to_string(), percent_decode(tag), percent_decode(asset));
        return match release_asset(&state, req, &project_path, &tag, &asset).await {
            Ok(r) => r,
            Err(e) => e.into_response(),
        };
    }
    // Markdown images from other sites: /-/image/<signature>/<base64url of the URL>
    if let Some(rest) = path.strip_prefix("/-/image/")
        && let Some((sig, encoded)) = rest.split_once('/')
    {
        let (sig, encoded) = (sig.to_string(), encoded.to_string());
        return proxied_image(&state, &sig, &encoded).await;
    }
    if let Some((project_path, rest)) = path.trim_start_matches('/').split_once("/-/raw/") {
        let (project_path, rest) = (project_path.to_string(), percent_decode(rest));
        return match raw(&state, req, &project_path, &rest).await {
            Ok(r) => r,
            Err(e) => e.into_response(),
        };
    }
    spa(state, req).await
}

fn percent_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

async fn raw(state: &AppState, req: Request, project_path: &str, rest: &str) -> Result<Response, ApiError> {
    let (mut parts, _) = req.into_parts();
    let Viewer(v) = Viewer::from_request_parts(&mut parts, state).await?;
    let p = project_for(state, &v, project_path, ProjectAction::ReadCode).await?;
    let resolved = crate::api::repo::split_ref(state, &p, rest).await?;
    crate::api::repo::raw_response(state, &p, &resolved.commit, &resolved.path).await
}

async fn upload(state: &AppState, req: Request, id: &str, secret: &str, name: &str) -> Result<Response, ApiError> {
    let (mut parts, _) = req.into_parts();
    let Viewer(v) = Viewer::from_request_parts(&mut parts, state).await?;
    let p = project_for(state, &v, id, ProjectAction::Read).await?;
    crate::services::uploads::serve(state, &v, &p, secret, name).await
}

async fn release_asset(state: &AppState, req: Request, project_path: &str, tag: &str, asset: &str) -> Result<Response, ApiError> {
    let (mut parts, _) = req.into_parts();
    let Viewer(v) = Viewer::from_request_parts(&mut parts, state).await?;
    let p = project_for(state, &v, project_path, ProjectAction::Read).await?;
    let wants_page = parts.headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"));
    let url = crate::services::releases::download_url(state, &v, &p, tag, asset).await?;
    let ours = url.starts_with('/') && !url.starts_with("//") || url.starts_with(&format!("{}/", state.cfg.server.external_url));
    // a link to another site: tools are sent on (as GitLab), a browser is told
    // where first (this host's address must not lend its name to any page)
    // links are http(s) or ftp when made; checked again where one is followed
    if !ours && !url::Url::parse(&url).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "ftp")) {
        return Err(ApiError::NotFound);
    }
    if !ours && wants_page {
        let esc = crate::render::escape_html(&url);
        let host = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        let body = format!(
            "<!doctype html><meta charset=utf-8><title>Download</title><p>This file of {} is hosted on <strong>{}</strong>:</p><p><a href=\"{esc}\" rel=\"noreferrer\">{esc}</a></p>",
            crate::render::escape_html(&p.full_path),
            crate::render::escape_html(&host),
        );
        return Ok(([(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"))], body).into_response());
    }
    let location = HeaderValue::from_str(&url).map_err(|e| ApiError::Internal(e.into()))?;
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}

/// Largest image the proxy passes on.
const MAX_PROXIED_IMAGE: usize = 10 << 20;

async fn proxied_image(state: &AppState, sig: &str, encoded: &str) -> Response {
    let Some(url) = state.images.as_ref().and_then(|px| px.verify(sig, encoded)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match crate::services::webhooks::fetch_image(state, &url, MAX_PROXIED_IMAGE).await {
        Ok((ctype, body)) => {
            let mut r = Response::new(Body::from(body));
            let h = r.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_str(&ctype).unwrap_or(HeaderValue::from_static("application/octet-stream")));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
            // an SVG opened on its own runs no script and loads nothing
            h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"));
            r
        }
        Err(e) => {
            tracing::debug!(%url, error = %e, "image proxy");
            (StatusCode::BAD_GATEWAY, [(header::CACHE_CONTROL, "public, max-age=300")]).into_response()
        }
    }
}

/// The web app's CSP: its own scripts (and the inline ones `index.html` carries,
/// by hash), its own API, images from here (other sites' come through the image
/// proxy, unless it is off), and no framing.
fn app_csp(html: &str, proxied_images: bool) -> String {
    use base64::Engine;
    use sha2::Digest;
    static INLINE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = INLINE.get_or_init(|| regex::Regex::new(r"(?s)<script([^>]*)>(.*?)</script>").unwrap());
    let mut scripts = String::from("'self'");
    for c in re.captures_iter(html) {
        if c[1].contains("src=") {
            continue;
        }
        let digest = sha2::Sha256::digest(c[2].as_bytes());
        scripts.push_str(&format!(" 'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(digest)));
    }
    let images = if proxied_images { "'self' data: blob:" } else { "'self' data: blob: https: http:" };
    format!(
        "default-src 'self'; script-src {scripts}; style-src 'self' 'unsafe-inline'; img-src {images}; font-src 'self' data:; \
         connect-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'"
    )
}

/// Serve built assets; any other path gets `index.html` (client-side routing).
async fn spa(state: AppState, req: Request) -> Response {
    let dir = &state.cfg.server.static_dir;
    let index = dir.join("index.html");
    if req.uri().path().starts_with("/assets/") || req.uri().path() == "/favicon.svg" {
        let mut resp = match ServeDir::new(dir).oneshot(req).await {
            Ok(r) => r.into_response(),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        };
        if resp.status().is_success() {
            resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
        }
        return resp;
    }
    match tokio::fs::read(&index).await {
        Ok(html) => {
            let csp = app_csp(&String::from_utf8_lossy(&html), state.images.is_some());
            let mut r = Response::new(Body::from(html));
            if let Ok(v) = HeaderValue::from_str(&csp) {
                r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
            }
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            r
        }
        Err(_) => (StatusCode::NOT_FOUND, "Forgust web UI is not built (server.static_dir). The API is at /api/v1.\n").into_response(),
    }
}
