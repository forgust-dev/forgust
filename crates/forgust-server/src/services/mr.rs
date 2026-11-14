//! Merge requests (M3): versions, merge checks, server-side merges, note positions.
//!
//! Server-made ref updates bypass the pre-receive hook, so this module enforces the
//! protected-branch merge rules itself, holds the repo lock like a push, updates the
//! target with compare-and-swap, and enqueues the same post-receive job a push does.

use forgust_db::access::DEVELOPER;
use forgust_db::merge_requests::{self as mrdb, MergeRequest, MrDiff, NewDiff, NewMergeRequest, Note, Position};
use forgust_db::projects::Project;
use forgust_db::users::User;
use forgust_storage::{DiffOptions, Identity, StorageError, ZERO_OID, map_old_line_to_new};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::audit;
use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, project_role, require_project};
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::jobs::{PostReceive, RefUpdate};
use crate::state::AppState;

pub fn head_ref(iid: i64) -> String {
    format!("refs/merge-requests/{iid}/head")
}

pub(crate) fn keep_around(sha: &str) -> String {
    format!("refs/keep-around/{sha}")
}

pub(crate) fn committer(state: &AppState) -> Identity {
    Identity { name: state.cfg.server.commit_name.clone(), email: state.cfg.server.commit_email.clone() }
}

/// A squashed merge request: its message (title and description) and author
/// (the MR's author, else `fallback`).
pub(crate) async fn squash_parts(state: &AppState, mr: &MergeRequest, fallback: &Identity) -> ApiResult<(String, Identity)> {
    let author = match mr.author_id {
        Some(id) => forgust_db::users::find(&state.db, id).await?.map(|u| identity(&u)).unwrap_or(fallback.clone()),
        None => fallback.clone(),
    };
    let msg = if mr.description.trim().is_empty() { mr.title.clone() } else { format!("{}\n\n{}", mr.title, mr.description.trim()) };
    Ok((msg, author))
}

pub(crate) fn identity(u: &User) -> Identity {
    Identity { name: if u.name.is_empty() { u.username.clone() } else { u.name.clone() }, email: u.email.clone() }
}

async fn branch_head(state: &AppState, project_id: i64, branch: &str) -> ApiResult<Option<String>> {
    if !forgust_storage::validate_rev(branch) {
        return Err(ApiError::bad(format!("invalid branch name `{branch}`")));
    }
    Ok(state.storage.resolve(project_id, &format!("refs/heads/{branch}")).await?)
}

/// Keep a commit reachable so comments pointing at it survive force pushes and gc.
pub(crate) async fn pin(state: &AppState, project_id: i64, sha: &str) {
    match state.storage.update_ref(project_id, &keep_around(sha), sha, Some(ZERO_OID)).await {
        Ok(()) | Err(StorageError::RefChanged(_)) => {}
        Err(e) => tracing::warn!(project_id, sha, error = %e, "keep-around ref not written"),
    }
}

// ------------------------------------------------------------------ create and versions

pub struct CreateMr<'a> {
    pub source_branch: &'a str,
    pub target_branch: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub draft: bool,
    pub squash: bool,
    /// Stack on this open MR (the target must be its source branch).
    pub stack_on: Option<i64>,
    /// Delete the source branch when merging.
    pub remove_source_branch: bool,
}

pub async fn create(state: &AppState, principal: &Principal, p: &Project, req: CreateMr<'_>) -> ApiResult<MergeRequest> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    if req.title.trim().is_empty() || req.title.len() > 255 {
        return Err(ApiError::bad("a title of 1-255 characters is required"));
    }
    if req.source_branch == req.target_branch {
        return Err(ApiError::bad("source and target branch must differ"));
    }
    let source = branch_head(state, p.id, req.source_branch)
        .await?
        .ok_or_else(|| ApiError::bad(format!("branch `{}` does not exist", req.source_branch)))?;
    let target = branch_head(state, p.id, req.target_branch)
        .await?
        .ok_or_else(|| ApiError::bad(format!("branch `{}` does not exist", req.target_branch)))?;
    if let Some(parent) = req.stack_on {
        super::stack::precheck(state, p, parent, req.target_branch).await?;
    }
    if let Some(iid) = mrdb::open_exists(&state.db, p.id, req.source_branch, req.target_branch).await? {
        return Err(ApiError::conflict(format!("merge request !{iid} is already open for these branches")));
    }
    if state.storage.merge_base(p.id, &target, &source).await?.is_none() {
        return Err(ApiError::bad("the branches have no common history"));
    }

    let mut tx = state.db.begin().await?;
    let iid = mrdb::next_iid(&mut *tx, p.id, "merge_requests").await?;
    let id = mrdb::insert(
        &mut *tx,
        NewMergeRequest {
            project_id: p.id,
            iid,
            title: req.title.trim(),
            description: req.description,
            author_id: user.id,
            source_branch: req.source_branch,
            target_branch: req.target_branch,
            draft: req.draft || is_draft_title(req.title),
            squash: req.squash,
            remove_source_branch: req.remove_source_branch,
        },
    )
    .await?;
    tx.commit().await?;
    state.storage.update_ref(p.id, &head_ref(iid), &source, None).await?;
    add_version(state, p.id, id, iid, &target, &source).await?;

    let mut mr = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    if let Some(parent) = req.stack_on {
        super::stack::stack_on(state, principal, p, &mr, parent).await?;
        mr = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    }
    audit(state, principal, "merge_request_opened", Some(("project", p.id)), json!({ "iid": iid }), None).await;
    after_change(state, p, &mr, "open", Some(&user)).await;
    super::refs::on_write(state, p, &user, mr_target(&mr), &mr.description, None).await;
    super::notify::new_merge_request(state, p, &mr, &user).await;
    // a merge request pipeline, when the config has jobs for them, and when the
    // author may run pipelines on the source (a protected one has protected variables)
    if crate::ci::may_run(state, principal, p, &mr.source_branch, false).await.unwrap_or(false)
        && let Err(e) = crate::ci::on_merge_request(state, p.id, &mr, user.id).await
    {
        tracing::warn!(project_id = p.id, iid = mr.iid, error = %e, "merge request pipeline not created");
    }
    Ok(mr)
}

