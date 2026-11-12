//! Git LFS server: batch API, basic transfer, objects in the blob store (M2).

use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use forgust_db::projects::Project;
use futures::{StreamExt, TryStreamExt};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::authz::{ProjectAction, can_project};
use crate::auth::{ClientIp, Principal};
use crate::blob::lfs_key;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::git_http::basic_principal;
use crate::state::AppState;

const LFS_JSON: &str = "application/vnd.git-lfs+json";

#[derive(Deserialize)]
struct BatchRequest {
    operation: String,
    #[serde(default)]
    transfers: Vec<String>,
    objects: Vec<BatchObject>,
}

#[derive(Deserialize)]
struct BatchObject {
    oid: String,
    size: i64,
}

fn lfs_error(status: StatusCode, message: &str) -> Response {
    let mut r = (status, axum::Json(json!({ "message": message }))).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(LFS_JSON));
    if status == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"Forgust\""));
    }
    r
}

fn valid_oid(oid: &str) -> bool {
    oid.len() == 64 && oid.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// What a request may do, and who makes it (for lock ownership).
pub(crate) struct Access {
    pub read: bool,
    pub write: bool,
    pub signed_in: bool,
    who: Who,
}

enum Who {
    User(forgust_db::users::User),
    /// A user's `git-lfs-authenticate` token: looked up only when a lock asks.
    SshUser(i64),
    Nobody,
}

/// (can read, can write) for the request, credentials checked once. LFS also
/// accepts the short-lived tokens that `git-lfs-authenticate` hands out over SSH.
pub(crate) async fn access(state: &AppState, project: &Project, headers: &HeaderMap, ip: ClientIp) -> ApiResult<Access> {
    if let Some(token) = lfs_ssh_token(headers) {
        let t = forgust_db::tokens::find_lfs_ssh_token(&state.db, &crypto::token_hash(&token)).await?.ok_or(ApiError::Unauthorized)?;
        if t.project_id != project.id {
            return Err(ApiError::NotFound);
        }
        let who = if t.principal_kind == "user" { Who::SshUser(t.principal_id) } else { Who::Nobody };
        return Ok(Access { read: true, write: t.can_write && !project.archived(), signed_in: true, who });
    }
    let principal = basic_principal(state, headers, ip).await?;
    let read = can_project(state, &principal, ProjectAction::ReadCode, project).await?;
    let write = read && can_project(state, &principal, ProjectAction::Push, project).await?;
    let signed_in = !matches!(principal, Principal::Anonymous);
    let who = match principal {
        Principal::User { user, .. } | Principal::Job { user, .. } => Who::User(user),
        _ => Who::Nobody,
    };
    Ok(Access { read, write, signed_in, who })
}

pub(crate) fn lfs_ssh_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let b64 = raw.strip_prefix("Basic ")?;
    let decoded = String::from_utf8(STANDARD.decode(b64.trim()).ok()?).ok()?;
    let (_, password) = decoded.split_once(':')?;
    password.starts_with(crypto::prefix::LFS_SSH).then(|| password.to_string())
}

pub async fn handle(state: AppState, project: Project, rest: String, req: Request) -> ApiResult<Response> {
    let Access { read, write, signed_in, who } =
        match access(&state, &project, req.headers(), ClientIp::of(&state, req.headers(), req.extensions())).await {
            Ok(a) => a,
            Err(ApiError::Unauthorized) => return Ok(lfs_error(StatusCode::UNAUTHORIZED, "Credentials needed")),
            Err(ApiError::NotFound) => return Ok(lfs_error(StatusCode::NOT_FOUND, "Repository not found")),
            Err(e) => return Err(e),
        };
    if !read {
        return Ok(if signed_in {
            lfs_error(StatusCode::NOT_FOUND, "Repository not found")
        } else {
            lfs_error(StatusCode::UNAUTHORIZED, "Credentials needed")
        });
    }
    let method = req.method().clone();
    match (method, rest.as_str()) {
        (Method::POST, "objects/batch") => batch(state, project, write, req).await,
        (Method::GET, p) if p.starts_with("objects/") => download(state, project, &p["objects/".len()..]).await,
        (Method::PUT, p) if p.starts_with("objects/") => {
            if !write {
                return Ok(lfs_error(StatusCode::FORBIDDEN, "You cannot push to this repository"));
            }
            let oid = p["objects/".len()..].to_string();
            upload(state, project, oid, req).await
        }
        (method, p) if p == "locks" || p.starts_with("locks/") => {
            let user = match who {
                Who::User(u) => Some(u),
                Who::SshUser(id) => forgust_db::users::find(&state.db, id).await?,
                Who::Nobody => None,
            };
            locks(state, project, write, user, method, p.to_string(), req).await
        }
        _ => Ok(lfs_error(StatusCode::NOT_FOUND, "Not found")),
    }
}

