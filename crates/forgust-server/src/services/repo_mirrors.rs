//! Repository mirrors: a project pulled from another forge (its branches and
//! tags follow the source, and nothing else may change them here), or pushed
//! to one after every change. Only the repository is mirrored: issues, merge
//! requests and LFS objects stay where they are.
//!
//! Addresses get the importer's SSRF check (http(s), no credentials, a public
//! address unless `features.imports_allow_local`) when added and again on
//! every run, and git is pinned to the checked addresses. A mirror's address
//! never changes, so its stored token cannot be sent anywhere else.

use std::collections::HashMap;

use forgust_db::project_mirrors::{self as mdb, Mirror, NewMirror};
use forgust_db::projects::Project;
use forgust_storage::{RefChange, StorageError, ZERO_OID};
use graphile_worker::{IntoTaskHandlerResult, TaskHandler, WorkerContext};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::jobs::{JobRequest, PostReceive, RefUpdate};
use crate::state::AppState;

/// Push mirrors a project may have.
pub const MAX_PUSH_MIRRORS: usize = 10;
/// Where a pull lands before it is compared with the branches and tags here.
const STAGING: &str = "refs/mirror/";

pub fn read_only(url: &str) -> String {
    format!("this repository is a pull mirror of {url}: push there, or remove the mirror in Settings > Repository")
}

/// Refuse changes to a pull mirror's branches and tags made here (web
/// commits, branches and tags, rebases, suggestions) before any work: the
/// pre-receive check (`hooks::check_push`) refuses them too, and pushes; the
/// merge status blocks merges. Issues and comments stay open.
pub async fn writable(state: &AppState, project_id: i64) -> ApiResult<()> {
    match mdb::pulled_from(&state.db, project_id).await? {
        Some(url) => Err(ApiError::forbidden(read_only(&url))),
        None => Ok(()),
    }
}

/// `Push` on the project, and a repository that is not a pull mirror.
pub async fn require_write(state: &AppState, principal: &Principal, p: &Project) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    writable(state, p.id).await
}

/// Add a mirror, audited as done by `principal`.
pub async fn add(state: &AppState, principal: &Principal, m: NewMirror<'_>) -> ApiResult<i64> {
    let id = mdb::insert(&state.db, m).await?;
    crate::services::audit(
        state,
        principal,
        "mirror_added",
        Some(("project", m.project_id)),
        serde_json::json!({ "mirror": id, "direction": m.direction, "url": m.url }),
        None,
    )
    .await;
    Ok(id)
}

/// The address after the SSRF check: http(s), no credentials in it.
pub async fn checked(state: &AppState, raw: &str) -> Result<url::Url, String> {
    let (url, _) = crate::import::client::checked_url(raw, state.cfg.features.imports_allow_local).await.map_err(|e| e.to_string())?;
    Ok(url)
}

/// The mirror's address, checked again, and git's settings for it (its
/// credentials, the pinned addresses, the CA bundle).
async fn remote(state: &AppState, m: &Mirror) -> Result<(url::Url, Vec<(String, String)>), String> {
    let mut auth = Vec::new();
    if let Some(sealed) = &m.token_enc {
        let token = state.secrets.decrypt_str(sealed).map_err(|e| format!("token: {e}"))?;
        auth.push(crate::import::client::basic_auth(if m.username.is_empty() { "oauth2" } else { &m.username }, &token));
    }
    crate::import::client::git_remote(&m.url, state.cfg.features.imports_allow_local, auth).await.map_err(|e| e.to_string())
}

/// What went wrong, in a line: git's complaint without its prefixes, or
/// plainly when the remote turned the credentials down.
fn git_error(e: StorageError) -> String {
    let text = match e {
        StorageError::Git(m) => m,
        e => e.to_string(),
    };
    let refused =
        ["could not read Username", "Authentication failed", "terminal prompts disabled", "returned error: 401", "returned error: 403"];
    if refused.iter().any(|r| text.contains(r)) {
        return "the remote refused the credentials (or the repository is not there)".into();
    }
    let text = text.trim().trim_start_matches("fatal: ").trim_start_matches("error: ");
    // the end of git's complaint says what went wrong
    match text.char_indices().rev().nth(600) {
        Some((i, _)) => format!("…{}", &text[i..]),
        None => text.to_string(),
    }
}