/// GitLab's draft prefixes.
pub(crate) fn is_draft_title(t: &str) -> bool {
    let t = t.trim_start().to_ascii_lowercase();
    ["draft:", "[draft]", "(draft)", "wip:", "[wip]"].iter().any(|p| t.starts_with(p))
}

/// What an edit of an MR may change (absent: unchanged).
#[derive(Default)]
pub struct DetailsChange<'a> {
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    /// Absent: a new title's draft prefix decides, else unchanged.
    pub draft: Option<bool>,
    pub target_branch: Option<&'a str>,
}

/// Edit title, description, draft and target (the author or a Developer): a
/// new target means a new version against it; mentions in a new description
/// are announced; a pending auto-merge is checked again.
pub async fn update_details(state: &AppState, principal: &Principal, p: &Project, m: &MergeRequest, c: DetailsChange<'_>) -> ApiResult<()> {
    may_edit(state, principal, p, m).await?;
    let title = c.title.unwrap_or(&m.title).trim();
    if title.is_empty() {
        return Err(ApiError::bad("the title cannot be empty"));
    }
    let draft = c.draft.unwrap_or_else(|| if c.title.is_some() { is_draft_title(title) } else { m.draft });
    let target = c.target_branch.unwrap_or(&m.target_branch);
    let retarget = target != m.target_branch;
    if retarget
        && (m.state != "opened"
            || target == m.source_branch
            || state.storage.resolve(p.id, &format!("refs/heads/{target}")).await?.is_none())
    {
        return Err(ApiError::bad("invalid target branch"));
    }
    if retarget && m.stack_id.is_some() {
        return Err(ApiError::bad("the merge request is in a stack: remove it from the stack to change its target"));
    }
    let description = c.description.unwrap_or(&m.description);
    mrdb::update_details(&state.db, m.id, title, description, draft, target).await?;
    if let Some(user) = principal.direct_user()
        && description != m.description
    {
        super::refs::on_write(state, p, user, mr_target(m), description, Some(&m.description)).await;
        // a ticked box (rebase, retry) in a Renovate merge request
        super::renovate::on_description_edit(state, p.id, m.author_id, user, &m.description, description).await;
    }
    if retarget {
        let head = state.storage.resolve(p.id, &head_ref(m.iid)).await?.unwrap_or_default();
        on_push(state, p.id, &[crate::jobs::RefUpdate { old: head.clone(), new: head, name: format!("refs/heads/{}", m.source_branch) }])
            .await
            .map_err(ApiError::Internal)?;
    }
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: m.iid });
    // marking ready, or a new target, may let a pending auto-merge go (or cancel it)
    super::auto_merge::poke(state, p.id, m.iid).await;
    Ok(())
}

/// Record a version for (target head, source head) unless it is the latest already.
pub(crate) async fn add_version(
    state: &AppState,
    project_id: i64,
    mr_id: i64,
    iid: i64,
    start: &str,
    head: &str,
) -> ApiResult<Option<MrDiff>> {
    let base =
        state.storage.merge_base(project_id, start, head).await?.ok_or_else(|| ApiError::bad("the branches have no common history"))?;
    let commits = state.storage.rev_list(project_id, &base, head, false).await?.len() as i32;
    let stats =
        state.storage.diff(project_id, Some(&base), head, DiffOptions { offset: 0, limit: 1, max_lines_per_file: 1, paths: None }).await?;
    let mut tx = state.db.begin().await?;
    mrdb::lock(&mut tx, mr_id).await?;
    let latest = mrdb::latest_diff(&mut *tx, mr_id).await?;
    if latest.as_ref().is_some_and(|d| d.head_sha == head && d.start_sha == start) {
        return Ok(None);
    }
    let d = mrdb::add_diff(
        &mut tx,
        mr_id,
        NewDiff {
            base_sha: &base,
            start_sha: start,
            head_sha: head,
            commits_count: commits,
            files_count: stats.total_files as i32,
            additions: stats.additions as i32,
            deletions: stats.deletions as i32,
        },
        None,
    )
    .await?;
    if let Some(prev) = &latest {
        if prev.head_sha != head {
            // new commits invalidate approvals (documented API behavior)
            mrdb::reset_approvals(&mut *tx, mr_id).await?;
        }
        let msg = format!("added version {} ({} commit{})", d.version, commits, if commits == 1 { "" } else { "s" });
        mrdb::add_note(&mut *tx, mr_id, None, None, &msg, true, Position::default()).await?;
    }
    mrdb::touch(&mut *tx, mr_id).await?;
    tx.commit().await?;
    for sha in [head, start, base.as_str()] {
        pin(state, project_id, sha).await;
    }
    state.storage.update_ref(project_id, &head_ref(iid), head, None).await?;
    Ok(Some(d))
}

