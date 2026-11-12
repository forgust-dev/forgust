//! Auto-merge (M6): "merge when checks pass".
//!
//! Someone who may merge sets it on an open MR. It then merges as them once
//! every merge check passes and the newest pipeline for the reviewed head has
//! succeeded (when there is one). New commits on the source branch, a failed or
//! canceled pipeline, closing, or a merge that fails cancel it, with a note.
//!
//! The check runs in a job (`AutoMergeCheck`), enqueued when something that
//! can unblock a merge happens: a pipeline finishes, an approval, a resolved
//! thread, the draft flag, the layer below in a stack merging. The CI sweep
//! re-checks every waiting MR each minute in case an event was missed.

use forgust_db::merge_requests::{self as mrdb, MergeRequest, Position};
use forgust_db::projects::Project;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::mr::{self, MergeMethod, MergeOptions};
use crate::auth::Principal;
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::jobs::JobRequest;
use crate::state::AppState;

fn method_name(m: MergeMethod) -> &'static str {
    match m {
        MergeMethod::Merge => "merge",
        MergeMethod::Squash => "squash",
        MergeMethod::FastForward => "fast_forward",
        MergeMethod::Rebase => "rebase",
    }
}

fn method_of(s: &str) -> MergeMethod {
    match s {
        "squash" => MergeMethod::Squash,
        "fast_forward" => MergeMethod::FastForward,
        "rebase" => MergeMethod::Rebase,
        _ => MergeMethod::Merge,
    }
}

const ACTIVE: [&str; 6] = ["created", "pending", "running", "preparing", "waiting_for_resource", "scheduled"];

/// What `enable` did.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Everything already passed: merged now.
    Merged,
    /// Waiting for the checks.
    Waiting,
}

/// Set auto-merge. Merges at once when nothing is left to wait for.
pub async fn enable(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    m: &MergeRequest,
    method: MergeMethod,
    sha: Option<&str>,
    remove_source: bool,
) -> ApiResult<Outcome> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    if m.state != "opened" {
        return Err(ApiError::conflict(format!("the merge request is {}", m.state)));
    }
    let status = mr::merge_status(state, principal, p, m).await?;
    if !status.user_can_merge {
        return Err(ApiError::forbidden(format!("you are not allowed to merge into `{}`", m.target_branch)));
    }
    let head =
        status.head_sha.clone().ok_or_else(|| ApiError::conflict(format!("source branch `{}` no longer exists", m.source_branch)))?;
    if sha.is_some_and(|s| s != head) {
        return Err(ApiError::conflict("the source branch changed since you loaded the page; review the new changes first"));
    }
    if !status.conflicts.is_empty() {
        return Err(ApiError::conflict("there are merge conflicts: resolve them first"));
    }
    let waiting = status.pipeline_status.as_deref().is_some_and(|s| ACTIVE.contains(&s));
    if status.can_merge && !waiting {
        mr::merge(
            state,
            principal,
            p,
            m,
            MergeOptions { method, expected_head: Some(&head), remove_source_branch: remove_source, message: None, train: None },
        )
        .await?;
        return Ok(Outcome::Merged);
    }
    mrdb::set_auto_merge(&state.db, m.id, Some(user.id), Some(method_name(method)), Some(&head), remove_source, Some(&m.target_branch))
        .await?;
    note(state, m, Some(user.id), "set auto-merge: it merges when every check passes").await;
    // a check that passed in between still counts
    poke(state, p.id, m.iid).await;
    Ok(Outcome::Waiting)
}

/// Turn auto-merge off (the author, whoever set it, or anyone who may merge).
pub async fn disable(state: &AppState, principal: &Principal, p: &Project, m: &MergeRequest) -> ApiResult<()> {
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    if m.auto_merge_by.is_none() {
        return Err(ApiError::NotFound);
    }
    let allowed = m.auto_merge_by == Some(user.id)
        || mr::may_edit(state, principal, p, m).await.is_ok()
        || mr::branch_rights(state, principal, p, &m.target_branch, false).await?;
    if !allowed {
        return Err(ApiError::forbidden("you cannot change this merge request"));
    }
    cancel(state, m, Some(user.id), "canceled auto-merge").await;
    Ok(())
}

async fn note(state: &AppState, m: &MergeRequest, author: Option<i64>, text: &str) {
    let r: anyhow::Result<()> = async {
        mrdb::add_note(&state.db, m.id, author, None, text, true, Position::default()).await?;
        mrdb::touch(&state.db, m.id).await?;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(mr = m.id, error = %e, "auto-merge note not added");
    }
    state.events.publish(Event::MergeRequest { project_id: m.project_id, iid: m.iid });
}

async fn cancel(state: &AppState, m: &MergeRequest, author: Option<i64>, text: &str) {
    if let Err(e) = mrdb::set_auto_merge(&state.db, m.id, None, None, None, false, None).await {
        tracing::warn!(mr = m.id, error = %e, "auto-merge not canceled");
        return;
    }
    note(state, m, author, text).await;
}