// ------------------------------------------------------------------ locks

fn lock_json(l: &forgust_db::lfs_locks::LfsLock) -> Value {
    json!({
        "id": l.id.to_string(),
        "path": l.path,
        "locked_at": l.created_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "owner": { "name": if l.owner_name.is_empty() { &l.owner_username } else { &l.owner_name } },
    })
}

#[derive(Deserialize, Default)]
struct LockQuery {
    path: Option<String>,
    id: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

#[derive(Deserialize, Default)]
struct LockBody {
    #[serde(default)]
    path: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(rename = "ref", default)]
    git_ref: Option<Value>,
}

/// The Git LFS locking API: create, list, verify (ours / theirs before a
/// push) and unlock (someone else's lock: Maintainer, with `force`).
async fn locks(
    state: AppState,
    project: Project,
    write: bool,
    user: Option<forgust_db::users::User>,
    method: Method,
    path: String,
    req: Request,
) -> ApiResult<Response> {
    let query: LockQuery = axum::extract::Query::try_from_uri(req.uri()).map(|q| q.0).unwrap_or_default();
    let body: LockBody = if method == Method::POST {
        let bytes = axum::body::to_bytes(req.into_body(), 64 << 10).await.map_err(|_| ApiError::bad("request too large"))?;
        if bytes.is_empty() {
            LockBody::default()
        } else {
            serde_json::from_slice(&bytes).map_err(|e| ApiError::bad(format!("bad request: {e}")))?
        }
    } else {
        LockBody::default()
    };
    let page = |cursor: Option<&str>, limit: Option<i64>| {
        (cursor.and_then(|c| c.parse::<i64>().ok()).unwrap_or(0), limit.unwrap_or(100).clamp(1, 1000))
    };
    match (method, path.as_str()) {
        (Method::GET, "locks") => {
            let (after, limit) = page(query.cursor.as_deref(), query.limit);
            let id = query.id.as_deref().and_then(|i| i.parse::<i64>().ok());
            let list = forgust_db::lfs_locks::list(&state.db, project.id, query.path.as_deref(), id, after, limit + 1).await?;
            let next = (list.len() as i64 > limit).then(|| list[limit as usize - 1].id.to_string());
            let locks: Vec<Value> = list.iter().take(limit as usize).map(lock_json).collect();
            Ok(json_lfs(json!({ "locks": locks, "next_cursor": next })))
        }
        (Method::POST, "locks") => {
            let Some(u) = user.filter(|_| write) else { return Ok(lfs_error(StatusCode::FORBIDDEN, "You cannot push to this repository")) };
            let p = body.path.trim().trim_start_matches('/').to_string();
            if p.is_empty() || p.len() > 4096 || p.split('/').any(|s| s == ".." || s.is_empty()) {
                return Ok(lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid path"));
            }
            let ref_name = body.git_ref.as_ref().and_then(|r| r.get("name")).and_then(|n| n.as_str()).map(str::to_string);
            match forgust_db::lfs_locks::create(&state.db, project.id, &p, u.id, ref_name.as_deref()).await? {
                Some(id) => {
                    let lock = forgust_db::lfs_locks::find(&state.db, project.id, id).await?.ok_or(ApiError::NotFound)?;
                    let mut r = json_lfs(json!({ "lock": lock_json(&lock) }));
                    *r.status_mut() = StatusCode::CREATED;
                    Ok(r)
                }
                None => {
                    let existing = forgust_db::lfs_locks::list(&state.db, project.id, Some(&p), None, 0, 1).await?;
                    let mut r = json_lfs(json!({ "lock": existing.first().map(lock_json), "message": "already locked" }));
                    *r.status_mut() = StatusCode::CONFLICT;
                    Ok(r)
                }
            }
        }
        (Method::POST, "locks/verify") => {
            let Some(u) = user.filter(|_| write) else { return Ok(lfs_error(StatusCode::FORBIDDEN, "You cannot push to this repository")) };
            let (after, limit) = page(body.cursor.as_deref(), body.limit);
            let list = forgust_db::lfs_locks::list(&state.db, project.id, None, None, after, limit + 1).await?;
            let next = (list.len() as i64 > limit).then(|| list[limit as usize - 1].id.to_string());
            let (ours, theirs): (Vec<_>, Vec<_>) = list.iter().take(limit as usize).partition(|l| l.owner_id == u.id);
            Ok(json_lfs(json!({
                "ours": ours.into_iter().map(lock_json).collect::<Vec<_>>(),
                "theirs": theirs.into_iter().map(lock_json).collect::<Vec<_>>(),
                "next_cursor": next,
            })))
        }
        (Method::POST, p) if p.starts_with("locks/") && p.ends_with("/unlock") => {
            let Some(u) = user.filter(|_| write) else { return Ok(lfs_error(StatusCode::FORBIDDEN, "You cannot push to this repository")) };
            let Ok(id) = p["locks/".len()..p.len() - "/unlock".len()].parse::<i64>() else {
                return Ok(lfs_error(StatusCode::NOT_FOUND, "Lock not found"));
            };
            let Some(lock) = forgust_db::lfs_locks::find(&state.db, project.id, id).await? else {
                return Ok(lfs_error(StatusCode::NOT_FOUND, "Lock not found"));
            };
            if lock.owner_id != u.id {
                let principal = Principal::User { user: u.clone(), scopes: None };
                if !body.force {
                    return Ok(lfs_error(
                        StatusCode::FORBIDDEN,
                        &format!("{} is locked by {}; unlock it with --force (Maintainer)", lock.path, lock.owner_username),
                    ));
                }
                if !can_project(&state, &principal, ProjectAction::Admin, &project).await? {
                    return Ok(lfs_error(StatusCode::FORBIDDEN, "only a Maintainer can unlock someone else's lock"));
                }
            }
            forgust_db::lfs_locks::delete(&state.db, project.id, id).await?;
            Ok(json_lfs(json!({ "lock": lock_json(&lock) })))
        }
        _ => Ok(lfs_error(StatusCode::NOT_FOUND, "Not found")),
    }
}

fn json_lfs(v: Value) -> Response {
    let mut r = axum::Json(v).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(LFS_JSON));
    r
}

