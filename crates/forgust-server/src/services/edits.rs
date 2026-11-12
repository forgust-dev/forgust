//! Changes made in the browser (M7): branches created and deleted, files
//! created, edited, moved and deleted as commits, without a working copy.
//!
//! Every write is a push: Developer+ on the project, the protected-branch rules
//! of the pre-receive hook (server-side writes skip it, so `check_push` runs
//! here), the repo lock, a compare-and-swap on the head the editor started
//! from, and the post-receive job (MR versions, pipelines, webhooks).

use base64::Engine;
use forgust_storage::{FileChange, StorageError, ZERO_OID, validate_path, validate_rev};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::mr::{committer, conflict_error, identity};
use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::jobs::{PostReceive, RefUpdate};
use crate::state::AppState;
use forgust_db::projects::Project;

const MAX_ACTIONS: usize = 100;
/// Content of one file in a browser commit.
const MAX_FILE: usize = 10 << 20;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Create,
    Update,
    Delete,
    Move,
}

/// One file of a commit (GitLab's `actions[]` of `POST .../repository/commits`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct FileAction {
    pub action: ActionKind,
    pub file_path: String,
    /// The file's path before a `move`.
    pub previous_path: Option<String>,
    /// New content (create, update; a move keeps the old content without it).
    pub content: Option<String>,
    /// `text` (default) or `base64`.
    pub encoding: Option<String>,
}

pub struct CommitRequest<'a> {
    pub branch: &'a str,
    /// Start the branch from here when it does not exist (default branch by default).
    pub start_branch: Option<&'a str>,
    pub message: &'a str,
    pub actions: &'a [FileAction],
    /// The branch head the editor loaded: refuse when it moved since.
    pub expected_head: Option<&'a str>,
    /// With `start_branch` on an existing branch: replace it (GitLab's `force`).
    pub force: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Committed {
    pub id: String,
    pub branch: String,
    /// The branch was created by this commit.
    pub created_branch: bool,
}

fn ref_name(kind: &str, prefix: &str, name: &str) -> ApiResult<String> {
    let n = name.trim().trim_start_matches(prefix);
    if n.is_empty() || !validate_rev(n) || n.ends_with('/') || n.ends_with(".lock") || n.contains("//") || n.starts_with("__forgust") {
        return Err(ApiError::bad(format!("`{name}` is not a valid {kind} name")));
    }
    Ok(n.to_string())
}

fn branch_name(name: &str) -> ApiResult<String> {
    ref_name("branch", "refs/heads/", name)
}

pub fn tag_name(name: &str) -> ApiResult<String> {
    ref_name("tag", "refs/tags/", name)
}

/// `rev` as git should read it: a bare name that is a branch becomes
/// `refs/heads/<name>`, since git itself would prefer a tag of the same name.
/// Anything else (a tag, a commit, `HEAD`, a full ref) is returned unchanged.
/// The commit a ref names (branch, tag, sha or HEAD; none: the default branch),
/// with the ref used. `None` when it names nothing.
pub async fn commit_of(state: &AppState, p: &forgust_db::projects::Project, r: Option<&str>) -> ApiResult<(String, Option<String>)> {
    let r = r.filter(|r| !r.is_empty()).or(p.default_branch.as_deref()).unwrap_or("HEAD").to_string();
    if !validate_rev(&r) {
        return Err(ApiError::bad("invalid ref"));
    }
    let full = qualify(state, p.id, &r).await?;
    let sha = state.storage.resolve(p.id, &full).await?;
    Ok((r, sha))
}

pub async fn qualify(state: &AppState, project_id: i64, rev: &str) -> ApiResult<String> {
    if !rev.starts_with("refs/") && rev != "HEAD" && validate_rev(rev) && head(state, project_id, rev).await?.is_some() {
        return Ok(format!("refs/heads/{rev}"));
    }
    Ok(rev.to_string())
}

async fn head(state: &AppState, project_id: i64, branch: &str) -> ApiResult<Option<String>> {
    Ok(state.storage.resolve(project_id, &format!("refs/heads/{branch}")).await?)
}

/// A push of `updates` must pass the protected-branch rules, as git's would.
async fn allowed(state: &AppState, principal: &Principal, project_id: i64, updates: Vec<crate::hooks::HookUpdate>) -> ApiResult<()> {
    let r = crate::hooks::check_push(state, principal, project_id, &updates, None).await?;
    if r.allowed { Ok(()) } else { Err(ApiError::forbidden(r.message)) }
}