/// After a push (worker): new versions for MRs whose source branch moved.
/// Safe to repeat and to run late: the version is for the branch as it is now
/// (an older push's job never makes an older head the latest version), and
/// `add_version` skips one that is already the latest.
pub async fn on_push(state: &AppState, project_id: i64, updates: &[RefUpdate]) -> anyhow::Result<()> {
    let mut project: Option<forgust_db::projects::Project> = None;
    for u in updates {
        let Some(branch) = u.name.strip_prefix("refs/heads/") else { continue };
        if u.new == ZERO_OID {
            continue;
        }
        let Some(head) = branch_head(state, project_id, branch).await? else { continue };
        // stacked layers above this branch: their map shows whether they need a rebase
        for above in mrdb::open_with_target(&state.db, project_id, branch).await? {
            if above.stack_id.is_some() {
                state.events.publish(Event::MergeRequest { project_id, iid: above.iid });
            }
        }
        for mr in mrdb::open_with_source(&state.db, project_id, branch).await? {
            let Some(start) = branch_head(state, project_id, &mr.target_branch).await? else { continue };
            // rebased onto the branch below by hand: its own commits start there now
            if mr.stack_id.is_some()
                && mr.stack_base_sha.as_deref() != Some(start.as_str())
                && state.storage.is_ancestor(project_id, &start, &head).await?
            {
                mrdb::set_stack_base(&state.db, mr.id, &start).await?;
            }
            // new commits cancel a pending auto-merge (the check sees the new head)
            super::auto_merge::poke(state, project_id, mr.iid).await;
            if let Some(d) = add_version(state, project_id, mr.id, mr.iid, &start, &head).await? {
                state.events.publish(Event::MergeRequest { project_id, iid: mr.iid });
                // the project for the webhook: loaded once, at the first new version
                if project.is_none() {
                    project = forgust_db::projects::find(&state.db, project_id).await?;
                }
                if let Some(p) = &project {
                    super::webhooks::emit(state, p, "merge_request", merge_request_payload(state, p, &mr, "update", None, Some(&d))).await;
                }
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ merge status

#[derive(Debug, Serialize, ToSchema)]
pub struct RuleStatus {
    pub name: String,
    pub required: i16,
    pub min_access_level: String,
    pub given: i64,
    pub satisfied: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MergeStatus {
    /// True when every check passes and the viewer may merge.
    pub can_merge: bool,
    /// Human-readable reasons it cannot be merged yet.
    pub blockers: Vec<String>,
    pub conflicts: Vec<String>,
    /// The target is an ancestor of the source: fast-forward is possible.
    pub fast_forward: bool,
    pub approval_rules: Vec<RuleStatus>,
    pub unresolved_threads: i64,
    /// Source head the status was computed for; send it back when merging.
    pub head_sha: Option<String>,
    pub target_sha: Option<String>,
    pub user_can_merge: bool,
    /// Status of the newest pipeline for the source head (MR or branch pipeline).
    pub pipeline_status: Option<String>,
    pub pipeline_required: bool,
    /// The project merges through merge trains.
    pub merge_trains_enabled: bool,
    /// Place on the merge train (1 merges next), when on one.
    pub train_position: Option<i64>,
}

/// Whether `user` may merge into `target_branch` (protected-branch merge rules).
async fn may_merge_into(state: &AppState, principal: &Principal, p: &Project, target_branch: &str) -> ApiResult<bool> {
    branch_rights(state, principal, p, target_branch, false).await
}

/// Developer+ and, on a protected branch, a matching rule's merge level (or,
/// with `or_push`, its push level). Level 0 means "no one".
pub async fn branch_rights(state: &AppState, principal: &Principal, p: &Project, branch: &str, or_push: bool) -> ApiResult<bool> {
    // nothing merges into, or runs on, an archived project
    if p.archived() {
        return Ok(false);
    }
    let role = project_role(state, principal, p).await?;
    if principal.direct_user().is_none() || role.level < DEVELOPER {
        return Ok(false);
    }
    let rules = forgust_db::protected_branches::list(&state.db, p.id).await?;
    let matching = forgust_db::protected_branches::matching(&rules, branch);
    if matching.is_empty() {
        return Ok(true);
    }
    let allows = |lvl: i16| lvl > 0 && role.level >= lvl.max(DEVELOPER);
    Ok(matching.iter().any(|r| allows(r.merge_access_level) || (or_push && allows(r.push_access_level))))
}

pub async fn merge_status(state: &AppState, principal: &Principal, p: &Project, mr: &MergeRequest) -> ApiResult<MergeStatus> {
    let mut blockers = Vec::new();
    let mut conflicts = Vec::new();
    let mut fast_forward = false;
    let head = branch_head(state, p.id, &mr.source_branch).await?;
    let target = branch_head(state, p.id, &mr.target_branch).await?;
    if mr.state != "opened" {
        blockers.push(format!("the merge request is {}", mr.state));
    }
    if mr.draft {
        blockers.push("the merge request is a draft".into());
    }
    if let Some(b) = super::stack::merge_blocker(state, mr).await? {
        blockers.push(b);
    }
    match (&head, &target) {
        (None, _) => blockers.push(format!("source branch `{}` no longer exists", mr.source_branch)),
        (_, None) => blockers.push(format!("target branch `{}` no longer exists", mr.target_branch)),
        (Some(h), Some(t)) => {
            fast_forward = state.storage.is_ancestor(p.id, t, h).await?;
            if !fast_forward {
                let m = state.storage.merge_tree(p.id, t, h, None).await?;
                if !m.clean() {
                    blockers.push("there are merge conflicts".into());
                    conflicts = m.conflicts;
                }
            }
        }
    }

    let approvals = mrdb::approvals(&state.db, mr.id, p.id).await?;
    let mut approval_rules = Vec::new();
    for r in mrdb::rules(&state.db, p.id).await? {
        let given = approvals.iter().filter(|a| a.access_level >= r.min_access_level).count() as i64;
        let satisfied = given >= r.approvals_required as i64;
        if !satisfied {
            blockers.push(format!("approval rule `{}` needs {} approval(s), has {given}", r.name, r.approvals_required));
        }
        approval_rules.push(RuleStatus {
            name: r.name,
            required: r.approvals_required,
            min_access_level: forgust_db::access::name(r.min_access_level).into(),
            given,
            satisfied,
        });
    }
    // code owners, when the target branch requires their approval
    let protected = forgust_db::protected_branches::list(&state.db, p.id).await?;
    if forgust_db::protected_branches::matching(&protected, &mr.target_branch).iter().any(|r| r.code_owner_approval_required) {
        for s in super::codeowners::for_merge_request(state, p, mr).await?.sections.into_iter().filter(|s| !s.optional) {
            let name = if s.name.is_empty() { "Code owners".to_string() } else { format!("Code owners: {}", s.name) };
            if !s.satisfied {
                let who = s.owners.iter().map(|o| format!("@{o}")).collect::<Vec<_>>().join(", ");
                blockers.push(format!("{name} must approve ({who})"));
            }
            approval_rules.push(RuleStatus {
                name,
                required: s.approvals_required as i16,
                min_access_level: "code owner".into(),
                given: s.approved_by.len() as i64,
                satisfied: s.satisfied,
            });
        }
    }
    for (_, r) in forgust_db::people::mr_reviewers(&state.db, &[mr.id]).await? {
        if r.state == "requested_changes" {
            blockers.push(format!("@{} requested changes", r.user.username));
        }
    }
    let unresolved_threads = mrdb::unresolved_threads(&state.db, mr.id).await?;
    // "pipelines must succeed": the newest pipeline for the source head
    let mut pipeline_status = None;
    if let Some(h) = &head {
        pipeline_status = forgust_db::ci::latest_status_for_sha(&state.db, p.id, h, mr.id).await?;
    }
    // on a merge train, the train's pipeline is the one that counts
    if let Some(car) = forgust_db::merge_trains::of_mr(&state.db, mr.id).await?
        && car.pipeline_id.is_some()
    {
        pipeline_status = car.pipeline_status;
    }
    let settings = forgust_db::ci::settings(&state.db, p.id).await?;
    if settings.only_merge_if_pipeline_succeeds {
        match pipeline_status.as_deref() {
            Some("success") => {}
            Some(s) => blockers.push(format!("the pipeline must succeed (it is {s})")),
            None => blockers.push("the pipeline must succeed (no pipeline ran for the latest commit)".into()),
        }
    }
    // No new critical or high security finding left undismissed
    if settings.block_new_security_findings && mr.state == "opened" {
        let found = super::security::for_merge_request(state, p, mr).await.map_err(ApiError::Internal)?;
        let n = found.blocking();
        if n > 0 {
            blockers.push(format!("{n} new critical or high security finding(s) must be fixed or dismissed"));
        }
    }
    if p.archived() {
        blockers.push("the project is archived (read-only)".into());
    }
    let user_can_merge = may_merge_into(state, principal, p, &mr.target_branch).await?;
    if !user_can_merge {
        blockers.push(format!("you are not allowed to merge into `{}`", mr.target_branch));
    }
    let train_position = match forgust_db::merge_trains::of_mr(&state.db, mr.id).await? {
        Some(car) => forgust_db::merge_trains::cars(&state.db, p.id, Some(&car.target_branch))
            .await?
            .iter()
            .position(|c| c.id == car.id)
            .map(|i| i as i64 + 1),
        None => None,
    };
    Ok(MergeStatus {
        can_merge: blockers.is_empty(),
        pipeline_status,
        pipeline_required: settings.only_merge_if_pipeline_succeeds,
        merge_trains_enabled: settings.merge_trains_enabled,
        train_position,
        blockers,
        conflicts,
        fast_forward,
        approval_rules,
        unresolved_threads,
        head_sha: head,
        target_sha: target,
        user_can_merge,
    })
}

// ------------------------------------------------------------------ merge

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    /// A merge commit with both parents.
    Merge,
    /// All changes as one commit on the target.
    Squash,
    /// Move the target to the source head (only when possible).
    FastForward,
    /// Replay the source commits onto the target, then fast-forward.
    Rebase,
}

pub struct MergeOptions<'a> {
    pub method: MergeMethod,
    /// The source head the user reviewed; refuse when the branch moved since.
    pub expected_head: Option<&'a str>,
    pub remove_source_branch: bool,
    pub message: Option<&'a str>,
    /// A merge train's tested commit and the target it was built on: the
    /// target moves to exactly that commit.
    pub train: Option<(&'a str, &'a str)>,
}

pub async fn merge(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    mr: &MergeRequest,
    opts: MergeOptions<'_>,
) -> ApiResult<MergeRequest> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    let status = merge_status(state, principal, p, mr).await?;
    if !status.can_merge {
        return Err(ApiError::conflict(status.blockers.join("; ")));
    }
    let head = status.head_sha.clone().ok_or(ApiError::NotFound)?;
    if opts.expected_head.is_some_and(|e| e != head) {
        return Err(ApiError::conflict("the source branch changed since you loaded the page; review the new changes first"));
    }

    // a merge is a push to the target: share the repo lock with pushes
    let _lock = state.repo_locks.get(p.id).read_owned().await;
    // the source may have moved between the status check and the lock
    if branch_head(state, p.id, &mr.source_branch).await?.as_deref() != Some(head.as_str()) {
        return Err(ApiError::conflict("the source branch changed while merging; review the new changes first"));
    }
    let target_old = branch_head(state, p.id, &mr.target_branch).await?.ok_or(ApiError::NotFound)?;
    let author = identity(&user);
    let bot = committer(state);
    let target_ref = format!("refs/heads/{}", mr.target_branch);
    let default_msg =
        format!("Merge branch '{}' into '{}'\n\n{}\n\nSee merge request !{}", mr.source_branch, mr.target_branch, mr.title, mr.iid);

    let new_target = if let Some((train, base)) = opts.train {
        if target_old != base {
            return Err(ApiError::conflict("the target branch moved: the merge train is rebuilt"));
        }
        train.to_string()
    } else {
        match opts.method {
            MergeMethod::Merge => {
                let m = state.storage.merge_tree(p.id, &target_old, &head, None).await?;
                if !m.clean() {
                    return Err(ApiError::conflict(format!("merge conflicts in {}", m.conflicts.join(", "))));
                }
                let msg = opts.message.unwrap_or(&default_msg);
                state.storage.commit_tree(p.id, &m.tree, &[target_old.clone(), head.clone()], msg, &author, &bot).await?
            }
            MergeMethod::Squash => {
                let m = state.storage.merge_tree(p.id, &target_old, &head, None).await?;
                if !m.clean() {
                    return Err(ApiError::conflict(format!("merge conflicts in {}", m.conflicts.join(", "))));
                }
                let (squash_msg, mr_author) = squash_parts(state, mr, &author).await?;
                let msg = opts.message.unwrap_or(&squash_msg);
                state.storage.commit_tree(p.id, &m.tree, std::slice::from_ref(&target_old), msg, &mr_author, &bot).await?
            }
            MergeMethod::FastForward => {
                if !state.storage.is_ancestor(p.id, &target_old, &head).await? {
                    return Err(ApiError::conflict("fast-forward is not possible: rebase the source branch first"));
                }
                head.clone()
            }
            MergeMethod::Rebase => {
                if state.storage.is_ancestor(p.id, &target_old, &head).await? {
                    head.clone()
                } else {
                    let upstream =
                        state.storage.merge_base(p.id, &target_old, &head).await?.ok_or_else(|| ApiError::bad("no common history"))?;
                    // replay what was checked under the lock (the MR head ref only
                    // moves when the push worker runs, so it can lag)
                    let rebased = state.storage.rebase(p.id, &target_old, &upstream, &head, &bot).await.map_err(conflict_error)?;
                    may_rewrite(state, principal, p, &mr.source_branch, &head, &rebased).await?;
                    // the source branch follows its rebased commits (CAS on what we reviewed)
                    state
                        .storage
                        .update_ref(p.id, &format!("refs/heads/{}", mr.source_branch), &rebased, Some(&head))
                        .await
                        .map_err(conflict_error)?;
                    rebased
                }
            }
        }
    };

    state.storage.update_ref(p.id, &target_ref, &new_target, Some(&target_old)).await.map_err(conflict_error)?;
    pin(state, p.id, &head).await;
    // the next layer of a stack now targets the trunk (ADR-0019)
    let next = super::stack::after_merge(state, principal, p, mr, &head, &new_target).await;
    let mut tx = state.db.begin().await?;
    mrdb::set_merged(&mut *tx, mr.id, &new_target, user.id).await?;
    mrdb::add_note(
        &mut *tx,
        mr.id,
        Some(user.id),
        None,
        &format!("merged into `{}` ({})", mr.target_branch, &new_target[..8]),
        true,
        Position::default(),
    )
    .await?;
    let mut updates = vec![RefUpdate { old: target_old.clone(), new: new_target.clone(), name: target_ref.clone() }];
    if let Some((_, ups, _)) = &next {
        updates.extend(ups.iter().cloned());
    }
    // a branch other open MRs still target stays (they would lose their base)
    let still_used = mrdb::open_with_target(&state.db, p.id, &mr.source_branch)
        .await?
        .iter()
        .any(|o| next.as_ref().is_none_or(|(n, _, _)| n.id != o.id) && o.id != mr.id);
    if opts.remove_source_branch && !still_used {
        let source_ref = format!("refs/heads/{}", mr.source_branch);
        let current = branch_head(state, p.id, &mr.source_branch).await?;
        let protected = forgust_db::protected_branches::list(&state.db, p.id).await?.iter().any(|r| r.matches(&mr.source_branch));
        if let Some(cur) = current.filter(|_| !protected)
            && state.storage.delete_ref(p.id, &source_ref, Some(&cur)).await.is_ok()
        {
            updates.push(RefUpdate { old: cur, new: ZERO_OID.into(), name: source_ref });
        }
    }
    // same follow-up a push gets (events, default branch, maintenance, webhooks)
    state.jobs.enqueue(&mut tx, PostReceive { project_id: p.id, updates, principal: Some(principal.encode()) }.request()).await?;
    tx.commit().await?;

    let merged = mrdb::find(&state.db, p.id, mr.iid).await?.ok_or(ApiError::NotFound)?;
    if let Some((n, _, n_head)) = &next {
        if let Err(e) = add_version(state, p.id, n.id, n.iid, &new_target, n_head).await {
            tracing::warn!(iid = n.iid, error = %e, "stack: no new version after retarget");
        }
        state.events.publish(Event::MergeRequest { project_id: p.id, iid: n.iid });
        // the next layer may have been waiting for this one to auto-merge
        super::auto_merge::poke(state, p.id, n.iid).await;
    }
    audit(
        state,
        principal,
        "merge_request_merged",
        Some(("project", p.id)),
        json!({ "iid": mr.iid, "method": opts.method, "sha": new_target }),
        None,
    )
    .await;
    after_change(state, p, &merged, "merge", Some(&user)).await;
    done(state, user.id, p, mr).await;
    if p.default_branch.as_deref() == Some(mr.target_branch.as_str()) {
        super::refs::close_issues(state, p, &user, mr.iid, &mr.description).await;
    }
    Ok(merged)
}

pub(crate) fn conflict_error(e: StorageError) -> ApiError {
    match e {
        StorageError::Conflict(paths) => ApiError::conflict(format!("conflicts while rebasing: {}", paths.join(", "))),
        StorageError::RefChanged(r) => ApiError::conflict(format!("{r} moved during the merge; try again")),
        other => other.into(),
    }
}

/// Moving `branch` from `old` to `new` without a fast-forward is a force push:
/// the protected-branch rules a `git push --force` meets apply (server-side
/// ref updates skip the pre-receive hook).
pub(crate) async fn may_rewrite(state: &AppState, principal: &Principal, p: &Project, branch: &str, old: &str, new: &str) -> ApiResult<()> {
    let u = crate::hooks::HookUpdate { old: old.into(), new: new.into(), name: format!("refs/heads/{branch}"), force: true };
    let r = crate::hooks::check_push(state, principal, p.id, &[u], None).await?;
    if r.allowed { Ok(()) } else { Err(ApiError::forbidden(r.message)) }
}

/// Rebase the source branch onto the target without merging.
pub async fn rebase_source(state: &AppState, principal: &Principal, p: &Project, mr: &MergeRequest) -> ApiResult<String> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    if mr.state != "opened" {
        return Err(ApiError::conflict("only open merge requests can be rebased"));
    }
    let _lock = state.repo_locks.get(p.id).read_owned().await;
    let head = branch_head(state, p.id, &mr.source_branch).await?.ok_or(ApiError::NotFound)?;
    let target = branch_head(state, p.id, &mr.target_branch).await?.ok_or(ApiError::NotFound)?;
    if state.storage.is_ancestor(p.id, &target, &head).await? {
        return Ok(head);
    }
    let upstream = state.storage.merge_base(p.id, &target, &head).await?.ok_or_else(|| ApiError::bad("no common history"))?;
    let rebased = state.storage.rebase(p.id, &target, &upstream, &head, &committer(state)).await.map_err(conflict_error)?;
    may_rewrite(state, principal, p, &mr.source_branch, &head, &rebased).await?;
    let source_ref = format!("refs/heads/{}", mr.source_branch);
    state.storage.update_ref(p.id, &source_ref, &rebased, Some(&head)).await.map_err(conflict_error)?;
    let mut tx = state.db.begin().await?;
    state
        .jobs
        .enqueue(
            &mut tx,
            PostReceive {
                project_id: p.id,
                updates: vec![RefUpdate { old: head, new: rebased.clone(), name: source_ref }],
                principal: Some(principal.encode()),
            }
            .request(),
        )
        .await?;
    tx.commit().await?;
    Ok(rebased)
}

pub async fn set_open(state: &AppState, principal: &Principal, p: &Project, mr: &MergeRequest, open: bool) -> ApiResult<MergeRequest> {
    may_edit(state, principal, p, mr).await?;
    match (mr.state.as_str(), open) {
        ("merged", _) => return Err(ApiError::conflict("the merge request is already merged")),
        ("opened", true) | ("closed", false) => return Ok(mr.clone()),
        _ => {}
    }
    if open && mrdb::open_exists(&state.db, p.id, &mr.source_branch, &mr.target_branch).await?.is_some() {
        return Err(ApiError::conflict("another merge request is open for these branches"));
    }
    let user = principal.direct_user().map(|u| u.id);
    if !open {
        // it and the layers above leave the stack (while it is still part of it)
        super::stack::detach(state, p, mr, user).await?;
    }
    let mut tx = state.db.begin().await?;
    mrdb::set_state(&mut *tx, mr.id, if open { "opened" } else { "closed" }).await?;
    mrdb::add_note(&mut *tx, mr.id, user, None, if open { "reopened" } else { "closed" }, true, Position::default()).await?;
    tx.commit().await?;
    let mr = mrdb::find(&state.db, p.id, mr.iid).await?.ok_or(ApiError::NotFound)?;
    after_change(state, p, &mr, if open { "reopen" } else { "close" }, principal.direct_user()).await;
    Ok(mr)
}

/// Replace an MR's labels (Developer+), leaving a system note for the change.
pub async fn set_labels(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    m: &MergeRequest,
    names: &[String],
) -> ApiResult<Vec<forgust_db::issues::Label>> {
    use forgust_db::issues as idb;
    require_project(state, principal, ProjectAction::Push, p).await?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let labels = super::issues::resolve_labels(state, p.id, names).await?;
    let before: Vec<idb::Label> = idb::labels_of_mrs(&state.db, &[m.id]).await?.into_iter().map(|(_, l)| l).collect();
    let added: Vec<&str> = labels.iter().filter(|l| !before.iter().any(|b| b.id == l.id)).map(|l| l.name.as_str()).collect();
    let removed: Vec<&str> = before.iter().filter(|b| !labels.iter().any(|l| l.id == b.id)).map(|l| l.name.as_str()).collect();
    let mut tx = state.db.begin().await?;
    idb::set_mr_labels(&mut tx, m.id, &labels.iter().map(|l| l.id).collect::<Vec<_>>()).await?;
    if let Some(text) = super::issues::label_change(&added, &removed) {
        mrdb::add_note(&mut *tx, m.id, Some(user.id), None, &text, true, Position::default()).await?;
        mrdb::touch(&mut *tx, m.id).await?;
    }
    tx.commit().await?;
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: m.iid });
    Ok(labels)
}

