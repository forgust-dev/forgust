//! Repository mirrors: a project pulled from another forge (its branches and
//! tags follow the source, and nothing else may change them here), or pushed
//! to one after every change. Only the repository is mirrored: issues, merge
//! requests and LFS objects stay where they are.
//!
//! Addresses get the importer's SSRF check (http(s), no credentials, a public
//! address unless `features.imports_allow_local`) when added and again on
//! every run, and git is pinned to the checked addresses. A mirror's address
//! never changes, so its stored token cannot be sent anywhere else.

use forgust_db::project_mirrors::{self as mdb, Mirror};
use forgust_db::projects::Project;
use forgust_storage::{StorageError, ZERO_OID};
use graphile_worker::{IntoTaskHandlerResult, TaskHandler, WorkerContext};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
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

/// Refuse writes to a pull mirror's branches and tags (pushes, web commits,
/// merges, rebases, suggestions, tags). Issues and comments stay open.
pub async fn writable(state: &AppState, project_id: i64) -> ApiResult<()> {
    match mdb::pulled_from(&state.db, project_id).await? {
        Some(url) => Err(ApiError::forbidden(read_only(&url))),
        None => Ok(()),
    }
}

/// The address after the SSRF check: http(s), no credentials in it.
pub async fn checked(state: &AppState, raw: &str) -> Result<url::Url, String> {
    let (url, _) = crate::import::client::checked_url(raw, state.cfg.features.imports_allow_local).await.map_err(|e| e.to_string())?;
    Ok(url)
}

/// git's settings for talking to the mirror: its credentials, the checked
/// addresses it is pinned to, and the instance's CA bundle.
async fn remote(state: &AppState, m: &Mirror) -> Result<(url::Url, Vec<(String, String)>), String> {
    use base64::Engine;
    let (url, addrs) =
        crate::import::client::checked_url(&m.url, state.cfg.features.imports_allow_local).await.map_err(|e| e.to_string())?;
    let mut cfg = crate::import::client::pin(&url, &addrs);
    if let Some(sealed) = &m.token_enc {
        let token = state.secrets.decrypt(sealed).map_err(|e| format!("token: {e}"))?;
        let token = String::from_utf8(token).map_err(|_| "token: not UTF-8".to_string())?;
        let user = if m.username.is_empty() { "oauth2" } else { m.username.as_str() };
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"));
        cfg.push(("http.extraHeader".into(), format!("Authorization: Basic {basic}")));
    }
    if let Some(bundle) = crate::tls::bundle_file().map_err(|e| e.to_string())? {
        cfg.push(("http.sslCAInfo".into(), bundle.display().to_string()));
    }
    Ok((url, cfg))
}

fn git_error(e: StorageError) -> String {
    let text = e.to_string();
    let text = text.trim();
    // the end of git's complaint says what went wrong
    match text.char_indices().rev().nth(600) {
        Some((i, _)) => format!("…{}", &text[i..]),
        None => text.to_string(),
    }
}