async fn after_push(state: &AppState, principal: &Principal, project_id: i64, updates: Vec<RefUpdate>) -> ApiResult<()> {
    let mut tx = state.db.begin().await?;
    state.jobs.enqueue(&mut tx, PostReceive { project_id, updates, principal: Some(principal.encode()) }.request()).await?;
    tx.commit().await?;
    Ok(())
}

/// Create `name` at `from` (a branch, tag or commit).
pub async fn create_branch(state: &AppState, principal: &Principal, p: &Project, name: &str, from: &str) -> ApiResult<String> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let name = branch_name(name)?;
    if !validate_rev(from) {
        return Err(ApiError::bad("bad starting point"));
    }
    let from = qualify(state, p.id, from).await?;
    let sha = state
        .storage
        .resolve(p.id, &format!("{from}^{{commit}}"))
        .await?
        .ok_or_else(|| ApiError::bad(format!("`{from}` is not a branch, tag or commit here")))?;
    let refname = format!("refs/heads/{name}");
    allowed(
        state,
        principal,
        p.id,
        vec![crate::hooks::HookUpdate { old: ZERO_OID.into(), new: sha.clone(), name: refname.clone(), force: false }],
    )
    .await?;
    {
        let _lock = state.repo_locks.get(p.id).read_owned().await;
        match state.storage.update_ref(p.id, &refname, &sha, Some(ZERO_OID)).await {
            Ok(()) => {}
            Err(StorageError::RefChanged(_)) => return Err(ApiError::conflict(format!("branch `{name}` already exists"))),
            Err(e) => return Err(e.into()),
        }
    }
    after_push(state, principal, p.id, vec![RefUpdate { old: ZERO_OID.into(), new: sha.clone(), name: refname }]).await?;
    super::audit(state, principal, "branch_created", Some(("project", p.id)), json!({ "branch": name, "sha": sha }), None).await;
    Ok(sha)
}

/// Delete a branch (not the default one); protected branches refuse it as a push would.
pub async fn delete_branch(state: &AppState, principal: &Principal, p: &Project, name: &str) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let name = branch_name(name)?;
    if p.default_branch.as_deref() == Some(name.as_str()) {
        return Err(ApiError::bad("the default branch cannot be deleted"));
    }
    let refname = format!("refs/heads/{name}");
    let old = head(state, p.id, &name).await?.ok_or(ApiError::NotFound)?;
    allowed(
        state,
        principal,
        p.id,
        vec![crate::hooks::HookUpdate { old: old.clone(), new: ZERO_OID.into(), name: refname.clone(), force: false }],
    )
    .await?;
    {
        let _lock = state.repo_locks.get(p.id).read_owned().await;
        state.storage.delete_ref(p.id, &refname, Some(&old)).await.map_err(conflict_error)?;
    }
    after_push(state, principal, p.id, vec![RefUpdate { old, new: ZERO_OID.into(), name: refname }]).await?;
    super::audit(state, principal, "branch_deleted", Some(("project", p.id)), json!({ "branch": name }), None).await;
    Ok(())
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedTag {
    pub name: String,
    /// The tag object (annotated) or the commit (lightweight).
    pub target: String,
    pub commit: String,
    pub annotated: bool,
}

/// Create tag `name` at `from`; with a message it is an annotated tag by the caller.
/// Protected tags (`protected_tags`) restrict who creates matching tags, through the push check.
pub async fn create_tag(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    name: &str,
    from: &str,
    message: Option<&str>,
) -> ApiResult<CreatedTag> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    write_tag(state, principal, p, name, from, message).await
}

/// [`create_tag`] once the caller is known to be allowed (releases let CI jobs tag).
pub(crate) async fn write_tag(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    name: &str,
    from: &str,
    message: Option<&str>,
) -> ApiResult<CreatedTag> {
    let name = tag_name(name)?;
    if !validate_rev(from) {
        return Err(ApiError::bad("bad starting point"));
    }
    let from = qualify(state, p.id, from).await?;
    let commit = state
        .storage
        .resolve(p.id, &format!("{from}^{{commit}}"))
        .await?
        .ok_or_else(|| ApiError::bad(format!("`{from}` is not a branch, tag or commit here")))?;
    let refname = format!("refs/tags/{name}");
    if state.storage.resolve(p.id, &refname).await?.is_some() {
        return Err(ApiError::conflict(format!("tag `{name}` already exists")));
    }
    let message = message.map(str::trim).filter(|m| !m.is_empty());
    let target = match message {
        Some(m) => {
            if m.len() > 100_000 {
                return Err(ApiError::bad("the tag message is too long"));
            }
            let user = principal.user().ok_or(ApiError::Unauthorized)?;
            state.storage.tag_object(p.id, &name, &commit, m, &identity(user)).await?
        }
        None => commit.clone(),
    };
    allowed(
        state,
        principal,
        p.id,
        vec![crate::hooks::HookUpdate { old: ZERO_OID.into(), new: target.clone(), name: refname.clone(), force: false }],
    )
    .await?;
    {
        let _lock = state.repo_locks.get(p.id).read_owned().await;
        match state.storage.update_ref(p.id, &refname, &target, Some(ZERO_OID)).await {
            Ok(()) => {}
            Err(StorageError::RefChanged(_)) => return Err(ApiError::conflict(format!("tag `{name}` already exists"))),
            Err(e) => return Err(e.into()),
        }
    }
    after_push(state, principal, p.id, vec![RefUpdate { old: ZERO_OID.into(), new: target.clone(), name: refname }]).await?;
    super::audit(state, principal, "tag_created", Some(("project", p.id)), json!({ "tag": name, "commit": commit }), None).await;
    Ok(CreatedTag { name, annotated: target != commit, target, commit })
}

