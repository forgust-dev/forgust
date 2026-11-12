//! Push-time enforcement (architecture: Push path).
//!
//! git runs `forgust hook pre-receive|post-receive` (installed as `core.hooksPath`
//! for each receive-pack). The hook reads ref updates from stdin, works out which
//! updates are force pushes in its own environment (new objects are still in the
//! quarantine directory), and asks the internal API over a Unix socket.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use bytes::Bytes;
use forgust_db::access::{DEVELOPER, MAINTAINER};
use http_body_util::{BodyExt, Full};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::Principal;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::jobs::{PostReceive, RefUpdate};
use crate::state::AppState;
use forgust_storage::ZERO_OID;

pub const ENV_SOCKET: &str = "FORGUST_HOOK_SOCKET";
pub const ENV_SECRET: &str = "FORGUST_HOOK_SECRET";
pub const ENV_PROJECT: &str = "FORGUST_PROJECT_ID";
pub const ENV_PRINCIPAL: &str = "FORGUST_PRINCIPAL";
pub const ENV_PROTOCOL: &str = "FORGUST_PROTOCOL";

#[derive(Debug, Serialize, Deserialize)]
pub struct HookUpdate {
    pub old: String,
    pub new: String,
    pub name: String,
    /// Non-fast-forward (computed by the hook in the quarantine environment).
    pub force: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HookRequest {
    pub project_id: i64,
    pub principal: String,
    pub protocol: String,
    pub updates: Vec<HookUpdate>,
    /// The push's quarantine directory (pre-receive): its objects are not in
    /// the repository yet.
    #[serde(default)]
    pub quarantine: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HookResponse {
    pub allowed: bool,
    pub message: String,
}

/// Write the hook scripts that git calls (`core.hooksPath`).
pub fn install_hooks(dir: &Path, exe: Option<&Path>) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    let exe = match exe {
        Some(e) => e.to_path_buf(),
        None => std::env::current_exe()?,
    };
    for name in ["pre-receive", "post-receive"] {
        let path = dir.join(name);
        let script = format!("#!/bin/sh\nexec '{}' hook {name}\n", exe.display().to_string().replace('\'', "'\\''"));
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Environment for a receive-pack that should run our hooks.
pub fn hook_env(state: &AppState, project_id: i64, principal: &Principal, protocol: &str) -> Vec<(String, String)> {
    vec![
        (ENV_SOCKET.into(), state.hooks.socket.display().to_string()),
        (ENV_SECRET.into(), state.hooks.secret.clone()),
        (ENV_PROJECT.into(), project_id.to_string()),
        (ENV_PRINCIPAL.into(), principal.encode()),
        (ENV_PROTOCOL.into(), protocol.into()),
    ]
}

// ------------------------------------------------------------------ hook side (CLI)

/// Entry point of `forgust hook <name>`. Returns the process exit code.
pub async fn run_hook(name: &str) -> i32 {
    match run_hook_inner(name).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Forgust: hook {name} failed: {e:#}");
            // fail closed for pre-receive, open for post-receive (the push landed)
            if name == "pre-receive" { 1 } else { 0 }
        }
    }
}

async fn run_hook_inner(name: &str) -> anyhow::Result<i32> {
    let socket = PathBuf::from(std::env::var(ENV_SOCKET)?);
    let secret = std::env::var(ENV_SECRET)?;
    let project_id: i64 = std::env::var(ENV_PROJECT)?.parse()?;
    let principal = std::env::var(ENV_PRINCIPAL)?;
    let protocol = std::env::var(ENV_PROTOCOL).unwrap_or_default();

    let mut updates = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let mut parts = line.split_whitespace();
        let (Some(old), Some(new), Some(refname)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let force = name == "pre-receive" && old != ZERO_OID && new != ZERO_OID && !is_ancestor(old, new)?;
        updates.push(HookUpdate { old: old.into(), new: new.into(), name: refname.into(), force });
    }
    let quarantine = std::env::var("GIT_QUARANTINE_PATH").ok().filter(|q| !q.is_empty());
    let body = serde_json::to_vec(&HookRequest { project_id, principal, protocol, updates, quarantine })?;
    let resp: HookResponse = call_socket(&socket, &format!("/internal/{name}"), &secret, body).await?;
    if !resp.message.is_empty() {
        for l in resp.message.lines() {
            eprintln!("Forgust: {l}");
        }
    }
    Ok(if resp.allowed { 0 } else { 1 })
}

/// Runs in the hook's environment, so quarantined objects are visible.
fn is_ancestor(old: &str, new: &str) -> anyhow::Result<bool> {
    let status = std::process::Command::new("git")
        .args(["merge-base", "--is-ancestor", old, new])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!("git merge-base failed"),
    }
}

async fn call_socket<T: for<'de> Deserialize<'de>>(socket: &Path, path: &str, secret: &str, body: Vec<u8>) -> anyhow::Result<T> {
    let stream = tokio::net::UnixStream::connect(socket).await?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await?;
    tokio::spawn(conn);
    let req = hyper::Request::post(path)
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("x-forgust-hook-secret", secret)
        .body(Full::new(Bytes::from(body)))?;
    let resp = sender.send_request(req).await?;
    let status = resp.status();
    let bytes = resp.into_body().collect().await?.to_bytes();
    anyhow::ensure!(status.is_success(), "internal API returned {status}: {}", String::from_utf8_lossy(&bytes));
    Ok(serde_json::from_slice(&bytes)?)
}

// ------------------------------------------------------------------ server side

pub fn internal_router(state: AppState) -> Router {
    Router::new().route("/internal/pre-receive", post(pre_receive)).route("/internal/post-receive", post(post_receive)).with_state(state)
}

fn check_secret(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let sent = headers.get("x-forgust-hook-secret").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !crypto::eq_ct(sent, &state.hooks.secret) {
        return Err(ApiError::Unauthorized);
    }
    Ok(())
}

/// Rebuild the principal from its `kind:id` encoding.
pub async fn decode_principal(state: &AppState, encoded: &str) -> ApiResult<Principal> {
    let (kind, id) = encoded.split_once(':').ok_or_else(|| ApiError::bad("bad principal"))?;
    let id: i64 = id.parse().map_err(|_| ApiError::bad("bad principal"))?;
    Ok(match kind {
        "user" | "bot" => {
            let user = forgust_db::users::find(&state.db, id).await?.ok_or(ApiError::Unauthorized)?;
            Principal::User { user, scopes: None }
        }
        "deploy_key" => Principal::DeployKey { key_id: id },
        "deploy_token" => {
            // only the id matters for push rules: deploy tokens never push to protected branches
            Principal::DeployToken {
                token: forgust_db::tokens::DeployToken {
                    id,
                    name: String::new(),
                    username: String::new(),
                    project_id: None,
                    namespace_id: None,
                    scopes: vec![],
                    expires_at: None,
                    revoked: false,
                    created_at: chrono::Utc::now(),
                },
            }
        }
        _ => return Err(ApiError::forbidden("this principal cannot push")),
    })
}

/// Refs users may not write directly.
const INTERNAL_REF_PREFIXES: &[&str] = &[
    "refs/merge-requests/",
    "refs/keep-around/",
    "refs/forgust/",
    "refs/pipelines/",
    "refs/environments/",
    forgust_storage::REBASE_PREFIX,
];

/// Decide one push against protected branch rules.
pub async fn check_push(
    state: &AppState,
    principal: &Principal,
    project_id: i64,
    updates: &[HookUpdate],
    quarantine: Option<&Path>,
) -> ApiResult<HookResponse> {
    if let Some(p) = forgust_db::projects::find(&state.db, project_id).await?
        && p.archived()
    {
        return Ok(HookResponse { allowed: false, message: crate::auth::authz::ARCHIVED.into() });
    }
    // storage quota: new objects must fit (a push that only moves or deletes refs
    // brings none and always passes)
    if let Some(q) = quarantine {
        // a walk of the quarantine folder: off the async workers
        let dir = q.to_path_buf();
        let incoming = tokio::task::spawn_blocking(move || crate::services::quotas::dir_bytes(&dir)).await.unwrap_or(0);
        if incoming > 0
            && let Some(room) = crate::services::quotas::room(state, project_id).await?
            && incoming > room.left
        {
            return Ok(HookResponse { allowed: false, message: room.message() });
        }
    }
    let rules = forgust_db::protected_branches::list(&state.db, project_id).await?;
    let tag_rules = forgust_db::protected_branches::tags(&state.db, project_id).await?;
    let level = match principal {
        // a CI job acts for its user (it creates tags through the releases API)
        Principal::User { user, .. } | Principal::Job { user, .. } => {
            if user.is_admin {
                forgust_db::access::OWNER
            } else {
                forgust_db::authz::project_level(&state.db, user.id, project_id).await?.unwrap_or(0)
            }
        }
        _ => 0,
    };
    for u in updates {
        if INTERNAL_REF_PREFIXES.iter().any(|p| u.name.starts_with(p)) {
            return Ok(HookResponse { allowed: false, message: format!("{} is managed by Forgust and cannot be pushed to.", u.name) });
        }
        // only branches and tags: refs/replace/* would rewrite what Forgust reads
        // of protected branches, refs/notes and the rest have no rules at all
        if !u.name.starts_with("refs/heads/") && !u.name.starts_with("refs/tags/") {
            return Ok(HookResponse { allowed: false, message: format!("{}: only branches and tags can be pushed.", u.name) });
        }
        // a tag named like a branch (or the reverse) would shadow it wherever the bare name is used
        if u.old == ZERO_OID && u.new != ZERO_OID {
            let other = match u.name.strip_prefix("refs/heads/") {
                Some(b) => format!("refs/tags/{b}"),
                None => format!("refs/heads/{}", &u.name["refs/tags/".len()..]),
            };
            if state.storage.resolve(project_id, &other).await?.is_some() {
                return Ok(HookResponse {
                    allowed: false,
                    message: format!("{} cannot be created: {other} already uses that name.", u.name),
                });
            }
        }
        if let Some(tag) = u.name.strip_prefix("refs/tags/") {
            let matching: Vec<_> = tag_rules.iter().filter(|r| r.matches(tag)).collect();
            if matching.is_empty() {
                continue;
            }
            if u.new == ZERO_OID {
                return Ok(HookResponse {
                    allowed: false,
                    message: format!("You cannot delete the protected tag {tag} by pushing; a Maintainer can delete it in Forgust."),
                });
            }
            if u.old != ZERO_OID {
                return Ok(HookResponse { allowed: false, message: format!("You cannot update the protected tag {tag}.") });
            }
            let human = matches!(principal, Principal::User { user, .. } | Principal::Job { user, .. } if !user.is_bot() || level > 0);
            if !(human && matching.iter().any(|r| r.create_access_level > 0 && level >= r.create_access_level.max(DEVELOPER))) {
                let who = matching.iter().map(|r| r.create_access_level).max().unwrap_or(MAINTAINER);
                let msg = if who == 0 {
                    format!("You are not allowed to create the protected tag {tag}: no one may.")
                } else {
                    format!(
                        "You are not allowed to create the protected tag {tag}. It requires the {} role.",
                        forgust_db::access::name(who)
                    )
                };
                return Ok(HookResponse { allowed: false, message: msg });
            }
            continue;
        }
        let Some(branch) = u.name.strip_prefix("refs/heads/") else { continue };
        let matching = forgust_db::protected_branches::matching(&rules, branch);
        if matching.is_empty() {
            continue;
        }
        if u.new == ZERO_OID {
            return Ok(HookResponse { allowed: false, message: format!("You cannot delete the protected branch {branch}.") });
        }
        // the most permissive matching rule applies, as in GitLab
        let may_push = matching.iter().any(|r| match principal {
            Principal::User { user, .. } if !user.is_bot() || level > 0 => {
                r.push_access_level > 0 && level >= r.push_access_level.max(DEVELOPER)
            }
            Principal::DeployKey { .. } => r.allow_deploy_keys,
            _ => false,
        });
        if !may_push {
            let who = matching.iter().map(|r| r.push_access_level).max().unwrap_or(MAINTAINER);
            let msg = if who == 0 {
                format!("You are not allowed to push code to protected branch {branch}: no one may push.")
            } else {
                format!(
                    "You are not allowed to push code to protected branch {branch}. Pushing requires the {} role.",
                    forgust_db::access::name(who)
                )
            };
            return Ok(HookResponse { allowed: false, message: msg });
        }
        if u.force && !matching.iter().any(|r| r.allow_force_push) {
            return Ok(HookResponse {
                allowed: false,
                message: format!("You are not allowed to force push code to the protected branch {branch}."),
            });
        }
    }
    // then the project's push rules and LFS locks
    if let Some(message) = crate::services::push_rules::check(state, principal, project_id, quarantine, updates).await? {
        return Ok(HookResponse { allowed: false, message });
    }
    Ok(HookResponse { allowed: true, message: String::new() })
}

async fn pre_receive(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<HookRequest>) -> ApiResult<Json<HookResponse>> {
    check_secret(&state, &headers)?;
    let principal = decode_principal(&state, &req.principal).await?;
    let quarantine = req.quarantine.as_deref().map(Path::new);
    let resp = check_push(&state, &principal, req.project_id, &req.updates, quarantine).await?;
    if !resp.allowed {
        crate::services::audit(
            &state,
            &principal,
            "push_rejected",
            Some(("project", req.project_id)),
            json!({ "reason": resp.message, "refs": req.updates.iter().map(|u| &u.name).collect::<Vec<_>>() }),
            None,
        )
        .await;
        metrics::counter!("forgust_push_rejected_total").increment(1);
    }
    Ok(Json(resp))
}

async fn post_receive(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<HookRequest>) -> ApiResult<Json<HookResponse>> {
    check_secret(&state, &headers)?;
    let updates = req.updates.into_iter().map(|u| RefUpdate { old: u.old, new: u.new, name: u.name }).collect();
    let mut tx = state.db.begin().await?;
    state.jobs.enqueue(&mut tx, PostReceive { project_id: req.project_id, updates, principal: Some(req.principal) }.request()).await?;
    tx.commit().await?;
    Ok(Json(HookResponse { allowed: true, message: String::new() }))
}
