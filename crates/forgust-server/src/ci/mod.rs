//! CI compatible with GitLab Runner (M4): pipelines, the runner API, job logs
//! and artifacts. See the architecture (CI) and CI compatibility guides.

pub mod archive;
pub mod bridge;
pub mod environments;
pub mod hooks;
pub mod keywords;
pub mod oidc;
pub mod payload;
pub mod pipeline;
pub mod reports;
pub mod resolver;
pub mod runner_api;
pub mod schedule;
pub mod security;
pub mod trace;
pub mod variables;

use forgust_db::ci::Pipeline;
use forgust_db::projects::Project;

use crate::auth::Principal;
use crate::events::Event;
use crate::jobs::RefUpdate;
use crate::state::AppState;

use variables::Var;

/// Create pipelines for pushed branches and tags, and merge request
/// pipelines for open MRs of pushed branches (when the config has jobs for them).
pub async fn on_push(state: &AppState, project_id: i64, updates: &[RefUpdate], pusher: Option<&Principal>) -> anyhow::Result<()> {
    let Some(p) = forgust_db::projects::find(&state.db, project_id).await? else { return Ok(()) };
    // built-in Renovate rebases its branches on a moved default branch
    if let Some(default) = &p.default_branch
        && updates.iter().any(|u| u.name.strip_prefix("refs/heads/") == Some(default.as_str()) && u.new != pipeline::ZERO_OID)
    {
        crate::services::renovate::on_default_branch_push(state, project_id, pusher).await;
        crate::services::scanning::on_default_branch_push(state, project_id).await;
    }
    // each pushed branch's open merge requests, read once for the scanners and
    // the pipelines below
    let mut open: std::collections::HashMap<&str, Vec<forgust_db::merge_requests::MergeRequest>> = std::collections::HashMap::new();
    for u in updates.iter().filter(|u| u.new != pipeline::ZERO_OID) {
        let Some(branch) = u.name.strip_prefix("refs/heads/") else { continue };
        let mrs = forgust_db::merge_requests::open_with_source(&state.db, project_id, branch).await?;
        // built-in scanners look at merge requests' new heads
        for mr in &mrs {
            crate::services::scanning::on_merge_request_head(state, &p, mr, &u.new).await;
        }
        open.insert(branch, mrs);
    }
    // pipelines run as a person: pushes by deploy keys or tokens run none
    let Some(user_id) = pusher.and_then(|p| p.user()).map(|u| u.id) else { return Ok(()) };
    for u in updates {
        if u.new == pipeline::ZERO_OID {
            continue;
        }
        let (git_ref, tag) = match (u.name.strip_prefix("refs/heads/"), u.name.strip_prefix("refs/tags/")) {
            (Some(b), _) => (b, false),
            (_, Some(t)) => (t, true),
            _ => continue,
        };
        let trigger = |source, mr| pipeline::Trigger {
            tag,
            before_sha: &u.old,
            merge_request_id: mr,
            ..pipeline::Trigger::new(project_id, git_ref, &u.new, source, Some(user_id))
        };
        if let Err(e) = pipeline::create(state, trigger("push", None)).await {
            tracing::warn!(project_id, git_ref, error = %e, "pipeline not created");
        }
        if !tag {
            for mr in open.get(git_ref).into_iter().flatten() {
                // the push may be handled after (or while) the MR was opened on
                // this very commit, which starts its pipeline too
                if let Err(e) = mr_pipeline_once(state, mr.id, &u.new, trigger("merge_request_event", Some(mr.id))).await {
                    tracing::warn!(project_id, mr = mr.iid, error = %e, "merge request pipeline not created");
                }
            }
        }
    }
    Ok(())
}

/// A merge request pipeline for the MR's current head (on MR creation).
pub async fn on_merge_request(
    state: &AppState,
    project_id: i64,
    mr: &forgust_db::merge_requests::MergeRequest,
    user_id: i64,
) -> anyhow::Result<()> {
    let Some(sha) = state.storage.resolve(project_id, &format!("refs/heads/{}", mr.source_branch)).await? else { return Ok(()) };
    if let Some(p) = forgust_db::projects::find(&state.db, project_id).await? {
        crate::services::scanning::on_merge_request_head(state, &p, mr, &sha).await;
    }
    // the push of this commit, handled first or at the same time, may start it too
    mr_pipeline_once(
        state,
        mr.id,
        &sha,
        pipeline::Trigger {
            merge_request_id: Some(mr.id),
            ..pipeline::Trigger::new(project_id, &mr.source_branch, &sha, "merge_request_event", Some(user_id))
        },
    )
    .await
}

/// Create a merge request pipeline for `sha` unless one exists: the check and
/// the creation hold a lock on (MR, commit), so a push and the MR's opening
/// handled at the same time start one pipeline, not two.
async fn mr_pipeline_once(state: &AppState, mr_id: i64, sha: &str, trigger: pipeline::Trigger<'_>) -> anyhow::Result<()> {
    let mut lock = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("mr-pipeline:{mr_id}:{sha}"))
        .execute(&mut *lock)
        .await?;
    if !forgust_db::ci::has_mr_pipeline(&state.db, mr_id, sha).await? {
        pipeline::create(state, trigger).await?;
    }
    // released once the pipeline is committed: the next one sees it
    lock.commit().await?;
    Ok(())
}

/// The variables `rules` see when a pipeline is created: predefined, CI/CD
/// settings, then the pipeline's own.
pub async fn rule_variables(
    state: &AppState,
    info: &variables::PipelineInfo<'_>,
    pipeline_vars: &[(String, String)],
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut out: std::collections::HashMap<String, String> =
        variables::pipeline_vars(state, info).into_iter().map(|v| (v.key, v.value)).collect();
    for v in settings_variables(state, info.project, info.protected).await? {
        out.insert(v.key, v.value);
    }
    out.extend(pipeline_vars.iter().cloned());
    Ok(out)
}