/// Bring the source's branches and tags over: those gone there are deleted,
/// then new and moved ones set (diverged ones too: the source wins), each in
/// one transaction.
/// Returns what changed.
pub async fn pull(state: &AppState, p: &Project, m: &Mirror) -> Result<Vec<RefUpdate>, String> {
    let err = |e: StorageError| e.to_string();
    crate::services::quotas::check_write(state, p.id, 0).await.map_err(|e| e.to_string())?;
    let (url, cfg) = remote(state, m).await?;
    let st = &state.storage;
    // a writer like a push: maintenance (repack, prune) waits, or the fetched
    // objects could go before the refs point at them
    let _lock = state.repo_locks.get(p.id).read_owned().await;
    let refs = |prefixes: &'static [&'static str]| async move {
        Ok::<HashMap<String, String>, String>(st.ref_targets(p.id, prefixes).await.map_err(err)?.into_iter().collect())
    };
    let clear_staging = || async {
        let stale: Vec<RefChange> = refs(&[STAGING]).await?.into_keys().map(|name| RefChange { name, new: None, old: None }).collect();
        st.transact_refs(p.id, &stale).await.map_err(err)
    };
    // a clean staging area: what a previous run left must not look like the source
    clear_staging().await?;
    // what is here before the fetch: git may bring tags straight into
    // refs/tags (auto-following), and those are changes too
    let before = refs(&["refs/heads/", "refs/tags/"]).await?;
    let refspecs = vec![format!("+refs/heads/*:{STAGING}heads/*"), format!("+refs/tags/*:{STAGING}tags/*")];
    st.fetch(p.id, url.as_str(), &refspecs, &cfg).await.map_err(git_error)?;
    let result = async {
        let staged: HashMap<String, String> =
            refs(&[STAGING]).await?.into_iter().map(|(name, target)| (format!("refs/{}", &name[STAGING.len()..]), target)).collect();
        // a source without branches is more likely broken than emptied: keep ours
        if !staged.keys().any(|n| n.starts_with("refs/heads/")) && before.keys().any(|n| n.starts_with("refs/heads/")) {
            return Err("the source has no branches: nothing was changed here".to_string());
        }
        let now = refs(&["refs/heads/", "refs/tags/"]).await?;
        let mut changes: Vec<RefChange> = now
            .iter()
            .filter(|(name, _)| !staged.contains_key(*name))
            .map(|(name, current)| RefChange { name: name.clone(), new: None, old: Some(current.clone()) })
            .collect();
        changes.extend(staged.iter().filter(|(name, target)| now.get(*name) != Some(target)).map(|(name, target)| RefChange {
            name: name.clone(),
            new: Some(target.clone()),
            old: Some(now.get(name).cloned().unwrap_or_else(|| ZERO_OID.to_string())),
        }));
        if !changes.is_empty() {
            // what came must fit: past the quota, nothing is applied (the
            // fetched objects go with the next maintenance)
            crate::jobs::refresh_repository_size(state, p.id).await;
            if let Some(r) = crate::services::quotas::room(state, p.id).await.map_err(|e| e.to_string())?
                && r.used > r.quota
            {
                return Err(format!("the source does not fit in the storage quota ({}): nothing was changed here", r.message()));
            }
            // deletions first, on their own: git cannot remove `foo` and create
            // `foo/bar` in one transaction
            let (deletes, sets): (Vec<RefChange>, Vec<RefChange>) = changes.into_iter().partition(|c| c.new.is_none());
            st.transact_refs(p.id, &deletes).await.map_err(err)?;
            st.transact_refs(p.id, &sets).await.map_err(err)?;
            follow_default_branch(state, p, &url, &cfg).await.map_err(|e| e.to_string())?;
        }
        // what changed since before the fetch, auto-followed tags included
        let gone = before.iter().filter(|(name, _)| !staged.contains_key(*name)).map(|(name, old)| (name, old.as_str(), ZERO_OID));
        let set = staged.iter().map(|(name, new)| (name, before.get(name).map(String::as_str).unwrap_or(ZERO_OID), new.as_str()));
        Ok(gone
            .chain(set)
            .filter(|(_, old, new)| old != new)
            .map(|(name, old, new)| RefUpdate { name: name.clone(), old: old.to_string(), new: new.to_string() })
            .collect())
    }
    .await;
    let _ = clear_staging().await;
    result
}

/// When the default branch went away (renamed upstream, or the project had
/// its own), the source's default takes its place; else `main`, `master`, or
/// the first branch.
async fn follow_default_branch(state: &AppState, p: &Project, url: &url::Url, cfg: &[(String, String)]) -> anyhow::Result<()> {
    let st = &state.storage;
    let current = p.default_branch.as_deref().unwrap_or_default();
    if !current.is_empty() && st.resolve(p.id, &format!("refs/heads/{current}")).await?.is_some() {
        return Ok(());
    }
    let mut names: Vec<String> =
        st.ref_targets(p.id, &["refs/heads/"]).await?.into_iter().map(|(name, _)| name["refs/heads/".len()..].to_string()).collect();
    names.sort();
    let source_default = st.remote_head(p.id, url.as_str(), cfg).await.ok().flatten();
    let pick = source_default.as_deref().into_iter().chain(["main", "master"]).find(|n| names.iter().any(|b| b == n)).map(str::to_string);
    match pick.or(names.into_iter().next()) {
        Some(b) => crate::services::repos::adopt_default_branch(state, p.id, &b).await,
        None => Ok(()),
    }
}