/// The author or a Developer+ may edit, close and reopen.
pub async fn may_edit(state: &AppState, principal: &Principal, p: &Project, mr: &MergeRequest) -> ApiResult<()> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    if mr.author_id == Some(user.id) {
        // its author, while they can still see code (not once demoted to Guest)
        require_project(state, principal, ProjectAction::ReadCode, p).await?;
        return crate::auth::authz::writable(p);
    }
    require_project(state, principal, ProjectAction::Push, p).await
}

pub async fn approve(state: &AppState, principal: &Principal, p: &Project, mr: &MergeRequest, approve: bool) -> ApiResult<()> {
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let role = project_role(state, principal, p).await?;
    if role.level < forgust_db::access::REPORTER {
        return Err(ApiError::forbidden("approving needs at least the Reporter role"));
    }
    if mr.author_id == Some(user.id) && approve {
        return Err(ApiError::forbidden("authors cannot approve their own merge request"));
    }
    if mr.state != "opened" {
        return Err(ApiError::conflict("only open merge requests can be approved"));
    }
    if approve {
        let new = mrdb::approve(&state.db, mr.id, user.id).await?;
        super::people::reviewed(state, mr, user.id, super::people::ReviewAction::Approved).await;
        done(state, user.id, p, mr).await;
        // (approving again says nothing new)
        if new {
            super::notify::approved(state, p, mr, user).await;
        }
    } else {
        mrdb::unapprove(&state.db, mr.id, user.id).await?;
        super::people::reviewed(state, mr, user.id, super::people::ReviewAction::Unapproved).await;
    }
    mrdb::add_note(&state.db, mr.id, Some(user.id), None, if approve { "approved" } else { "revoked approval" }, true, Position::default())
        .await?;
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: mr.iid });
    super::auto_merge::poke(state, p.id, mr.iid).await;
    Ok(())
}