/// Whether `p` may run pipelines (and jobs) for `git_ref`: Developer+, and on
/// a protected branch, someone who may push or merge there (as GitLab).
/// Tags have no protection rules yet.
pub async fn may_run(state: &AppState, p: &Principal, proj: &Project, git_ref: &str, tag: bool) -> crate::error::ApiResult<bool> {
    use crate::auth::authz::{ProjectAction, can_project};
    if !can_project(state, p, ProjectAction::Push, proj).await? {
        return Ok(false);
    }
    if tag {
        return Ok(true);
    }
    crate::services::mr::branch_rights(state, p, proj, git_ref, true).await
}

/// `name` as a branch, else as a tag: `(sha, is_tag)`.
pub async fn resolve_ref(state: &AppState, project_id: i64, name: &str) -> anyhow::Result<Option<(String, bool)>> {
    if let Some(sha) = state.storage.resolve(project_id, &format!("refs/heads/{name}")).await? {
        return Ok(Some((sha, false)));
    }
    Ok(state.storage.resolve(project_id, &format!("refs/tags/{name}")).await?.map(|sha| (sha, true)))
}

/// `refs/pipelines/<id>` keeps the pipeline's commit fetchable by its jobs
/// even after the branch moves on (hidden from clients like keep-around refs).
pub async fn pin_commit(state: &AppState, project_id: i64, pipeline_id: i64, sha: &str) {
    if let Err(e) = state.storage.update_ref(project_id, &payload::pipeline_ref(pipeline_id), sha, None).await {
        tracing::warn!(project_id, pipeline_id, error = %e, "pipeline ref not written");
    }
}

pub async fn unpin_commit(state: &AppState, project_id: i64, pipeline_id: i64) {
    if let Err(e) = state.storage.delete_ref(project_id, &payload::pipeline_ref(pipeline_id), None).await {
        tracing::debug!(project_id, pipeline_id, error = %e, "pipeline ref not removed");
    }
}

/// CI/CD variables of the project's groups (outermost first) and the project;
/// protected ones only on protected refs.
pub async fn settings_variables(state: &AppState, project: &Project, protected_ref: bool) -> anyhow::Result<Vec<Var>> {
    let rows = forgust_db::ci_variables::for_project(&state.db, project.id, &project.traversal_ids).await?;
    let mut out = Vec::new();
    for r in rows {
        if r.protected && !protected_ref {
            continue;
        }
        let value = state.secrets.decrypt_str(&r.value_enc)?;
        out.push(Var { key: r.key, value, public: false, masked: r.masked, file: r.variable_type == "file", raw: r.raw });
    }
    Ok(out)
}

/// Side effects of a pipeline status change: MR widgets refresh.
pub async fn on_pipeline_status(state: &AppState, pipeline: &Pipeline) {
    if let Some(mr_id) = pipeline.merge_request_id
        && let Ok(Some(mr)) = forgust_db::merge_requests::find_by_id(&state.db, mr_id).await
    {
        state.events.publish(Event::MergeRequest { project_id: pipeline.project_id, iid: mr.iid });
        crate::services::auto_merge::poke(state, pipeline.project_id, mr.iid).await;
        return;
    }
    // branch pipelines show on MRs of that branch too
    if !pipeline.tag
        && let Ok(mrs) = forgust_db::merge_requests::open_with_source(&state.db, pipeline.project_id, &pipeline.git_ref).await
    {
        for mr in mrs {
            state.events.publish(Event::MergeRequest { project_id: pipeline.project_id, iid: mr.iid });
            crate::services::auto_merge::poke(state, pipeline.project_id, mr.iid).await;
        }
    }
}

/// Periodic CI housekeeping: fail jobs whose runner went silent, retry log
/// archiving, delete expired artifacts.
pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    let silence = state.cfg.ci.stuck_timeout_secs as f64;
    for id in forgust_db::ci::stale_running(&state.db, silence, 600).await? {
        // a canceling job ends canceled; finish_job keeps whatever another
        // path (the runner's final update) recorded first
        pipeline::finish_job(state, id, "failed", Some("stuck_or_timeout_failure"), None).await?;
    }
    for id in forgust_db::ci::unarchived_finished(&state.db, 100).await? {
        if let Err(e) = trace::archive(state, id).await {
            tracing::warn!(build_id = id, error = %e, "trace archiving failed");
        }
    }
    schedule::run_due(state).await?;
    if let Err(e) = crate::services::renovate::run_due(state).await {
        tracing::warn!(error = %e, "renovate runs not started");
    }
    if let Err(e) = crate::services::scanning::run_due(state).await {
        tracing::warn!(error = %e, "security scans not started");
    }
    // delayed jobs whose time came, environments to stop
    for (id, _) in forgust_db::ci::due_scheduled(&state.db).await? {
        if let Err(e) = pipeline::start_waiting(state, id).await {
            tracing::warn!(build_id = id, error = %e, "delayed job not started");
        }
    }
    if let Err(e) = environments::sweep(state).await {
        tracing::warn!(error = %e, "environments sweep failed");
    }
    // expired artifacts: their rows go, a trigger queues their blobs
    while !forgust_db::ci::take_expired_artifacts(&state.db, 500).await?.is_empty() {}
    // the registry's stale uploads, cleanup policies and garbage
    if let Err(e) = crate::registry::sweep(state).await {
        tracing::warn!(error = %e, "registry sweep failed");
    }
    crate::services::uploads::drain_deletions(state).await?;
    Ok(())
}
