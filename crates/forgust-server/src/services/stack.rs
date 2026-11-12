//! Stacked merge requests (ADR-0019).
//!
//! A stack is an explicit chain: the bottom MR targets the trunk, each other MR
//! targets the source branch of the one below. Only the lowest unmerged MR can
//! be merged; when it is, the next one is retargeted to the trunk and rebased
//! onto it so its diff keeps showing only its own commits.

use std::collections::HashMap;

use forgust_db::merge_requests::{self as mrdb, MergeRequest, Position};
use forgust_db::projects::Project;
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::jobs::{PostReceive, RefUpdate};
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct StackEntry {
    pub iid: i64,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub source_branch: String,
    pub target_branch: String,
    pub author_username: Option<String>,
    /// The branch below moved on: this MR does not contain its head.
    pub needs_rebase: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StackView {
    pub id: i64,
    pub trunk: String,
    /// 1-based position of the MR being viewed (1 = bottom).
    pub position: usize,
    pub size: usize,
    /// Bottom first: merged layers, then the open chain.
    pub entries: Vec<StackEntry>,
    /// The lowest unmerged MR: the only one that can be merged.
    pub next_to_merge: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StackBadge {
    pub id: i64,
    pub position: usize,
    pub size: usize,
}

/// Stack members bottom first: merged ones by merge time, then the open chain
/// walked from the trunk (or from a merged layer's branch, before a retarget).
/// Closed MRs are left out.
pub fn order(trunk: &str, members: Vec<MergeRequest>) -> Vec<MergeRequest> {
    let (mut merged, mut open): (Vec<_>, Vec<_>) = members.into_iter().filter(|m| m.state != "closed").partition(|m| m.state == "merged");
    merged.sort_by_key(|m| (m.merged_at, m.id));
    let mut out = merged;
    let mut bases: Vec<String> = vec![trunk.to_string()];
    bases.extend(out.iter().map(|m| m.source_branch.clone()));
    let mut cur: Option<String> = None;
    loop {
        let pos = match &cur {
            Some(c) => open.iter().position(|m| &m.target_branch == c),
            None => open.iter().position(|m| bases.contains(&m.target_branch)),
        };
        let Some(i) = pos else { break };
        let m = open.remove(i);
        cur = Some(m.source_branch.clone());
        out.push(m);
    }
    // a chain broken by hand (retargeted elsewhere): keep them visible at the top
    open.sort_by_key(|m| m.id);
    out.extend(open);
    out
}

async fn members(state: &AppState, stack_id: i64) -> ApiResult<(mrdb::Stack, Vec<MergeRequest>)> {
    let s = mrdb::find_stack(&state.db, stack_id).await?.ok_or(ApiError::NotFound)?;
    let list = mrdb::stack_members(&state.db, &[stack_id]).await?;
    let ordered = order(&s.trunk, list);
    Ok((s, ordered))
}

async fn head(state: &AppState, project_id: i64, branch: &str) -> ApiResult<Option<String>> {
    Ok(state.storage.resolve(project_id, &format!("refs/heads/{branch}")).await?)
}

/// The stack map for an MR's page.
pub async fn view(state: &AppState, p: &Project, m: &MergeRequest) -> ApiResult<Option<StackView>> {
    let Some(sid) = m.stack_id else { return Ok(None) };
    let (s, list) = members(state, sid).await?;
    let mut entries = Vec::with_capacity(list.len());
    for x in &list {
        let needs_rebase = if x.state == "opened" {
            match (head(state, p.id, &x.target_branch).await?, head(state, p.id, &x.source_branch).await?) {
                (Some(t), Some(h)) => !state.storage.is_ancestor(p.id, &t, &h).await?,
                _ => false,
            }
        } else {
            false
        };
        entries.push(StackEntry {
            iid: x.iid,
            title: x.title.clone(),
            state: x.state.clone(),
            draft: x.draft,
            source_branch: x.source_branch.clone(),
            target_branch: x.target_branch.clone(),
            author_username: x.author_username.clone(),
            needs_rebase,
        });
    }
    Ok(Some(StackView {
        id: s.id,
        trunk: s.trunk,
        position: list.iter().position(|x| x.id == m.id).map(|i| i + 1).unwrap_or(0),
        size: list.len(),
        next_to_merge: list.iter().find(|x| x.state == "opened").map(|x| x.iid),
        entries,
    }))
}

/// Position badges for list rows, keyed by MR id.
pub async fn badges(state: &AppState, list: &[MergeRequest]) -> ApiResult<HashMap<i64, StackBadge>> {
    let mut ids: Vec<i64> = list.iter().filter_map(|m| m.stack_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let all = mrdb::stack_members(&state.db, &ids).await?;
    for sid in ids {
        let Some(s) = mrdb::find_stack(&state.db, sid).await? else { continue };
        let ordered = order(&s.trunk, all.iter().filter(|m| m.stack_id == Some(sid)).cloned().collect());
        for (i, m) in ordered.iter().enumerate() {
            out.insert(m.id, StackBadge { id: sid, position: i + 1, size: ordered.len() });
        }
    }
    Ok(out)
}

/// The blocker for merging `m` while lower layers are open.
pub async fn merge_blocker(state: &AppState, m: &MergeRequest) -> ApiResult<Option<String>> {
    let Some(sid) = m.stack_id else { return Ok(None) };
    let (_, list) = members(state, sid).await?;
    Ok(match list.iter().find(|x| x.state == "opened") {
        Some(low) if low.id != m.id => Some(format!("merge !{} first: a stack merges from the bottom up", low.iid)),
        _ => None,
    })
}

async fn note(state: &AppState, m: &MergeRequest, author: Option<i64>, text: &str) -> ApiResult<()> {
    mrdb::add_note(&state.db, m.id, author, None, text, true, Position::default()).await?;
    mrdb::touch(&state.db, m.id).await?;
    Ok(())
}

/// Before creating an MR on top of `parent_iid`: it exists, is open, is the top
/// of its stack, and `target` is its source branch.
pub async fn precheck(state: &AppState, p: &Project, parent_iid: i64, target: &str) -> ApiResult<MergeRequest> {
    let parent = mrdb::find(&state.db, p.id, parent_iid).await?.ok_or_else(|| ApiError::bad(format!("no merge request !{parent_iid}")))?;
    if parent.state != "opened" {
        return Err(ApiError::conflict(format!("!{parent_iid} is not open")));
    }
    if target != parent.source_branch {
        return Err(ApiError::bad(format!("to stack on !{parent_iid}, target its branch `{}`", parent.source_branch)));
    }
    if let Some(sid) = parent.stack_id {
        let (_, list) = members(state, sid).await?;
        if let Some(above) = list.iter().find(|x| x.state == "opened" && x.target_branch == parent.source_branch) {
            return Err(ApiError::conflict(format!("!{} is already stacked on !{parent_iid}; a stack is a single chain", above.iid)));
        }
    }
    Ok(parent)
}

/// Put `m` on top of `parent` (which must be the top of its stack, or alone).
/// `m` must target `parent`'s source branch.
pub async fn stack_on(state: &AppState, principal: &Principal, p: &Project, m: &MergeRequest, parent_iid: i64) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let user = principal.direct_user().map(|u| u.id);
    let parent = mrdb::find(&state.db, p.id, parent_iid).await?.ok_or_else(|| ApiError::bad(format!("no merge request !{parent_iid}")))?;
    if parent.id == m.id {
        return Err(ApiError::bad("a merge request cannot be stacked on itself"));
    }
    if m.state != "opened" || parent.state != "opened" {
        return Err(ApiError::conflict("only open merge requests can be stacked"));
    }
    if m.stack_id.is_some() {
        return Err(ApiError::conflict(format!("!{} is already in a stack; leave it first", m.iid)));
    }
    if m.target_branch != parent.source_branch {
        return Err(ApiError::bad(format!(
            "!{} must target `{}` (the branch of !{}) to stack on it",
            m.iid, parent.source_branch, parent.iid
        )));
    }
    if let Some(sid) = parent.stack_id {
        let (_, list) = members(state, sid).await?;
        if let Some(above) = list.iter().find(|x| x.state == "opened" && x.target_branch == parent.source_branch) {
            return Err(ApiError::conflict(format!("!{} is already stacked on !{}; a stack is a single chain", above.iid, parent.iid)));
        }
    }
    let base = match (head(state, p.id, &parent.source_branch).await?, head(state, p.id, &m.source_branch).await?) {
        (Some(a), Some(b)) => state.storage.merge_base(p.id, &a, &b).await?,
        _ => None,
    };
    let mut tx = state.db.begin().await?;
    let sid = match parent.stack_id {
        Some(sid) => sid,
        None => {
            let sid = mrdb::create_stack(&mut *tx, p.id, &parent.target_branch).await?;
            mrdb::set_stack(&mut *tx, parent.id, Some(sid), None).await?;
            sid
        }
    };
    mrdb::set_stack(&mut *tx, m.id, Some(sid), base.as_deref()).await?;
    tx.commit().await?;
    note(state, m, user, &format!("stacked on !{}", parent.iid)).await?;
    note(state, &parent, user, &format!("!{} stacked on top", m.iid)).await?;
    for x in [m, &parent] {
        state.events.publish(Event::MergeRequest { project_id: p.id, iid: x.iid });
    }
    Ok(())
}

/// Take `m` and every layer above it out of its stack; a stack left with fewer
/// than two merge requests is dissolved. The caller checks permissions.
pub async fn detach(state: &AppState, p: &Project, m: &MergeRequest, actor: Option<i64>) -> ApiResult<()> {
    let Some(sid) = m.stack_id else { return Ok(()) };
    let (_, list) = members(state, sid).await?;
    let at = list.iter().position(|x| x.id == m.id);
    let leaving: Vec<&MergeRequest> = match at {
        Some(i) => list[i..].iter().filter(|x| x.state != "merged").collect(),
        None => vec![m],
    };
    let staying: Vec<&MergeRequest> = list.iter().filter(|x| !leaving.iter().any(|l| l.id == x.id)).collect();
    let dissolve = staying.len() < 2;
    let mut tx = state.db.begin().await?;
    for x in &leaving {
        mrdb::set_stack(&mut *tx, x.id, None, None).await?;
    }
    if dissolve {
        for x in &staying {
            mrdb::set_stack(&mut *tx, x.id, None, None).await?;
        }
        mrdb::delete_stack(&mut *tx, sid).await?;
    }
    tx.commit().await?;
    for x in &leaving {
        note(state, x, actor, "removed from its stack").await?;
    }
    for x in leaving.iter().chain(staying.iter()) {
        state.events.publish(Event::MergeRequest { project_id: p.id, iid: x.iid });
    }
    Ok(())
}

pub async fn leave(state: &AppState, principal: &Principal, p: &Project, m: &MergeRequest) -> ApiResult<()> {
    super::mr::may_edit(state, principal, p, m).await?;
    if m.stack_id.is_none() {
        return Err(ApiError::NotFound);
    }
    detach(state, p, m, principal.direct_user().map(|u| u.id)).await
}

/// After `merged` landed on its target (moved from its old head `old_head` to
/// `new_target`): the next layer of its stack now targets the trunk. When the
/// landed commits are not the merged MR's own (squash, rebase), the next branch
/// is rebased onto the new trunk, and every layer above onto the rebased one
/// below it. Runs under the repo lock `merge` holds. Returns the next MR, the
/// ref updates to report, and the next MR's new head.
pub async fn after_merge(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    merged: &MergeRequest,
    old_head: &str,
    new_target: &str,
) -> Option<(MergeRequest, Vec<RefUpdate>, String)> {
    let sid = merged.stack_id?;
    let (_, list) = match members(state, sid).await {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!(error = %e, "stack: members not loaded");
            return None;
        }
    };
    // the open layers above the merged one, bottom first
    let mut chain: Vec<&MergeRequest> = Vec::new();
    let mut below = merged.source_branch.clone();
    while let Some(x) = list.iter().find(|x| x.state == "opened" && x.target_branch == below) {
        below = x.source_branch.clone();
        chain.push(x);
    }
    let next = (*chain.first()?).clone();
    let actor = principal.direct_user().map(|u| u.id);
    let next_head = head(state, p.id, &next.source_branch).await.ok().flatten()?;
    let mut updates = Vec::new();
    let mut new_head = next_head.clone();
    let mut problem: Option<(i64, String)> = None;
    let landed_as_is = state.storage.is_ancestor(p.id, old_head, new_target).await.unwrap_or(false);
    if !landed_as_is {
        // replay each layer's own commits (those after the old head below it)
        let (mut onto, mut upstream) = (new_target.to_string(), old_head.to_string());
        for x in &chain {
            let Some(tip) = head(state, p.id, &x.source_branch).await.ok().flatten() else {
                problem = Some((x.iid, "its branch no longer exists".into()));
                break;
            };
            let r = format!("refs/heads/{}", x.source_branch);
            let step: ApiResult<String> = async {
                let rebased = state
                    .storage
                    .rebase(p.id, &onto, &upstream, &tip, &super::mr::committer(state))
                    .await
                    .map_err(super::mr::conflict_error)?;
                super::mr::may_rewrite(state, principal, p, &x.source_branch, &tip, &rebased).await?;
                state.storage.update_ref(p.id, &r, &rebased, Some(&tip)).await.map_err(super::mr::conflict_error)?;
                mrdb::set_stack_base(&state.db, x.id, &onto).await?;
                Ok(rebased)
            }
            .await;
            match step {
                Ok(rebased) => {
                    updates.push(RefUpdate { old: tip.clone(), new: rebased.clone(), name: r });
                    if x.id == next.id {
                        new_head = rebased.clone();
                    }
                    upstream = tip;
                    onto = rebased;
                }
                Err(e) => {
                    problem = Some((x.iid, e.to_string()));
                    break;
                }
            }
        }
    } else if let Err(e) = mrdb::set_stack_base(&state.db, next.id, new_target).await {
        tracing::warn!(error = %e, "stack: base not recorded");
    }
    let r: ApiResult<()> = async {
        mrdb::set_target(&state.db, next.id, &merged.target_branch).await?;
        mrdb::retarget_auto_merge(&state.db, next.id, &merged.target_branch).await?;
        note(state, &next, actor, &format!("retargeted to `{}` after !{} merged", merged.target_branch, merged.iid)).await?;
        if let Some((iid, why)) = &problem
            && let Some(x) = chain.iter().find(|x| x.iid == *iid)
        {
            note(state, x, actor, &format!("could not be rebased after !{} merged ({why}): use Rebase stack", merged.iid)).await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(error = %e, "stack: retarget not recorded");
    }
    let next = mrdb::find(&state.db, p.id, next.iid).await.ok().flatten()?;
    Some((next, updates, new_head))
}

/// Rebase every open layer, bottom up, onto the branch below it. Stops at the
/// first conflict. Returns how many branches moved.
pub async fn rebase(state: &AppState, principal: &Principal, p: &Project, m: &MergeRequest) -> ApiResult<usize> {
    require_project(state, principal, ProjectAction::Push, p).await?;
    let sid = m.stack_id.ok_or(ApiError::NotFound)?;
    let _lock = state.repo_locks.get(p.id).read_owned().await;
    let (_, list) = members(state, sid).await?;
    let mut updates = Vec::new();
    let mut failure = None;
    for x in list.iter().filter(|x| x.state == "opened") {
        let (Some(onto), Some(tip)) = (head(state, p.id, &x.target_branch).await?, head(state, p.id, &x.source_branch).await?) else {
            failure = Some(format!("a branch of !{} no longer exists", x.iid));
            break;
        };
        if state.storage.is_ancestor(p.id, &onto, &tip).await? {
            mrdb::set_stack_base(&state.db, x.id, &onto).await?;
            continue;
        }
        let upstream = match &x.stack_base_sha {
            Some(b) if state.storage.is_ancestor(p.id, b, &tip).await? => b.clone(),
            _ => state.storage.merge_base(p.id, &onto, &tip).await?.ok_or_else(|| ApiError::bad("no common history"))?,
        };
        let rebased = match state.storage.rebase(p.id, &onto, &upstream, &tip, &super::mr::committer(state)).await {
            Ok(r) => r,
            Err(forgust_storage::StorageError::Conflict(paths)) => {
                failure = Some(format!("!{} conflicts with `{}` in {}", x.iid, x.target_branch, paths.join(", ")));
                break;
            }
            Err(e) => return Err(e.into()),
        };
        super::mr::may_rewrite(state, principal, p, &x.source_branch, &tip, &rebased).await?;
        let r = format!("refs/heads/{}", x.source_branch);
        state.storage.update_ref(p.id, &r, &rebased, Some(&tip)).await.map_err(super::mr::conflict_error)?;
        mrdb::set_stack_base(&state.db, x.id, &onto).await?;
        updates.push(RefUpdate { old: tip, new: rebased, name: r });
    }
    let moved = updates.len();
    if !updates.is_empty() {
        let mut tx = state.db.begin().await?;
        state.jobs.enqueue(&mut tx, PostReceive { project_id: p.id, updates, principal: Some(principal.encode()) }.request()).await?;
        tx.commit().await?;
    }
    for x in &list {
        state.events.publish(Event::MergeRequest { project_id: p.id, iid: x.iid });
    }
    match failure {
        Some(f) if moved == 0 => Err(ApiError::conflict(f)),
        Some(f) => Err(ApiError::conflict(format!("rebased {moved} branch(es), then stopped: {f}"))),
        None => Ok(moved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mr(id: i64, source: &str, target: &str, state: &str) -> MergeRequest {
        MergeRequest {
            id,
            project_id: 1,
            iid: id,
            title: format!("mr {id}"),
            description: String::new(),
            author_id: None,
            author_username: None,
            source_branch: source.into(),
            target_branch: target.into(),
            state: state.into(),
            draft: false,
            squash: false,
            merge_commit_sha: None,
            merged_by: None,
            merged_at: (state == "merged").then(chrono::Utc::now),
            closed_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            imported_author: None,
            stack_id: Some(1),
            stack_base_sha: None,
            auto_merge_by: None,
            auto_merge_method: None,
            auto_merge_sha: None,
            auto_merge_remove_source: false,
            auto_merge_target: None,
            remove_source_branch: false,
            milestone_id: None,
            milestone_title: None,
        }
    }

    fn iids(v: &[MergeRequest]) -> Vec<i64> {
        v.iter().map(|m| m.iid).collect()
    }

    #[test]
    fn orders_the_chain_from_the_trunk() {
        let list = vec![mr(3, "c", "b", "opened"), mr(1, "a", "main", "opened"), mr(2, "b", "a", "opened")];
        assert_eq!(iids(&order("main", list)), vec![1, 2, 3]);
    }

    #[test]
    fn merged_layers_come_first_and_retargeted_or_not_both_work() {
        // after !1 merged and !2 retargeted to main
        let list = vec![mr(3, "c", "b", "opened"), mr(1, "a", "main", "merged"), mr(2, "b", "main", "opened")];
        assert_eq!(iids(&order("main", list)), vec![1, 2, 3]);
        // before the retarget happened
        let list = vec![mr(3, "c", "b", "opened"), mr(1, "a", "main", "merged"), mr(2, "b", "a", "opened")];
        assert_eq!(iids(&order("main", list)), vec![1, 2, 3]);
    }

    #[test]
    fn closed_are_left_out_and_strays_kept() {
        let list = vec![mr(1, "a", "main", "opened"), mr(2, "b", "a", "closed"), mr(3, "c", "elsewhere", "opened")];
        assert_eq!(iids(&order("main", list)), vec![1, 3]);
    }
}
