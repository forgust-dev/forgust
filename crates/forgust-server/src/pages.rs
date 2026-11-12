//! Pages: static sites from CI. A passing `pages` job's `publish`
//! folder (in its artifacts archive) becomes the project's site, served on
//! `<top-level namespace>.<pages.domain>` (a wildcard DNS record pointing
//! here): `acme.pages.example.com/shop/` for `acme/shop`, and
//! `acme.pages.example.com/` for a project named `acme.pages.example.com`.
//!
//! Sites run visitors' scripts: they live on their own origin (the config
//! refuses a Pages domain holding Forgust's host), get no cookies and are only
//! served for public projects.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use forgust_db::ci::Build;

use crate::ci::archive::{self, Entry};
use crate::state::AppState;

/// Requests for a Pages host are answered here; the rest goes on.
pub async fn by_host(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let domain = &state.cfg.pages.domain;
    if domain.is_empty() {
        return next.run(req).await;
    }
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let host = host.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map_or(host, |(h, _)| h).to_ascii_lowercase();
    let Some(namespace) = host.strip_suffix(&format!(".{domain}")).filter(|n| !n.is_empty() && !n.contains('.')) else {
        return next.run(req).await;
    };
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "405 Method Not Allowed");
    }
    match serve(&state, namespace, &host, req.uri().path()).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(host, error = %e, "pages");
            plain(StatusCode::BAD_GATEWAY, "502 This site cannot be served right now")
        }
    }
}

fn plain(status: StatusCode, text: &'static str) -> Response {
    let mut r = (status, text).into_response();
    site_headers(&mut r);
    r
}

/// A site's own headers: no Forgust cookie or CSP applies here.
fn site_headers(r: &mut Response) {
    let h = r.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.remove(header::SET_COOKIE);
}

/// The project a path names under a namespace host, with what remains of the
/// path: the longest project path wins (`/docs/api/x` is `ns/docs/api` if it
/// exists, else `ns/docs`), then the namespace's root site.
async fn site_of(state: &AppState, namespace: &str, host: &str, path: &str) -> anyhow::Result<Option<(i64, String, bool)>> {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    // every candidate at once; the longest that is a project wins
    let candidates: Vec<(usize, String)> = (1..=segments.len().min(20))
        .filter(|&take| !segments[..take].iter().any(|s| s.is_empty()))
        .map(|take| (take, format!("{namespace}/{}", segments[..take].join("/"))))
        .collect();
    let paths: Vec<String> = candidates.iter().map(|(_, c)| c.clone()).collect();
    if let Some((id, found)) = forgust_db::pages::longest_project_of(&state.db, &paths).await?
        && let Some((take, _)) = candidates.iter().find(|(_, c)| c.eq_ignore_ascii_case(&found))
    {
        let rest = segments[*take..].join("/");
        // `/project` without its slash: relative links need it
        let bare = *take == segments.len();
        return Ok(Some((id, rest, bare && !path.ends_with('/'))));
    }
    let root = format!("{namespace}/{host}");
    Ok(forgust_db::pages::project_by_path(&state.db, &root).await?.map(|id| (id, path.trim_start_matches('/').to_string(), false)))
}