// ------------------------------------------------------------------ notes

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewPosition {
    /// Version the comment was written on (defaults to the latest).
    pub version: Option<i32>,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub old_line: Option<i32>,
    pub new_line: Option<i32>,
    /// A comment on several lines: the first of them (on either side).
    pub start_old_line: Option<i32>,
    pub start_new_line: Option<i32>,
}

pub async fn add_note(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    mr: &MergeRequest,
    body: &str,
    parent_id: Option<i64>,
    position: Option<NewPosition>,
) -> ApiResult<Note> {
    require_project(state, principal, ProjectAction::ReadCode, p).await?;
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    if body.trim().is_empty() || body.len() > 100_000 {
        return Err(ApiError::bad("a comment of 1-100,000 characters is required"));
    }
    if let Some(pid) = parent_id {
        let parent = mrdb::note(&state.db, mr.id, pid).await?.ok_or(ApiError::NotFound)?;
        if parent.parent_id.is_some() || parent.system {
            return Err(ApiError::bad("replies go to the first comment of a thread"));
        }
        if position.is_some() {
            return Err(ApiError::bad("replies take the position of their thread"));
        }
    }
    let diffs = mrdb::diffs(&state.db, mr.id).await?;
    let mut pos = Position::default();
    let (diff, old_path, new_path);
    if let Some(np) = &position {
        diff = match np.version {
            Some(v) => diffs.iter().find(|d| d.version == v).ok_or_else(|| ApiError::bad("unknown version"))?,
            None => diffs.first().ok_or_else(|| ApiError::bad("no version yet"))?,
        };
        if np.old_line.is_none() && np.new_line.is_none() {
            return Err(ApiError::bad("a position needs old_line or new_line"));
        }
        // a range starts on or before the line it ends on, and is not huge
        let before = |start: Option<i32>, end: Option<i32>| match (start, end) {
            (Some(s), Some(e)) => s >= 1 && s <= e && e - s < 1000,
            (Some(_), None) => true,
            _ => true,
        };
        if np.start_old_line.is_some_and(|s| s < 1)
            || np.start_new_line.is_some_and(|s| s < 1)
            || !before(np.start_old_line, np.old_line)
            || !before(np.start_new_line, np.new_line)
        {
            return Err(ApiError::bad("a line range must start before it ends"));
        }
        old_path = np.old_path.clone().or(np.new_path.clone());
        new_path = np.new_path.clone().or(np.old_path.clone());
        let file = old_path.as_deref().or(new_path.as_deref()).ok_or_else(|| ApiError::bad("a position needs a path"))?;
        if !forgust_storage::validate_path(file) {
            return Err(ApiError::bad("bad path"));
        }
        pos = Position {
            diff_id: Some(diff.id),
            base_sha: Some(&diff.base_sha),
            start_sha: Some(&diff.start_sha),
            head_sha: Some(&diff.head_sha),
            old_path: old_path.as_deref(),
            new_path: new_path.as_deref(),
            old_line: np.old_line,
            new_line: np.new_line,
            start_old_line: np.start_old_line,
            start_new_line: np.start_new_line,
        };
    }
    let id = mrdb::add_note(&state.db, mr.id, Some(user.id), parent_id, body, false, pos).await?;
    mrdb::touch(&state.db, mr.id).await?;
    super::people::reviewed(state, mr, user.id, super::people::ReviewAction::Commented).await;
    let note = mrdb::note(&state.db, mr.id, id).await?.ok_or(ApiError::NotFound)?;
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: mr.iid });
    super::webhooks::emit(
        state,
        p,
        "note",
        json!({ "object_kind": "note", "project": project_json(state, p), "merge_request": { "iid": mr.iid, "title": mr.title },
                "note": { "id": note.id, "body": note.body, "author": user.username, "new_path": note.new_path, "new_line": note.new_line } }),
    )
    .await;
    let mentioned = super::refs::on_write(state, p, &user, mr_target(mr), body, None).await;
    done(state, user.id, p, mr).await;
    super::notify::merge_request_note(state, p, mr, &user, &note, &mentioned).await;
    Ok(note)
}

