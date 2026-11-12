//! Git smart HTTP (protocol v0/v1/v2) and the URL parsing shared with LFS.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use forgust_db::projects::Project;
use forgust_storage::{GitService, ServiceOptions};
use futures::TryStreamExt;
use tokio::io::AsyncWriteExt;
use tokio_util::io::{ReaderStream, StreamReader};

use crate::auth::authz::{ProjectAction, can_project};
use crate::auth::{ClientIp, Principal, account_open, login_allowed, login_checked, principal_from_token};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, PartialEq, Eq)]
pub enum GitRoute {
    InfoRefs,
    UploadPack,
    ReceivePack,
    Lfs(String),
}

/// Split `group/sub/project.git/info/refs` into (`group/sub/project`, route).
pub fn parse_git_path(path: &str) -> Option<(String, GitRoute)> {
    let path = path.trim_start_matches('/');
    let (repo, route) = if let Some(r) = path.strip_suffix("/info/refs") {
        (r, GitRoute::InfoRefs)
    } else if let Some(r) = path.strip_suffix("/git-upload-pack") {
        (r, GitRoute::UploadPack)
    } else if let Some(r) = path.strip_suffix("/git-receive-pack") {
        (r, GitRoute::ReceivePack)
    } else {
        let i = path.find("/info/lfs/")?;
        (&path[..i], GitRoute::Lfs(path[i + "/info/lfs/".len()..].to_string()))
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if repo.is_empty() || !repo.contains('/') || repo.contains("/-/") {
        return None;
    }
    Some((repo.to_string(), route))
}

/// (project, current full path when the request used an old path).
pub async fn find_project(state: &AppState, full_path: &str) -> ApiResult<Option<(Project, Option<String>)>> {
    let mut conn = state.db.acquire().await?;
    let Some(hit) = forgust_db::routes::lookup(&mut conn, full_path).await? else { return Ok(None) };
    if hit.source_type != "project" {
        return Ok(None);
    }
    let Some(p) = forgust_db::projects::find(&mut *conn, hit.source_id).await? else { return Ok(None) };
    Ok(Some((p, hit.redirected.then_some(hit.path))))
}

/// Principal from HTTP Basic credentials, as git and git-lfs send them.
/// Passwords are rate limited like the web login.
pub async fn basic_principal(state: &AppState, headers: &HeaderMap, ip: ClientIp) -> ApiResult<Principal> {
    let Some(raw) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return Ok(Principal::Anonymous) };
    if let Some(bearer) = raw.strip_prefix("Bearer ") {
        let bearer = bearer.trim();
        // job tokens over git HTTP are a switch, whichever way they are sent
        if bearer.starts_with(crypto::prefix::JOB) && !state.cfg.features.ci_job_token_git_http {
            return Err(ApiError::Unauthorized);
        }
        return principal_from_token(state, bearer).await?.ok_or(ApiError::Unauthorized);
    }
    let Some(b64) = raw.strip_prefix("Basic ") else { return Err(ApiError::Unauthorized) };
    let decoded = STANDARD.decode(b64.trim()).map_err(|_| ApiError::Unauthorized)?;
    let decoded = String::from_utf8(decoded).map_err(|_| ApiError::Unauthorized)?;
    let (username, password) = decoded.split_once(':').ok_or(ApiError::Unauthorized)?;

    if username == "gitlab-ci-token" {
        if !state.cfg.features.ci_job_token_git_http || !password.starts_with(crypto::prefix::JOB) {
            return Err(ApiError::Unauthorized);
        }
        return principal_from_token(state, password).await?.ok_or(ApiError::Unauthorized);
    }
    if password.starts_with(crypto::prefix::PERSONAL) || password.starts_with(crypto::prefix::DEPLOY) {
        let p = principal_from_token(state, password).await?.ok_or(ApiError::Unauthorized)?;
        if let Principal::DeployToken { token } = &p
            && token.username != username
        {
            return Err(ApiError::Unauthorized);
        }
        return Ok(p);
    }
    // username + password; refused once 2FA is on (use a token instead)
    if !login_allowed(state, ip, username) {
        return Err(ApiError::Unauthorized);
    }
    let Some((id, Some(hash))) = forgust_db::users::credentials(&state.db, username).await? else {
        crypto::check_password(password, None).await;
        login_checked(state, ip, username, None, false);
        return Err(ApiError::Unauthorized);
    };
    if !account_open(state, ip, id).await? {
        return Err(ApiError::Unauthorized);
    }
    let ok = crypto::check_password(password, Some(hash)).await;
    login_checked(state, ip, username, Some(id), ok);
    if !ok {
        return Err(ApiError::Unauthorized);
    }
    let user = forgust_db::users::find(&state.db, id).await?.ok_or(ApiError::Unauthorized)?;
    if !user.is_active() || user.totp_enabled {
        return Err(ApiError::Unauthorized);
    }
    Ok(Principal::User { user, scopes: None })
}