async fn batch(state: AppState, project: Project, write: bool, req: Request) -> ApiResult<Response> {
    let auth = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    let bytes = axum::body::to_bytes(req.into_body(), 4 << 20).await.map_err(|_| ApiError::bad("batch request too large"))?;
    let br: BatchRequest = serde_json::from_slice(&bytes).map_err(|e| ApiError::bad(format!("bad batch request: {e}")))?;
    if !br.transfers.is_empty() && !br.transfers.iter().any(|t| t == "basic") {
        return Ok(lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "only the basic transfer adapter is supported"));
    }
    if br.operation == "upload" && !write {
        return Ok(lfs_error(StatusCode::FORBIDDEN, "You cannot push to this repository"));
    }
    if br.operation != "upload" && br.operation != "download" {
        return Ok(lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "unknown operation"));
    }
    let base = format!("{}/info/lfs/objects", state.cfg.http_clone_url(&project.full_path));
    let mut header_map = serde_json::Map::new();
    if let Some(a) = &auth {
        header_map.insert("Authorization".into(), Value::String(a.clone()));
    }
    let mut out = Vec::new();
    let mut incoming = 0u64; // bytes this upload adds to the project
    // what the project has of these, in one query (a batch is up to 100 objects)
    let wanted: Vec<String> = br.objects.iter().filter(|o| valid_oid(&o.oid)).map(|o| o.oid.clone()).collect();
    let have: std::collections::HashMap<String, i64> =
        forgust_db::lfs::linked_sizes(&state.db, project.id, &wanted).await?.into_iter().collect();
    for o in br.objects {
        if !valid_oid(&o.oid) || o.size < 0 {
            out.push(json!({ "oid": o.oid, "size": o.size, "error": { "code": 422, "message": "invalid object" } }));
            continue;
        }
        if br.operation == "download" {
            match have.get(&o.oid).copied() {
                Some(size) => {
                    let href = match state.blobs.presigned_get(&lfs_key(&o.oid), Duration::from_secs(3600)).await? {
                        Some(url) => json!({ "href": url.to_string(), "expires_in": 3600 }),
                        None => json!({ "href": format!("{base}/{}", o.oid), "header": header_map, "expires_in": 3600 }),
                    };
                    out.push(json!({ "oid": o.oid, "size": size, "authenticated": true, "actions": { "download": href } }));
                }
                None => out.push(json!({ "oid": o.oid, "size": o.size, "error": { "code": 404, "message": "Object does not exist" } })),
            }
        } else {
            if o.size as u64 > state.cfg.limits.max_lfs_object_bytes {
                out.push(json!({ "oid": o.oid, "size": o.size, "error": { "code": 422, "message": "object is larger than the instance limit" } }));
                continue;
            }
            if have.contains_key(&o.oid) {
                // already here: nothing to upload
                out.push(json!({ "oid": o.oid, "size": o.size, "authenticated": true }));
            } else {
                incoming += o.size as u64;
                out.push(json!({
                    "oid": o.oid, "size": o.size, "authenticated": true,
                    "actions": { "upload": { "href": format!("{base}/{}", o.oid), "header": header_map, "expires_in": 3600 } }
                }));
            }
        }
    }
    // storage quota: the whole upload must fit
    if incoming > 0
        && let Some(room) = crate::services::quotas::room(&state, project.id).await?
        && incoming > room.left
    {
        return Ok(lfs_error(StatusCode::INSUFFICIENT_STORAGE, &room.message()));
    }
    Ok(json_lfs(json!({ "transfer": "basic", "objects": out, "hash_algo": "sha256" })))
}