async fn serve(state: &AppState, namespace: &str, host: &str, path: &str) -> anyhow::Result<Response> {
    let not_found = || plain(StatusCode::NOT_FOUND, "404 Not Found");
    let Some((project_id, rest, needs_slash)) = site_of(state, namespace, host, path).await? else { return Ok(not_found()) };
    let Some(p) = forgust_db::projects::find(&state.db, project_id).await? else { return Ok(not_found()) };
    // public projects only: a private site would need its own sign-in
    if p.visibility != forgust_db::visibility::PUBLIC {
        return Ok(not_found());
    }
    let Some(d) = forgust_db::pages::deployment(&state.db, p.id).await? else { return Ok(not_found()) };
    if needs_slash {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::MOVED_PERMANENTLY;
        r.headers_mut().insert(header::LOCATION, HeaderValue::from_str(&format!("{path}/"))?);
        site_headers(&mut r);
        return Ok(r);
    }
    let entries = entries_of(state, &d.blob_key).await?;
    let rest = match percent_decode(&rest) {
        Some(r) if !r.split('/').any(|s| s == ".." || s == ".") => r,
        _ => return Ok(not_found()),
    };
    let base = format!("{}/", d.publish);
    let find = |name: &str| entries.get(name);
    let file = if rest.is_empty() || rest.ends_with('/') {
        find(&format!("{base}{rest}index.html"))
    } else if let Some(e) = find(&format!("{base}{rest}")) {
        Some(e)
    } else if find(&format!("{base}{rest}/index.html")).is_some() {
        // a folder: on to it with its slash
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::MOVED_PERMANENTLY;
        r.headers_mut().insert(header::LOCATION, HeaderValue::from_str(&format!("{path}/"))?);
        site_headers(&mut r);
        return Ok(r);
    } else {
        find(&format!("{base}{rest}.html"))
    };
    let (status, e) = match file {
        Some(e) => (StatusCode::OK, e),
        // the site's own 404 page, when it has one
        None => match find(&format!("{base}404.html")) {
            Some(e) => (StatusCode::NOT_FOUND, e),
            None => return Ok(not_found()),
        },
    };
    let stream = archive::open(state, &d.blob_key, e).await?;
    let mime = mime_guess::from_path(&e.name).first_or_octet_stream();
    let mut r = Response::new(Body::from_stream(stream));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.essence_str()).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(e.size));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=60"));
    site_headers(&mut r);
    Ok(r)
}

/// `%20` and friends in a path; None for a bad escape or a NUL.
fn percent_decode(s: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(s).decode_utf8().ok()?;
    (!decoded.contains('\0')).then(|| decoded.into_owned())
}

/// A deployment's archive listing, read once per archive.
/// A site's files by name (its archive's listing, read once an hour at most).
async fn entries_of(state: &AppState, key: &str) -> anyhow::Result<Arc<HashMap<String, Entry>>> {
    static CACHE: OnceLock<moka::future::Cache<String, Arc<HashMap<String, Entry>>>> = OnceLock::new();
    let cache =
        CACHE.get_or_init(|| moka::future::Cache::builder().max_capacity(200).time_to_live(std::time::Duration::from_secs(3600)).build());
    if let Some(e) = cache.get(key).await {
        return Ok(e);
    }
    let (all, _) = archive::entries(state, key).await?;
    let files = Arc::new(all.into_iter().filter(|e| !e.is_dir()).map(|e| (e.name.clone(), e)).collect::<HashMap<_, _>>());
    cache.insert(key.to_string(), files.clone()).await;
    Ok(files)
}

/// A passing Pages job: its `publish` folder becomes the project's site
/// (newer jobs only: a late older job does not take the site back).
pub async fn deploy(state: &AppState, b: &Build, publish: &str) -> anyhow::Result<()> {
    if !state.cfg.pages.enabled() {
        return Ok(());
    }
    // only the default branch publishes (as GitLab): a branch or a merge
    // request of anyone who can push must not replace the public site
    let pipeline = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await?.ok_or_else(|| anyhow::anyhow!("pipeline vanished"))?;
    let project = forgust_db::projects::find(&state.db, b.project_id).await?.ok_or_else(|| anyhow::anyhow!("project vanished"))?;
    if pipeline.tag || pipeline.merge_request_id.is_some() || project.default_branch.as_deref() != Some(pipeline.git_ref.as_str()) {
        anyhow::bail!("Pages are published from the default branch only (this ran on `{}`)", pipeline.git_ref);
    }
    let Some(a) = forgust_db::ci::artifact(&state.db, b.id, "archive").await?.filter(|a| a.file_format == "zip") else {
        anyhow::bail!("the Pages job left no artifacts archive (`artifacts:paths` must hold `{publish}`)");
    };
    let entries = entries_of(state, &a.blob_key).await?;
    if !entries.keys().any(|name| name.starts_with(&format!("{publish}/"))) {
        anyhow::bail!("the Pages job's artifacts have no `{publish}/` folder");
    }
    let mut tx = state.db.begin().await?;
    forgust_db::pages::deploy(&mut tx, b.project_id, b.id, a.id, publish).await?;
    tx.commit().await?;
    Ok(())
}