/// Bring the source's branches and tags over: new and moved ones are set
/// (diverged ones too: the source wins), and those gone there are deleted.
/// Returns what changed.
pub async fn pull(state: &AppState, p: &Project, m: &Mirror) -> Result<Vec<RefUpdate>, String> {
    if let Some(r) = crate::services::quotas::room(state, p.id).await.map_err(|e| e.to_string())?
        && r.full()
    {
        return Err(r.message());
    }
    let (url, cfg) = remote(state, m).await?;
    let st = &state.storage;
    // a clean staging area: what a previous run left must not look like the source
    for r in st.refs(p.id, STAGING).await.map_err(|e| e.to_string())? {
        let _ = st.delete_ref(p.id, &r.name, None).await;
    }
    let refspecs = vec![format!("+refs/heads/*:{STAGING}heads/*"), format!("+refs/tags/*:{STAGING}tags/*")];
    st.fetch(p.id, url.as_str(), &refspecs, &cfg).await.map_err(git_error)?;
    let mut updates = Vec::new();
    let result = async {
        let staged = st.refs(p.id, STAGING).await.map_err(|e| e.to_string())?;
        let local_heads = st.refs(p.id, "refs/heads/").await.map_err(|e| e.to_string())?;
        // a source without branches is more likely broken than emptied: keep ours
        if !staged.iter().any(|r| r.name.starts_with(&format!("{STAGING}heads/"))) && !local_heads.is_empty() {
            return Err("the source has no branches: nothing was changed here".to_string());
        }
        let mut local = local_heads;
        local.extend(st.refs(p.id, "refs/tags/").await.map_err(|e| e.to_string())?);
        let local: std::collections::HashMap<String, String> = local.into_iter().map(|r| (r.name, r.target)).collect();
        let mut seen = std::collections::HashSet::new();
        for r in &staged {
            let name = format!("refs/{}", &r.name[STAGING.len()..]);
            seen.insert(name.clone());
            let old = local.get(&name).cloned();
            if old.as_deref() == Some(r.target.as_str()) {
                continue;
            }
            st.update_ref(p.id, &name, &r.target, Some(old.as_deref().unwrap_or(ZERO_OID))).await.map_err(|e| e.to_string())?;
            updates.push(RefUpdate { old: old.unwrap_or_else(|| ZERO_OID.to_string()), new: r.target.clone(), name });
        }
        for (name, old) in &local {
            if !seen.contains(name) {
                st.delete_ref(p.id, name, Some(old)).await.map_err(|e| e.to_string())?;
                updates.push(RefUpdate { old: old.clone(), new: ZERO_OID.to_string(), name: name.clone() });
            }
        }
        Ok(())
    }
    .await;
    for r in st.refs(p.id, STAGING).await.unwrap_or_default() {
        let _ = st.delete_ref(p.id, &r.name, None).await;
    }
    result.map(|()| updates)
}

/// Send this repository's branches and tags to the mirror: every branch
/// (forced, and those gone here deleted there), or the protected ones only
/// (forced, nothing deleted).
pub async fn push(state: &AppState, p: &Project, m: &Mirror) -> Result<(), String> {
    let (url, cfg) = remote(state, m).await?;
    let mut refspecs = vec!["+refs/tags/*:refs/tags/*".to_string()];
    if m.only_protected {
        let rules = forgust_db::protected_branches::list(&state.db, p.id).await.map_err(|e| e.to_string())?;
        for r in state.storage.refs(p.id, "refs/heads/").await.map_err(|e| e.to_string())? {
            let branch = &r.name["refs/heads/".len()..];
            if rules.iter().any(|rule| rule.matches(branch)) {
                refspecs.push(format!("+{0}:{0}", r.name));
            }
        }
    } else {
        refspecs.push("+refs/heads/*:refs/heads/*".into());
    }
    state.storage.push(p.id, url.as_str(), &refspecs, &cfg, !m.only_protected).await.map_err(git_error)
}

/// Who a pull's pipelines run as: the mirror's creator, while they are active
/// and may still push to the project. Nobody (no pipelines) otherwise.
async fn pipelines_as(state: &AppState, p: &Project, m: &Mirror) -> Option<Principal> {
    if !m.trigger_pipelines {
        return None;
    }
    let user = forgust_db::users::find(&state.db, m.created_by?).await.ok()??;
    if user.state != "active" {
        return None;
    }
    // `Push` is refused on a pull mirror's repository: the role is what counts
    let level = forgust_db::authz::project_level(&state.db, user.id, p.id).await.ok().flatten().unwrap_or(0);
    (level >= forgust_db::access::DEVELOPER || user.is_admin).then_some(Principal::User { user, scopes: None })
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
                let principal = pipelines_as(state, &p, &m).await.map(|p| p.encode());
                let mut conn = state.db.acquire().await?;
                state.jobs.enqueue(&mut conn, PostReceive { project_id: p.id, updates, principal }.request()).await?;
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
    let due = mdb::due_pulls(&state.db, 200).await?;
    let mut conn = state.db.acquire().await?;
    for id in due {
        let Some(m) = mdb::find(&mut *conn, id).await? else { continue };
        // picked once: the next sweep waits for its interval again
        mdb::mark_attempt(&mut *conn, id).await?;
        state.jobs.enqueue(&mut conn, MirrorUpdate::request(m.project_id, id)).await?;
    }
    Ok(())
}