async fn download(state: AppState, project: Project, oid: &str) -> ApiResult<Response> {
    if !valid_oid(oid) {
        return Ok(lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid oid"));
    }
    if forgust_db::lfs::linked_size(&state.db, project.id, oid).await?.is_none() {
        return Ok(lfs_error(StatusCode::NOT_FOUND, "Object does not exist"));
    }
    let Some((size, stream)) = state.blobs.get(&lfs_key(oid)).await? else {
        return Ok(lfs_error(StatusCode::NOT_FOUND, "Object is missing from storage"));
    };
    let mut r = Response::new(Body::from_stream(stream));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    Ok(r)
}

async fn upload(state: AppState, project: Project, oid: String, req: Request) -> ApiResult<Response> {
    if !valid_oid(&oid) {
        return Ok(lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid oid"));
    }
    if let Err(msg) = crate::services::quotas::cap(&state, project.id, u64::MAX).await? {
        return Ok(lfs_error(StatusCode::INSUFFICIENT_STORAGE, &msg));
    }
    let body = req.into_body().into_data_stream().map_err(std::io::Error::other).boxed();
    match store_verified(&state, project.id, &oid, body).await {
        Ok(_) => Ok(StatusCode::OK.into_response()),
        Err(e) => Ok(lfs_error(StatusCode::UNPROCESSABLE_ENTITY, &e)),
    }
}

/// Store an object's content under its oid after checking its sha256 and size
/// limit, and link it to the project (uploads and imports).
pub async fn store_verified(state: &AppState, project_id: i64, oid: &str, body: crate::blob::ByteStream) -> Result<u64, String> {
    if !valid_oid(oid) {
        return Err("invalid oid".into());
    }
    let limit = state.cfg.limits.max_lfs_object_bytes;
    let tmp = format!("tmp/lfs-{}-{}", oid, crypto::random_urlsafe(8));
    let (body, hashed) = crate::blob::hashing(body, Some(limit), "object too large");
    if let Err(e) = state.blobs.put_stream(&tmp, body).await {
        let _ = state.blobs.delete(&tmp).await;
        return Err(format!("upload failed: {e}"));
    }
    let (digest, size) = hashed.finish();
    if digest != oid {
        let _ = state.blobs.delete(&tmp).await;
        return Err("content does not match the object id".into());
    }
    state.blobs.rename(&tmp, &lfs_key(oid)).await.map_err(|e| e.to_string())?;
    forgust_db::lfs::link(&state.db, project_id, oid, size as i64).await.map_err(|e| e.to_string())?;
    metrics::counter!("forgust_lfs_uploaded_bytes_total").increment(size);
    Ok(size)
}

/// For blob views: an LFS pointer file → (oid, size).
pub fn parse_pointer(data: &[u8]) -> Option<(String, u64)> {
    if data.len() > 1024 {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    if !text.starts_with("version https://git-lfs.github.com/spec/v1") {
        return None;
    }
    let mut oid = None;
    let mut size = None;
    for line in text.lines() {
        if let Some(o) = line.strip_prefix("oid sha256:") {
            oid = Some(o.trim().to_string());
        } else if let Some(s) = line.strip_prefix("size ") {
            size = s.trim().parse().ok();
        }
    }
    Some((oid.filter(|o| valid_oid(o))?, size?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pointers() {
        let p = b"version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n";
        let (oid, size) = parse_pointer(p).unwrap();
        assert_eq!(size, 12345);
        assert!(oid.starts_with("4d7a"));
        assert!(parse_pointer(b"hello").is_none());
    }
}