/// A note's position on another version of the MR.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MappedPosition {
    pub version: i32,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub old_line: Option<i32>,
    pub new_line: Option<i32>,
}

/// Where a positioned note lands on `to` (None = outdated). New-side lines move
/// through diff(note head → version head); old-side lines through diff(note base →
/// version base). The result is cached by (note, version head).
pub async fn map_position(state: &AppState, project_id: i64, note: &Note, to: &MrDiff) -> ApiResult<Option<MappedPosition>> {
    let (Some(head), Some(base)) = (&note.head_sha, &note.base_sha) else { return Ok(None) };
    let key = crate::state::ResponseCache::key(project_id, "notepos", &[&note.id.to_string(), &to.head_sha, &to.base_sha]);
    if let Some(b) = state.cache.get(&key).await {
        return Ok(serde_json::from_slice(&b).unwrap_or(None));
    }
    let (from_sha, to_sha, path, line, new_side) = match (note.new_line, note.old_line) {
        (Some(l), _) => (head.as_str(), to.head_sha.as_str(), note.new_path.as_deref(), l, true),
        (None, Some(l)) => (base.as_str(), to.base_sha.as_str(), note.old_path.as_deref(), l, false),
        _ => return Ok(None),
    };
    let Some(path) = path else { return Ok(None) };
    let mapped = if from_sha == to_sha {
        Some((path.to_string(), line as u32))
    } else {
        let d = state
            .storage
            .diff(
                project_id,
                Some(from_sha),
                to_sha,
                DiffOptions { offset: 0, limit: 10, max_lines_per_file: usize::MAX, paths: Some(vec![path.to_string()]) },
            )
            .await?;
        match d.files.iter().find(|f| f.old_path == path) {
            None => Some((path.to_string(), line as u32)),
            Some(f) => map_old_line_to_new(f, line as u32).map(|l| (f.new_path.clone(), l)),
        }
    };
    let result = mapped.map(|(path, line)| MappedPosition {
        version: to.version,
        old_path: if new_side { note.old_path.clone() } else { Some(path.clone()) },
        new_path: if new_side { Some(path) } else { note.new_path.clone() },
        old_line: if new_side { None } else { Some(line as i32) },
        new_line: if new_side { Some(line as i32) } else { None },
    });
    state.cache.put(key, serde_json::to_vec(&result).unwrap_or_default().into()).await;
    Ok(result)
}