/// Delete a tag (a protected one: Maintainers only); its release goes with it
/// (as GitLab), when the push is processed.
pub async fn delete_tag(state: &AppState, principal: &Principal, p: &Project, name: &str) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let name = tag_name(name)?;
    let refname = format!("refs/tags/{name}");
    // the ref's own value (the tag object when annotated), not the peeled commit
    let old =
        state.storage.refs(p.id, &refname).await?.into_iter().find(|r| r.name == refname).map(|r| r.target).ok_or(ApiError::NotFound)?;
    // a protected tag cannot be deleted by pushing; here, by Maintainers
    let protected = forgust_db::protected_branches::tags(&state.db, p.id).await?.iter().any(|r| r.matches(&name));
    if protected {
        require_project(state, principal, ProjectAction::Admin, p)
            .await
            .map_err(|_| ApiError::forbidden(format!("{name} is a protected tag: only Maintainers can delete it")))?;
    } else {
        allowed(
            state,
            principal,
            p.id,
            vec![crate::hooks::HookUpdate { old: old.clone(), new: ZERO_OID.into(), name: refname.clone(), force: false }],
        )
        .await?;
    }
    {
        let _lock = state.repo_locks.get(p.id).read_owned().await;
        state.storage.delete_ref(p.id, &refname, Some(&old)).await.map_err(conflict_error)?;
    }
    after_push(state, principal, p.id, vec![RefUpdate { old, new: ZERO_OID.into(), name: refname }]).await?;
    super::audit(state, principal, "tag_deleted", Some(("project", p.id)), json!({ "tag": name }), None).await;
    Ok(())
}

fn decode(a: &FileAction) -> ApiResult<Option<Vec<u8>>> {
    let Some(c) = &a.content else { return Ok(None) };
    let bytes = match a.encoding.as_deref() {
        None | Some("text") => c.clone().into_bytes(),
        Some("base64") => base64::engine::general_purpose::STANDARD
            .decode(c)
            .map_err(|_| ApiError::bad(format!("{}: content is not base64", a.file_path)))?,
        Some(other) => return Err(ApiError::bad(format!("unknown encoding `{other}`"))),
    };
    if bytes.len() > MAX_FILE {
        return Err(ApiError::bad(format!("{} is larger than 10 MB", a.file_path)));
    }
    Ok(Some(bytes))
}