/// Enqueue a check when the MR waits to auto-merge (cheap otherwise).
pub async fn poke(state: &AppState, project_id: i64, iid: i64) {
    let r: anyhow::Result<()> = async {
        let Some(m) = mrdb::find(&state.db, project_id, iid).await? else { return Ok(()) };
        if m.auto_merge_by.is_none() || m.state != "opened" {
            return Ok(());
        }
        let mut conn = state.db.acquire().await?;
        state.jobs.enqueue(&mut conn, AutoMergeCheck { project_id, iid }.request()).await?;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(project_id, iid, error = %e, "auto-merge check not queued");
    }
}

/// Merge when everything passes; cancel on what will not fix itself.
pub async fn check(state: &AppState, project_id: i64, iid: i64) -> anyhow::Result<()> {
    let Some(m) = mrdb::find(&state.db, project_id, iid).await? else { return Ok(()) };
    let (Some(by), Some(sha)) = (m.auto_merge_by, m.auto_merge_sha.clone()) else { return Ok(()) };
    if m.state != "opened" {
        return Ok(());
    }
    // consent was for this target: a person retargeting the MR (who may not
    // merge there themselves) must not borrow the setter's merge rights
    if m.auto_merge_target.as_deref() != Some(m.target_branch.as_str()) {
        cancel(state, &m, None, "canceled auto-merge: the target branch changed").await;
        return Ok(());
    }
    let Some(p) = forgust_db::projects::find(&state.db, project_id).await? else { return Ok(()) };
    let Some(user) = forgust_db::users::find(&state.db, by).await?.filter(|u| u.is_active()) else {
        cancel(state, &m, None, "canceled auto-merge: the person who set it can no longer merge").await;
        return Ok(());
    };
    let principal = Principal::User { user, scopes: None };
    let status = match mr::merge_status(state, &principal, &p, &m).await {
        Ok(s) => s,
        Err(e) => return Err(anyhow::anyhow!("merge status: {e:?}")),
    };
    if status.head_sha.as_deref() != Some(sha.as_str()) {
        cancel(state, &m, None, "canceled auto-merge: new commits were pushed").await;
        return Ok(());
    }
    if !status.user_can_merge {
        cancel(state, &m, None, "canceled auto-merge: the person who set it can no longer merge").await;
        return Ok(());
    }
    if !status.conflicts.is_empty() {
        cancel(state, &m, None, "canceled auto-merge: there are merge conflicts").await;
        return Ok(());
    }
    match status.pipeline_status.as_deref() {
        Some(s) if ACTIVE.contains(&s) => return Ok(()),
        Some(s @ ("failed" | "canceled")) => {
            cancel(state, &m, None, &format!("canceled auto-merge: the pipeline {s}")).await;
            return Ok(());
        }
        _ => {}
    }
    if !status.can_merge {
        return Ok(()); // approvals, threads, draft, a lower layer: wait
    }
    let opts = MergeOptions {
        method: method_of(m.auto_merge_method.as_deref().unwrap_or("merge")),
        expected_head: Some(&sha),
        remove_source_branch: m.auto_merge_remove_source,
        message: None,
        train: None,
    };
    if let Err(e) = mr::merge(state, &principal, &p, &m, opts).await {
        let why = match e {
            ApiError::Conflict(msg) | ApiError::Forbidden(msg) | ApiError::BadRequest(msg) => msg,
            other => other.to_string(),
        };
        cancel(state, &m, None, &format!("canceled auto-merge: the merge failed ({why})")).await;
    }
    Ok(())
}

/// Every waiting MR (the CI sweep's safety net for missed events).
pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    for (project_id, iid) in mrdb::auto_merge_pending(&state.db, 200).await? {
        if let Err(e) = check(state, project_id, iid).await {
            tracing::warn!(project_id, iid, error = %e, "auto-merge check failed");
        }
    }
    Ok(())
}

/// Background check of one MR (deduplicated per MR).
#[derive(Debug, Serialize, Deserialize)]
pub struct AutoMergeCheck {
    pub project_id: i64,
    pub iid: i64,
}

impl AutoMergeCheck {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: <Self as graphile_worker::TaskHandler>::IDENTIFIER,
            queue: Some(format!("auto_merge:{}", self.project_id)),
            job_key: Some(format!("auto_merge:{}:{}", self.project_id, self.iid)),
            payload: serde_json::to_value(&self).unwrap_or_default(),
            max_attempts: Some(5),
        }
    }
}

impl graphile_worker::TaskHandler for AutoMergeCheck {
    const IDENTIFIER: &'static str = "auto_merge_check";

    async fn run(self, ctx: graphile_worker::WorkerContext) -> impl graphile_worker::IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        check(state, self.project_id, self.iid).await.map_err(|e| e.to_string())
    }
}