pub async fn resolve_thread(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    mr: &MergeRequest,
    note_id: i64,
    resolved: bool,
) -> ApiResult<()> {
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    let note = mrdb::note(&state.db, mr.id, note_id).await?.ok_or(ApiError::NotFound)?;
    if note.parent_id.is_some() || note.system {
        return Err(ApiError::bad("resolve the first comment of a thread"));
    }
    if mr.author_id != Some(user.id) && note.author_id != Some(user.id) {
        require_project(state, principal, ProjectAction::Push, p).await?;
    }
    mrdb::set_resolved(&state.db, note.id, resolved.then_some(user.id)).await?;
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: mr.iid });
    super::auto_merge::poke(state, p.id, mr.iid).await;
    Ok(())
}

/// Authors edit their own notes; Maintainers can delete any.
pub async fn edit_note(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    mr: &MergeRequest,
    note_id: i64,
    body: Option<&str>,
) -> ApiResult<()> {
    let note = mrdb::note(&state.db, mr.id, note_id).await?.ok_or(ApiError::NotFound)?;
    super::check_note_change(state, principal, p, note.author_id, note.system, body).await?;
    match body {
        Some(b) => {
            mrdb::edit_note(&state.db, note.id, b).await?;
            if let Some(user) = principal.direct_user() {
                super::refs::on_write(state, p, user, mr_target(mr), b, Some(&note.body)).await;
            }
        }
        None => mrdb::delete_note(&state.db, note.id).await?,
    }
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: mr.iid });
    Ok(())
}