async fn exists(state: &AppState, project_id: i64, commit: &str, path: &str) -> ApiResult<bool> {
    match state.storage.blob(project_id, commit, path, 0).await {
        Ok(_) => Ok(true),
        Err(StorageError::PathNotFound(_)) | Err(StorageError::RevisionNotFound(_)) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// One commit of file changes on `branch` (created from `start_branch` when missing).
pub async fn commit(state: &AppState, principal: &Principal, p: &Project, req: CommitRequest<'_>) -> ApiResult<Committed> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    let branch = branch_name(req.branch)?;
    let message = req.message.trim();
    if message.is_empty() || message.len() > 10_000 {
        return Err(ApiError::bad("a commit message of 1-10,000 characters is required"));
    }
    if req.actions.is_empty() || req.actions.len() > MAX_ACTIONS {
        return Err(ApiError::bad(format!("1-{MAX_ACTIONS} file actions are needed")));
    }
    let current = head(state, p.id, &branch).await?;
    if let Some(expected) = req.expected_head
        && current.as_deref() != Some(expected)
    {
        return Err(ApiError::conflict(format!("`{branch}` changed since you opened it: reload to see the new version")));
    }
    let start_point = |start: &str| {
        let start = start.to_string();
        async move {
            if !validate_rev(&start) {
                return Err(ApiError::bad("bad start_branch"));
            }
            let start = qualify(state, p.id, &start).await?;
            state
                .storage
                .resolve(p.id, &format!("{start}^{{commit}}"))
                .await?
                .ok_or_else(|| ApiError::bad(format!("`{start}` does not exist")))
        }
    };
    // a new branch from elsewhere onto an existing name would build on content
    // the author never saw: refused, unless asked to replace it (as GitLab)
    let elsewhere = req.start_branch.filter(|s| s.trim_start_matches("refs/heads/") != branch);
    let (base, force) = match (&current, elsewhere) {
        (Some(_), Some(start)) if req.force => (start_point(start).await?, true),
        (Some(_), Some(_)) => return Err(ApiError::bad(format!("a branch called `{branch}` already exists"))),
        (Some(h), None) => (h.clone(), false),
        (None, _) => {
            let start = req.start_branch.or(p.default_branch.as_deref()).ok_or_else(|| ApiError::bad("start_branch is required"))?;
            (start_point(start).await?, false)
        }
    };

    let mut changes = Vec::new();
    for a in req.actions {
        if !validate_path(&a.file_path) {
            return Err(ApiError::bad(format!("`{}` is not a valid path", a.file_path)));
        }
        let content = decode(a)?;
        let here = exists(state, p.id, &base, &a.file_path).await?;
        match a.action {
            ActionKind::Create => {
                if here {
                    return Err(ApiError::conflict(format!("{} already exists", a.file_path)));
                }
                changes.push(FileChange { path: a.file_path.clone(), content: Some(content.unwrap_or_default()) });
            }
            ActionKind::Update => {
                if !here {
                    return Err(ApiError::conflict(format!("{} does not exist", a.file_path)));
                }
                let c = content.ok_or_else(|| ApiError::bad(format!("{}: content is required", a.file_path)))?;
                changes.push(FileChange { path: a.file_path.clone(), content: Some(c) });
            }
            ActionKind::Delete => {
                if !here {
                    return Err(ApiError::conflict(format!("{} does not exist", a.file_path)));
                }
                changes.push(FileChange { path: a.file_path.clone(), content: None });
            }
            ActionKind::Move => {
                let from = a.previous_path.as_deref().ok_or_else(|| ApiError::bad("a move needs previous_path"))?;
                if !validate_path(from) || !exists(state, p.id, &base, from).await? {
                    return Err(ApiError::conflict(format!("{from} does not exist")));
                }
                if here {
                    return Err(ApiError::conflict(format!("{} already exists", a.file_path)));
                }
                let c = match content {
                    Some(c) => c,
                    None => {
                        let b = state.storage.blob(p.id, &base, from, MAX_FILE).await?;
                        if b.truncated {
                            return Err(ApiError::bad(format!("{from} is too large to move here")));
                        }
                        b.data
                    }
                };
                changes.push(FileChange { path: from.to_string(), content: None });
                changes.push(FileChange { path: a.file_path.clone(), content: Some(c) });
            }
        }
    }

    let tree = state.storage.tree_with_changes(p.id, &base, &changes).await?;
    let sha = state.storage.commit_tree(p.id, &tree, std::slice::from_ref(&base), message, &identity(&user), &committer(state)).await?;
    // an empty commit (content unchanged) is refused; the object is left for gc
    let diff = state
        .storage
        .diff(p.id, Some(&base), &sha, forgust_storage::DiffOptions { offset: 0, limit: 1, max_lines_per_file: 1, paths: None })
        .await?;
    if diff.total_files == 0 {
        return Err(ApiError::bad("nothing changed"));
    }
    let refname = format!("refs/heads/{branch}");
    let old = current.clone().unwrap_or_else(|| ZERO_OID.to_string());
    allowed(state, principal, p.id, vec![crate::hooks::HookUpdate { old: old.clone(), new: sha.clone(), name: refname.clone(), force }])
        .await?;
    {
        let _lock = state.repo_locks.get(p.id).read_owned().await;
        state.storage.update_ref(p.id, &refname, &sha, Some(&old)).await.map_err(conflict_error)?;
    }
    after_push(state, principal, p.id, vec![RefUpdate { old, new: sha.clone(), name: refname }]).await?;
    super::audit(
        state,
        principal,
        "web_commit",
        Some(("project", p.id)),
        json!({ "branch": branch, "sha": sha, "files": req.actions.len() }),
        None,
    )
    .await;
    Ok(Committed { id: sha, branch, created_branch: current.is_none() })
}