/// Send this repository's branches and tags to the mirror: every branch
/// (forced, and those gone here deleted there), or the protected ones only
/// (forced, nothing deleted).
pub async fn push(state: &AppState, p: &Project, m: &Mirror) -> Result<(), String> {
    let (url, cfg) = remote(state, m).await?;
    let mut refspecs = vec!["+refs/tags/*:refs/tags/*".to_string()];
    if m.only_protected {
        let rules = forgust_db::protected_branches::list(&state.db, p.id).await.map_err(|e| e.to_string())?;
        for (name, _) in state.storage.ref_targets(p.id, &["refs/heads/"]).await.map_err(|e| e.to_string())? {
            if rules.iter().any(|rule| rule.matches(&name["refs/heads/".len()..])) {
                refspecs.push(format!("+{name}:{name}"));
            }
        }
    } else {
        refspecs.push("+refs/heads/*:refs/heads/*".into());
    }
    state.storage.push(p.id, url.as_str(), &refspecs, &cfg, !m.only_protected).await.map_err(git_error)
}

/// Who a pull's pipelines run as: whoever added the mirror or turned its
/// pipelines on, while they are active and a Developer or more here. Nobody
/// (no pipelines) otherwise; an administrator without a role here neither.
async fn pipelines_as(state: &AppState, p: &Project, m: &Mirror) -> Option<Principal> {
    if !m.trigger_pipelines {
        return None;
    }
    let user = forgust_db::users::find(&state.db, m.created_by?).await.ok().flatten().filter(|u| u.is_active())?;
    // the role, not `Push`: an administrator's bypass does not count here
    let level = forgust_db::authz::project_level(&state.db, user.id, p.id).await.ok().flatten().unwrap_or(0);
    (level >= forgust_db::access::DEVELOPER).then_some(Principal::User { user, scopes: None })
}

// ------------------------------------------------------------------ jobs

/// Update one mirror (pull or push).
#[derive(Debug, Serialize, Deserialize)]
pub struct MirrorUpdate {
    pub mirror_id: i64,
}

impl MirrorUpdate {
    pub fn request(project_id: i64, mirror_id: i64) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            // one git operation at a time per project; a burst of pushes is one update
            queue: Some(format!("mirror:{project_id}")),
            job_key: Some(format!("mirror:{mirror_id}")),
            payload: serde_json::json!({ "mirror_id": mirror_id }),
            max_attempts: Some(3),
        }
    }
}

impl TaskHandler for MirrorUpdate {
    const IDENTIFIER: &'static str = "mirror_update";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        update(state, self.mirror_id).await.map_err(|e| e.to_string())
    }
}

/// Run one mirror now. Its outcome is recorded on it, not returned: a failed
/// update is not retried before its next turn.
pub async fn update(state: &AppState, mirror_id: i64) -> anyhow::Result<()> {
    let Some(m) = mdb::find(&state.db, mirror_id).await? else { return Ok(()) };
    let Some(p) = forgust_db::projects::find(&state.db, m.project_id).await? else { return Ok(()) };
    if !m.enabled {
        return Ok(());
    }
    if p.archived() {
        mdb::set_result(&state.db, m.id, Some("the project is archived")).await?;
        return Ok(());
    }
    mdb::mark_attempt(&state.db, m.id).await?;
    let outcome = if m.direction == "pull" {
        match pull(state, &p, &m).await {
            Ok(updates) if !updates.is_empty() => {
                let runs_as = pipelines_as(state, &p, &m).await.map(|p| p.encode());
                let mut conn = state.db.acquire().await?;
                state.jobs.enqueue(&mut conn, PostReceive::mirrored(p.id, updates, runs_as).request()).await?;
                Ok(())
            }
            r => r.map(|_| ()),
        }
    } else {
        push(state, &p, &m).await
    };
    if let Err(e) = &outcome {
        tracing::info!(project_id = p.id, mirror = m.id, direction = %m.direction, error = %e, "mirror update failed");
    }
    mdb::set_result(&state.db, m.id, outcome.err().as_deref()).await?;
    Ok(())
}

/// After a change to a project's branches or tags: its push mirrors follow.
pub async fn after_push(state: &AppState, project_id: i64, updates: &[RefUpdate]) -> anyhow::Result<()> {
    if !updates.iter().any(|u| u.name.starts_with("refs/heads/") || u.name.starts_with("refs/tags/")) {
        return Ok(());
    }
    let targets = mdb::push_targets(&state.db, project_id).await?;
    if targets.is_empty() {
        return Ok(());
    }
    let mut conn = state.db.acquire().await?;
    for id in targets {
        state.jobs.enqueue(&mut conn, MirrorUpdate::request(project_id, id)).await?;
    }
    Ok(())
}

/// Pull mirrors whose turn has come (cron, every 5 minutes).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MirrorSweep {}

impl TaskHandler for MirrorSweep {
    const IDENTIFIER: &'static str = "mirror_sweep";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        sweep(state).await.map_err(|e| e.to_string())
    }
}

pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    // picked once: the next sweep waits for their interval again
    let due = mdb::take_due_pulls(&state.db, 200).await?;
    let mut conn = state.db.acquire().await?;
    for (id, project_id) in due {
        state.jobs.enqueue(&mut conn, MirrorUpdate::request(project_id, id)).await?;
    }
    Ok(())
}