// ------------------------------------------------------------------ side effects

pub(crate) fn mr_target(m: &MergeRequest) -> super::refs::Target<'_> {
    super::refs::Target::MergeRequest { id: m.id, iid: m.iid, title: &m.title }
}

/// Someone acted on the MR: their to-dos about it are done.
async fn done(state: &AppState, user_id: i64, p: &Project, m: &MergeRequest) {
    if let Err(e) = forgust_db::todos::done_for_target(&state.db, user_id, p.id, "merge_request", m.iid).await {
        tracing::warn!(error = %e, "to-dos not marked done");
    }
}

pub fn project_json(state: &AppState, p: &Project) -> serde_json::Value {
    json!({ "id": p.id, "name": p.name, "path_with_namespace": p.full_path, "web_url": state.cfg.web_url(&p.full_path) })
}

pub fn merge_request_payload(
    state: &AppState,
    p: &Project,
    mr: &MergeRequest,
    action: &str,
    user: Option<&User>,
    diff: Option<&MrDiff>,
) -> serde_json::Value {
    json!({
        "object_kind": "merge_request",
        "event_type": "merge_request",
        "user": user.map(|u| json!({ "id": u.id, "username": u.username, "name": u.name })),
        "project": project_json(state, p),
        "object_attributes": {
            "iid": mr.iid, "title": mr.title, "description": mr.description, "state": mr.state, "action": action,
            "source_branch": mr.source_branch, "target_branch": mr.target_branch, "draft": mr.draft,
            "merge_commit_sha": mr.merge_commit_sha, "last_commit": diff.map(|d| d.head_sha.clone()),
            "url": state.cfg.mr_url(&p.full_path, mr.iid),
        }
    })
}

/// Live update, webhook and email after a state change.
async fn after_change(state: &AppState, p: &Project, mr: &MergeRequest, action: &str, user: Option<&User>) {
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: mr.iid });
    let latest = mrdb::latest_diff(&state.db, mr.id).await.ok().flatten();
    super::webhooks::emit(state, p, "merge_request", merge_request_payload(state, p, mr, action, user, latest.as_ref())).await;
    if action != "open" {
        super::notify::merge_request_state(state, p, mr, action, user).await;
    }
}