pub fn unauthorized() -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, "HTTP Basic: Access denied\n").into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"Forgust\""));
    r
}

fn no_cache(r: &mut Response) {
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, max-age=0, must-revalidate"));
    r.headers_mut().insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

fn pkt_line(s: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", s.len() + 4).into_bytes();
    v.extend_from_slice(s.as_bytes());
    v
}

/// Fallback entry point for git URLs.
pub async fn handle(state: AppState, full_path: String, route: GitRoute, req: Request) -> Response {
    match handle_inner(state, full_path, route, req).await {
        Ok(r) => r,
        Err(ApiError::Unauthorized) => unauthorized(),
        Err(ApiError::NotFound) => (StatusCode::NOT_FOUND, "Repository not found\n").into_response(),
        Err(ApiError::Forbidden(m)) => (StatusCode::FORBIDDEN, format!("{m}\n")).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn handle_inner(state: AppState, full_path: String, route: GitRoute, req: Request) -> ApiResult<Response> {
    // nothing about a path (that it exists, or where it moved) before the caller
    // may read it: a missing project answers as a private one would
    let ip = ClientIp::of(&state, req.headers(), req.extensions());
    let Some((project, moved)) = find_project(&state, &full_path).await? else {
        if crate::lfs::lfs_ssh_token(req.headers()).is_none() {
            // bad credentials: 401, as for a private project
            if let Principal::Anonymous = basic_principal(&state, req.headers(), ip).await? {
                return Err(ApiError::Unauthorized);
            }
        }
        return Err(ApiError::NotFound);
    };
    if let Some(new_path) = moved {
        let crate::lfs::Access { read, signed_in, .. } = crate::lfs::access(&state, &project, req.headers(), ip).await?;
        if !read {
            return Err(if signed_in { ApiError::NotFound } else { ApiError::Unauthorized });
        }
        // old path after a rename/transfer: git follows the redirect on info/refs
        let suffix = req.uri().path().trim_start_matches('/').strip_prefix(&full_path).unwrap_or("").to_string();
        let suffix = suffix.strip_prefix(".git").unwrap_or(&suffix);
        let q = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
        let location = format!("/{new_path}.git{suffix}{q}");
        return Ok((StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, location)]).into_response());
    }

    if let GitRoute::Lfs(rest) = route {
        return crate::lfs::handle(state, project, rest, req).await;
    }

    let principal = basic_principal(&state, req.headers(), ip).await?;
    let service = match &route {
        GitRoute::InfoRefs => {
            let svc = req.uri().query().unwrap_or("").split('&').find_map(|kv| kv.strip_prefix("service=")).unwrap_or("");
            GitService::from_name(svc)
                .filter(|s| *s != GitService::UploadArchive)
                .ok_or_else(|| ApiError::forbidden("dumb HTTP is not supported"))?
        }
        GitRoute::UploadPack => GitService::UploadPack,
        GitRoute::ReceivePack => GitService::ReceivePack,
        GitRoute::Lfs(_) => unreachable!(),
    };
    let action = if service == GitService::ReceivePack { ProjectAction::Push } else { ProjectAction::ReadCode };
    if !can_project(&state, &principal, action, &project).await? {
        if matches!(principal, Principal::Anonymous) {
            return Err(ApiError::Unauthorized);
        }
        if can_project(&state, &principal, ProjectAction::ReadCode, &project).await? {
            return Err(ApiError::forbidden(if project.archived() {
                crate::auth::authz::ARCHIVED
            } else {
                "You are not allowed to push code to this project."
            }));
        }
        return Err(ApiError::NotFound);
    }

    let git_protocol = req.headers().get("git-protocol").and_then(|v| v.to_str().ok()).filter(|v| v.len() < 256).map(str::to_string);
    let is_v2 = git_protocol.as_deref().is_some_and(|p| p.contains("version=2"));
    let mut opts = ServiceOptions { stateless_rpc: true, git_protocol, ..Default::default() };
    if service == GitService::ReceivePack {
        opts.hooks_path = Some(state.hooks.hooks_dir.clone());
        opts.env = crate::hooks::hook_env(&state, project.id, &principal, "http");
    }
    metrics::counter!("forgust_git_requests_total", "service" => service.name(), "transport" => "http").increment(1);

    if route == GitRoute::InfoRefs {
        opts.advertise_refs = true;
        let mut proc = state.storage.spawn_service(project.id, service, opts).await?;
        let mut out = Vec::new();
        if !is_v2 {
            out.extend(pkt_line(&format!("# service={}\n", service.name())));
            out.extend_from_slice(b"0000");
        }
        tokio::io::AsyncReadExt::read_to_end(&mut proc.stdout, &mut out).await.map_err(|e| ApiError::Internal(e.into()))?;
        let status = proc.wait().await.map_err(|e| ApiError::Internal(e.into()))?;
        if !status.success() {
            return Err(ApiError::Internal(anyhow::anyhow!("{} --advertise-refs failed: {status}", service.name())));
        }
        let mut r = Response::new(Body::from(out));
        r.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_str(&format!("application/x-{}-advertisement", service.name())).unwrap());
        no_cache(&mut r);
        return Ok(r);
    }

    if req.method() != Method::POST {
        return Err(ApiError::bad("expected POST"));
    }
    // pushes share the repo lock; maintenance takes it exclusively
    let lock = if service == GitService::ReceivePack { Some(state.repo_locks.get(project.id).read_owned().await) } else { None };
    let gzip = req.headers().get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    let body = req.into_body().into_data_stream().map_err(std::io::Error::other);
    let reader = StreamReader::new(body);
    let mut input: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>> =
        if gzip { Box::pin(async_compression::tokio::bufread::GzipDecoder::new(reader)) } else { Box::pin(reader) };

    let mut proc = state.storage.spawn_service(project.id, service, opts).await?;
    let mut stdin = proc.stdin.take().expect("stdin");
    tokio::spawn(async move {
        if let Err(e) = tokio::io::copy(&mut input, &mut stdin).await {
            tracing::debug!(error = %e, "git request body copy ended");
        }
        let _ = stdin.shutdown().await;
    });
    if let Some(mut err) = proc.stderr.take() {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut buf).await;
            if !buf.is_empty() {
                tracing::debug!(stderr = %String::from_utf8_lossy(&buf), "git service stderr");
            }
        });
    }
    let stdout = std::mem::replace(&mut proc.stdout, Box::pin(tokio::io::empty()));
    let stream = ReaderStream::with_capacity(stdout, 64 * 1024);
    tokio::spawn(async move {
        let status = proc.wait().await;
        drop(lock);
        if let Ok(s) = status
            && !s.success()
        {
            tracing::debug!(status = %s, "git service exited with failure");
        }
    });
    let mut r = Response::new(Body::from_stream(stream));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_str(&format!("application/x-{}-result", service.name())).unwrap());
    no_cache(&mut r);
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_urls() {
        assert_eq!(parse_git_path("/acme/api.git/info/refs"), Some(("acme/api".into(), GitRoute::InfoRefs)));
        assert_eq!(parse_git_path("/acme/sub/api/git-upload-pack"), Some(("acme/sub/api".into(), GitRoute::UploadPack)));
        assert_eq!(parse_git_path("/a/b.git/git-receive-pack"), Some(("a/b".into(), GitRoute::ReceivePack)));
        assert_eq!(parse_git_path("/a/b.git/info/lfs/objects/batch"), Some(("a/b".into(), GitRoute::Lfs("objects/batch".into()))));
        assert_eq!(parse_git_path("/single/info/refs"), None);
        assert_eq!(parse_git_path("/a/b/-/tree/main"), None);
        assert_eq!(pkt_line("# service=git-upload-pack\n"), b"001e# service=git-upload-pack\n");
    }
}
