//! CI/CD API (M4): runners, pipelines, jobs and their logs, CI settings and
//! variables. The runner-facing protocol lives in `ci::runner_api`.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use forgust_db::ci::{self as cidb, Artifact, Build, Pipeline, ProjectCiSettings, Runner, RunnerManager};
use forgust_db::projects::Project;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::{group_for, project_for};
use crate::auth::authz::{GroupAction, ProjectAction, can_project};
use crate::auth::{Admin, Principal, SignedIn, Viewer};
use crate::ci::{pipeline as pl, runner_api, trace};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// ================================================================ runners

#[derive(Serialize, ToSchema)]
pub struct RunnerDto {
    #[serde(flatten)]
    pub runner: Runner,
    pub managers: Vec<RunnerManager>,
    /// Whether the viewer may edit or delete it.
    pub can_manage: bool,
    /// `online` (contact in the last 2 minutes), `offline`, `never_contacted`, `paused`.
    pub status: String,
}

#[derive(Serialize, ToSchema)]
pub struct NewRunnerToken {
    pub id: i64,
    /// Shown once: `gitlab-runner register --url <url> --token <token>`.
    pub token: String,
}

#[derive(Deserialize, ToSchema)]
pub struct RunnerBody {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "yes")]
    pub run_untagged: bool,
    #[serde(default)]
    pub protected_only: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub maximum_timeout_secs: Option<i32>,
}

fn yes() -> bool {
    true
}

fn runner_status(r: &Runner) -> &'static str {
    if r.paused {
        return "paused";
    }
    match r.contacted_at {
        None => "never_contacted",
        Some(t) if Utc::now() - t < chrono::Duration::minutes(2) => "online",
        Some(_) => "offline",
    }
}

impl RunnerBody {
    /// Normalized tags (checked), and the minimum job timeout applied.
    fn checked(&self) -> ApiResult<(Vec<String>, Option<i32>)> {
        let mut tags: Vec<String> = self.tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        tags.sort();
        tags.dedup();
        if tags.iter().any(|t| t.len() > 64 || t.contains(',')) {
            return Err(ApiError::bad("tags are at most 64 characters, without commas"));
        }
        if tags.is_empty() && !self.run_untagged {
            return Err(ApiError::bad("a runner without tags must run untagged jobs"));
        }
        Ok((tags, self.maximum_timeout_secs.map(|t| t.max(600))))
    }
}

async fn runner_dto(state: &AppState, r: Runner, can_manage: bool) -> ApiResult<RunnerDto> {
    Ok(runner_dtos(state, vec![(r, can_manage)]).await?.pop().expect("one in, one out"))
}

/// Runners with their managers (one query for all of them).
async fn runner_dtos(state: &AppState, runners: Vec<(Runner, bool)>) -> ApiResult<Vec<RunnerDto>> {
    let ids: Vec<i64> = runners.iter().map(|(r, _)| r.id).collect();
    let mut managers = forgust_db::group_by_id(cidb::managers_of(&state.db, &ids).await?.into_iter().map(|m| (m.runner_id, m)).collect());
    Ok(runners
        .into_iter()
        .map(|(r, can_manage)| {
            let status = runner_status(&r).to_string();
            RunnerDto { managers: managers.remove(&r.id).unwrap_or_default(), runner: r, can_manage, status }
        })
        .collect())
}

async fn create_runner(
    state: &AppState,
    kind: &str,
    group_id: Option<i64>,
    project_id: Option<i64>,
    user_id: i64,
    b: RunnerBody,
) -> ApiResult<NewRunnerToken> {
    let (tags, maximum_timeout_secs) = b.checked()?;
    let (token, last4) = runner_api::new_runner_token();
    let id = cidb::create_runner(
        &state.db,
        cidb::NewRunner {
            kind,
            group_id,
            project_id,
            description: b.description.trim(),
            token_hash: &crypto::token_hash(&token),
            token_last4: &last4,
            tags: &tags,
            run_untagged: b.run_untagged,
            protected_only: b.protected_only,
            paused: b.paused,
            maximum_timeout_secs,
            created_by: Some(user_id),
        },
    )
    .await?;
    Ok(NewRunnerToken { id, token })
}

#[utoipa::path(get, path = "/api/v1/admin/runners", responses((status = 200, body = [RunnerDto])), tag = "ci")]
pub async fn instance_runners(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<Vec<RunnerDto>>> {
    let list = cidb::instance_runners(&state.db).await?.into_iter().map(|r| (r, true)).collect();
    Ok(Json(runner_dtos(&state, list).await?))
}

#[utoipa::path(post, path = "/api/v1/admin/runners", request_body = RunnerBody, responses((status = 200, body = NewRunnerToken)), tag = "ci")]
pub async fn create_instance_runner(
    State(state): State<AppState>,
    Admin(me, _): Admin,
    Json(b): Json<RunnerBody>,
) -> ApiResult<Json<NewRunnerToken>> {
    Ok(Json(create_runner(&state, "instance", None, None, me.id, b).await?))
}

#[utoipa::path(get, path = "/api/v1/groups/{id}/runners", responses((status = 200, body = [RunnerDto])), tag = "ci")]
pub async fn group_runners(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<RunnerDto>>> {
    let g = group_for(&state, &p, &id, GroupAction::Admin).await?;
    let list = cidb::runners_of_group(&state.db, g.id).await?.into_iter().map(|r| (r, true)).collect();
    Ok(Json(runner_dtos(&state, list).await?))
}

#[utoipa::path(post, path = "/api/v1/groups/{id}/runners", request_body = RunnerBody, responses((status = 200, body = NewRunnerToken)), tag = "ci")]
pub async fn create_group_runner(
    State(state): State<AppState>,
    SignedIn(me, p): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<RunnerBody>,
) -> ApiResult<Json<NewRunnerToken>> {
    p.may_mint_credentials()?;
    let g = group_for(&state, &p, &id, GroupAction::Admin).await?;
    if g.kind != "group" {
        return Err(ApiError::bad("runners belong to groups, not personal namespaces"));
    }
    Ok(Json(create_runner(&state, "group", Some(g.id), None, me.id, b).await?))
}

/// Runners the project can use (its own, its groups', the instance's).
#[utoipa::path(get, path = "/api/v1/projects/{id}/runners", responses((status = 200, body = [RunnerDto])), tag = "ci")]
pub async fn project_runners(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<RunnerDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    let mut list = Vec::new();
    // whether the viewer manages a group's runners: once per group
    let mut groups: std::collections::HashMap<i64, bool> = std::collections::HashMap::new();
    for r in cidb::runners_for_project(&state.db, proj.id).await? {
        let manage = match (r.project_id, r.group_id) {
            // the project's own: `project_for(Admin)` above said yes
            (Some(id), _) if id == proj.id => true,
            (_, Some(g)) => match groups.get(&g) {
                Some(m) => *m,
                None => *groups.entry(g).or_insert(may_manage_runner(&state, &p, &r).await?),
            },
            _ => may_manage_runner(&state, &p, &r).await?,
        };
        list.push((r, manage));
    }
    Ok(Json(runner_dtos(&state, list).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/runners", request_body = RunnerBody, responses((status = 200, body = NewRunnerToken)), tag = "ci")]
pub async fn create_project_runner(
    State(state): State<AppState>,
    SignedIn(me, p): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<RunnerBody>,
) -> ApiResult<Json<NewRunnerToken>> {
    p.may_mint_credentials()?;
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(create_runner(&state, "project", None, Some(proj.id), me.id, b).await?))
}

async fn may_manage_runner(state: &AppState, p: &Principal, r: &Runner) -> ApiResult<bool> {
    if p.user().is_some_and(|u| u.is_admin) {
        return Ok(true);
    }
    Ok(match (r.kind.as_str(), r.group_id, r.project_id) {
        ("group", Some(g), _) => match forgust_db::namespaces::find(&state.db, g).await? {
            Some(ns) => crate::auth::authz::can_group(state, p, GroupAction::Admin, &ns).await?,
            None => false,
        },
        ("project", _, Some(pid)) => match forgust_db::projects::find(&state.db, pid).await? {
            Some(proj) => can_project(state, p, ProjectAction::Admin, &proj).await?,
            None => false,
        },
        _ => false,
    })
}

async fn managed_runner(state: &AppState, p: &Principal, id: i64) -> ApiResult<Runner> {
    let r = cidb::runner(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    if !may_manage_runner(state, p, &r).await? {
        return Err(ApiError::NotFound);
    }
    Ok(r)
}

#[utoipa::path(get, path = "/api/v1/runners/{id}", responses((status = 200, body = RunnerDto)), tag = "ci")]
pub async fn show_runner(State(state): State<AppState>, SignedIn(_, p): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<RunnerDto>> {
    let r = managed_runner(&state, &p, id).await?;
    Ok(Json(runner_dto(&state, r, true).await?))
}

#[utoipa::path(put, path = "/api/v1/runners/{id}", request_body = RunnerBody, responses((status = 200, body = RunnerDto)), tag = "ci")]
pub async fn update_runner(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<i64>,
    Json(b): Json<RunnerBody>,
) -> ApiResult<Json<RunnerDto>> {
    let r = managed_runner(&state, &p, id).await?;
    let (tags, maximum_timeout_secs) = b.checked()?;
    cidb::update_runner(
        &state.db,
        r.id,
        cidb::RunnerUpdate {
            description: b.description.trim(),
            tags: &tags,
            run_untagged: b.run_untagged,
            protected_only: b.protected_only,
            paused: b.paused,
            maximum_timeout_secs,
        },
    )
    .await?;
    let r = cidb::runner(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(runner_dto(&state, r, true).await?))
}

#[utoipa::path(delete, path = "/api/v1/runners/{id}", responses((status = 200)), tag = "ci")]
pub async fn delete_runner(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let r = managed_runner(&state, &p, id).await?;
    cidb::delete_runner(&state.db, r.id).await?;
    Ok(Json(json!({ "deleted": true })))
}

#[utoipa::path(post, path = "/api/v1/runners/{id}/reset_token", responses((status = 200, body = NewRunnerToken)), tag = "ci")]
pub async fn reset_runner_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<i64>,
) -> ApiResult<Json<NewRunnerToken>> {
    let r = managed_runner(&state, &p, id).await?;
    let token = runner_api::rotate_token(&state, r.id).await?;
    Ok(Json(NewRunnerToken { id: r.id, token }))
}

// ================================================================ pipelines

#[derive(Serialize, ToSchema)]
pub struct JobDto {
    pub id: i64,
    pub pipeline_id: i64,
    pub name: String,
    pub stage: String,
    pub stage_idx: i32,
    pub status: String,
    #[serde(rename = "when")]
    pub when_: String,
    pub allow_failure: bool,
    pub needs: Option<Vec<String>>,
    pub attempt: i32,
    pub retried: bool,
    pub tags: Vec<String>,
    pub failure_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub runner_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub queued_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Seconds spent running (so far, while running).
    pub duration: Option<f64>,
    /// Seconds waiting for a runner.
    pub queued_duration: Option<f64>,
    pub trace_size: i64,
    /// A `trigger` job: it starts a downstream pipeline instead of running.
    pub bridge: bool,
    pub environment: Option<JobEnvironment>,
    /// Asked before someone runs this manual job.
    pub manual_confirmation: Option<String>,
    /// From the log, with `coverage:`.
    pub coverage: Option<f64>,
    /// A delayed job (`scheduled`) starts then.
    pub scheduled_at: Option<DateTime<Utc>>,
    /// The pipeline a bridge started (when the viewer may see it).
    pub downstream: Option<cidb::PipelineLink>,
}

#[derive(Serialize, ToSchema, Clone)]
pub struct JobEnvironment {
    pub name: String,
    /// start, prepare, stop, verify or access.
    pub action: String,
    pub url: Option<String>,
}

/// Seconds from `start` to `end` (or now, while it lasts).
pub fn secs_between(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>) -> Option<f64> {
    start.map(|s| (end.unwrap_or_else(Utc::now) - s).num_milliseconds() as f64 / 1000.0)
}

impl From<&Build> for JobDto {
    fn from(b: &Build) -> Self {
        let spec = pl::StoredSpec::of(b).ok();
        let duration = secs_between(b.started_at, b.finished_at);
        let queued_duration = secs_between(b.queued_at, b.started_at);
        JobDto {
            id: b.id,
            pipeline_id: b.pipeline_id,
            name: b.name.clone(),
            stage: b.stage.clone(),
            stage_idx: b.stage_idx,
            status: b.status.clone(),
            when_: b.when_.clone(),
            allow_failure: b.allow_failure,
            needs: b.needs.clone(),
            attempt: b.attempt,
            retried: b.retried,
            tags: b.tags.clone(),
            failure_reason: b.failure_reason.clone(),
            exit_code: b.exit_code,
            runner_id: b.runner_id,
            created_at: b.created_at,
            queued_at: b.queued_at,
            started_at: b.started_at,
            finished_at: b.finished_at,
            duration,
            queued_duration,
            trace_size: b.trace_size,
            bridge: spec.as_ref().is_some_and(|s| s.job.trigger.is_some()),
            environment: spec.as_ref().and_then(|s| s.job.environment.as_ref()).map(|e| JobEnvironment {
                name: e.name.clone(),
                action: e.action.clone(),
                url: e.url.clone(),
            }),
            manual_confirmation: spec.as_ref().and_then(|s| s.job.manual_confirmation.clone()),
            coverage: None,
            scheduled_at: None,
            downstream: None,
        }
    }
}

/// Fill what a job's row does not hold: coverage, delay, downstream pipeline.
async fn enrich(state: &AppState, viewer: &Principal, jobs: &mut [JobDto], pipeline_id: i64) -> ApiResult<()> {
    let ids: Vec<i64> = jobs.iter().map(|j| j.id).collect();
    let cov: std::collections::HashMap<i64, f64> = cidb::coverages(&state.db, &ids).await?.into_iter().collect();
    let mut down: std::collections::HashMap<i64, cidb::PipelineLink> = std::collections::HashMap::new();
    for (bid, link) in cidb::downstream_pipelines(&state.db, pipeline_id).await? {
        let visible = match forgust_db::projects::find(&state.db, link.project_id).await? {
            Some(dp) => can_project(state, viewer, ProjectAction::ReadCode, &dp).await?,
            None => false,
        };
        if visible {
            down.insert(bid, link);
        }
    }
    for j in jobs.iter_mut() {
        j.coverage = cov.get(&j.id).copied();
        j.downstream = down.remove(&j.id);
        if j.status == "scheduled" {
            j.scheduled_at = cidb::scheduled_at(&state.db, j.id).await?;
        }
    }
    Ok(())
}

#[derive(Serialize, ToSchema)]
pub struct PipelineDetail {
    #[serde(flatten)]
    pub pipeline: Pipeline,
    /// Stage names in order (only stages that have jobs).
    pub stages: Vec<String>,
    /// Latest attempt of each job, plus older attempts with `retried: true`.
    pub jobs: Vec<JobDto>,
    pub duration: Option<f64>,
    pub can_run: bool,
    /// The pipeline whose `trigger` job started this one (when the viewer may see it).
    pub upstream: Option<cidb::PipelineLink>,
    /// Average coverage of its jobs that report one.
    pub coverage: Option<f64>,
}

#[derive(Deserialize)]
pub struct PipelineQuery {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    status: Option<String>,
    sha: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct PipelineSummary {
    #[serde(flatten)]
    pub pipeline: Pipeline,
    /// `[stage, status]` for the mini graph.
    pub stages: Vec<(String, String)>,
    pub duration: Option<f64>,
}

/// Stage names in order (only stages that have jobs).
fn stage_names(builds: &[&Build]) -> Vec<(i32, String)> {
    let mut stages: Vec<(i32, String)> = builds.iter().map(|b| (b.stage_idx, b.stage.clone())).collect();
    stages.sort();
    stages.dedup();
    stages
}

fn stage_statuses(builds: &[&Build]) -> Vec<(String, String)> {
    stage_names(builds)
        .into_iter()
        .map(|(idx, name)| (name, pl::pipeline_status(builds.iter().copied().filter(|b| b.stage_idx == idx)).to_string()))
        .collect()
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/pipelines", responses((status = 200, body = [PipelineSummary])), tag = "ci")]
pub async fn list_pipelines(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<PipelineQuery>,
) -> ApiResult<Json<Vec<PipelineSummary>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let paging = super::Paging { page: q.page.unwrap_or(1), per_page: q.per_page.unwrap_or(20), search: String::new() };
    let list = cidb::list_pipelines(
        &state.db,
        p.id,
        q.git_ref.as_deref().filter(|s| !s.is_empty()),
        q.status.as_deref().filter(|s| !s.is_empty()),
        q.sha.as_deref().filter(|s| !s.is_empty()),
        paging.limit(),
        paging.offset(),
    )
    .await?;
    Ok(Json(summaries(&state, list).await?))
}

/// List rows with their stage statuses (one query for all pipelines).
async fn summaries(state: &AppState, list: Vec<Pipeline>) -> ApiResult<Vec<PipelineSummary>> {
    let ids: Vec<i64> = list.iter().map(|p| p.id).collect();
    let builds = cidb::live_builds_of_pipelines(&state.db, &ids).await?;
    Ok(list
        .into_iter()
        .map(|pipeline| {
            let live: Vec<&Build> = builds.iter().filter(|b| b.pipeline_id == pipeline.id).collect();
            PipelineSummary { stages: stage_statuses(&live), duration: secs_between(pipeline.started_at, pipeline.finished_at), pipeline }
        })
        .collect())
}

async fn pipeline_detail(state: &AppState, p: &Principal, proj: &Project, pipeline: Pipeline) -> ApiResult<PipelineDetail> {
    let builds = cidb::builds_of_pipeline(&state.db, pipeline.id).await?;
    let live: Vec<&Build> = builds.iter().filter(|b| !b.retried).collect();
    let can_run = crate::ci::may_run(state, p, proj, &pipeline.git_ref, pipeline.tag).await?;
    let mut jobs: Vec<JobDto> = builds.iter().map(JobDto::from).collect();
    enrich(state, p, &mut jobs, pipeline.id).await?;
    let covered: Vec<f64> = jobs.iter().filter(|j| !j.retried).filter_map(|j| j.coverage).collect();
    let coverage = (!covered.is_empty()).then(|| covered.iter().sum::<f64>() / covered.len() as f64);
    let upstream = match cidb::upstream_pipeline(&state.db, pipeline.id).await? {
        Some(up) => match forgust_db::projects::find(&state.db, up.project_id).await? {
            Some(upp) if can_project(state, p, ProjectAction::ReadCode, &upp).await? => Some(up),
            _ => None,
        },
        None => None,
    };
    Ok(PipelineDetail {
        stages: stage_names(&live).into_iter().map(|(_, s)| s).collect(),
        jobs,
        duration: secs_between(pipeline.started_at, pipeline.finished_at),
        can_run,
        upstream,
        coverage,
        pipeline,
    })
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/pipelines/{iid}", responses((status = 200, body = PipelineDetail)), tag = "ci")]
pub async fn show_pipeline(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<PipelineDetail>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let pipeline = cidb::pipeline_by_iid(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(pipeline_detail(&state, &v, &p, pipeline).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

#[derive(Deserialize, ToSchema)]
pub struct RunPipeline {
    #[serde(rename = "ref")]
    pub git_ref: String,
    #[serde(default)]
    pub variables: Vec<KeyValue>,
}

/// Run a pipeline for a branch or tag ("Run pipeline").
#[utoipa::path(post, path = "/api/v1/projects/{id}/pipelines", request_body = RunPipeline, responses((status = 200, body = PipelineDetail)), tag = "ci")]
pub async fn run_pipeline(
    State(state): State<AppState>,
    SignedIn(me, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<RunPipeline>,
) -> ApiResult<Json<PipelineDetail>> {
    let proj = project_for(&state, &p, &id, ProjectAction::ReadCode).await?;
    let (sha, tag) = runnable_ref(&state, &p, &proj, &req.git_ref).await?;
    for v in &req.variables {
        check_variable_name(&v.key)?;
    }
    let created = pl::create(
        &state,
        pl::Trigger {
            tag,
            variables: req.variables.iter().map(|v| (v.key.clone(), v.value.clone())).collect(),
            ..pl::Trigger::new(proj.id, &req.git_ref, &sha, "web", Some(me.id))
        },
    )
    .await?;
    let id = match created {
        pl::Created::Pipeline(id) => id,
        pl::Created::NoConfig => return Err(ApiError::bad("this commit has no CI configuration (.gitlab-ci.yml)")),
        pl::Created::Skipped(why) => return Err(ApiError::bad(format!("no pipeline: {why}"))),
    };
    let pipeline = cidb::pipeline(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(pipeline_detail(&state, &p, &proj, pipeline).await?))
}

/// A branch or tag `p` may run pipelines for: `(sha, is_tag)`.
async fn runnable_ref(state: &AppState, p: &Principal, proj: &Project, name: &str) -> ApiResult<(String, bool)> {
    crate::auth::authz::writable(proj)?;
    let (sha, tag) =
        crate::ci::resolve_ref(state, proj.id, name).await?.ok_or_else(|| ApiError::bad(format!("no branch or tag named {name}")))?;
    if !crate::ci::may_run(state, p, proj, name, tag).await? {
        return Err(ApiError::forbidden("you cannot run pipelines for this ref"));
    }
    Ok((sha, tag))
}

fn check_variable_name(key: &str) -> ApiResult<()> {
    if key.is_empty() || key.len() > 255 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ApiError::bad(format!("invalid variable name `{key}`: use letters, digits and underscores")));
    }
    Ok(())
}

async fn runnable_pipeline(state: &AppState, p: &Principal, id: &str, iid: i64) -> ApiResult<(Project, Pipeline)> {
    let proj = project_for(state, p, id, ProjectAction::ReadCode).await?;
    let pipeline = cidb::pipeline_by_iid(&state.db, proj.id, iid).await?.ok_or(ApiError::NotFound)?;
    crate::auth::authz::writable(&proj)?;
    if !crate::ci::may_run(state, p, &proj, &pipeline.git_ref, pipeline.tag).await? {
        return Err(ApiError::forbidden("you cannot run pipelines for this ref"));
    }
    Ok((proj, pipeline))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/pipelines/{iid}/cancel", responses((status = 200, body = PipelineDetail)), tag = "ci")]
pub async fn cancel_pipeline(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<PipelineDetail>> {
    let (proj, pipeline) = runnable_pipeline(&state, &p, &id, iid).await?;
    pl::cancel(&state, pipeline.id).await?;
    let pipeline = cidb::pipeline(&state.db, pipeline.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(pipeline_detail(&state, &p, &proj, pipeline).await?))
}

/// Retry every failed or canceled job.
#[utoipa::path(post, path = "/api/v1/projects/{id}/pipelines/{iid}/retry", responses((status = 200, body = PipelineDetail)), tag = "ci")]
pub async fn retry_pipeline(
    State(state): State<AppState>,
    SignedIn(me, p): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<PipelineDetail>> {
    let (proj, pipeline) = runnable_pipeline(&state, &p, &id, iid).await?;
    for b in cidb::builds_of_pipeline(&state.db, pipeline.id).await? {
        if !b.retried && matches!(b.status.as_str(), "failed" | "canceled") {
            pl::retry_job(&state, &b, Some(me.id)).await?;
        }
    }
    let pipeline = cidb::pipeline(&state.db, pipeline.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(pipeline_detail(&state, &p, &proj, pipeline).await?))
}

#[derive(Deserialize)]
pub struct ShaQuery {
    shas: String,
}

#[derive(Serialize, ToSchema)]
pub struct CommitStatus {
    pub sha: String,
    pub status: String,
    pub pipeline_iid: i64,
}

/// Latest pipeline status of each commit (commit lists, branch lists).
#[utoipa::path(get, path = "/api/v1/projects/{id}/commit_statuses", responses((status = 200, body = [CommitStatus])), tag = "ci")]
pub async fn commit_statuses(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<ShaQuery>,
) -> ApiResult<Json<Vec<CommitStatus>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let shas: Vec<String> = q.shas.split(',').filter(|s| s.len() == 40).take(200).map(String::from).collect();
    Ok(Json(
        cidb::statuses_for_shas(&state.db, p.id, &shas)
            .await?
            .into_iter()
            .map(|(sha, status, pipeline_iid)| CommitStatus { sha, status, pipeline_iid })
            .collect(),
    ))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/pipelines", responses((status = 200, body = [PipelineSummary])), tag = "ci")]
pub async fn mr_pipelines(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Vec<PipelineSummary>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let mr = forgust_db::merge_requests::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let list = cidb::pipelines_of_mr(&state.db, mr.id, &mr.source_branch, p.id, 20).await?;
    let out = summaries(&state, list).await?;
    Ok(Json(out))
}

// ================================================================ jobs

#[derive(Serialize, ToSchema)]
pub struct JobDetail {
    #[serde(flatten)]
    pub job: JobDto,
    pub pipeline_iid: i64,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub sha: String,
    pub runner_description: Option<String>,
    pub artifacts: Vec<Artifact>,
    /// Attempts of this job, newest first (ids).
    pub attempts: Vec<i64>,
    pub can_run: bool,
    pub image: Option<String>,
}

pub(crate) async fn load_job(state: &AppState, p: &Principal, id: &str, job_id: i64) -> ApiResult<(Project, Build)> {
    let proj = project_for(state, p, id, ProjectAction::ReadCode).await?;
    let b = cidb::build(&state.db, job_id).await?.filter(|b| b.project_id == proj.id).ok_or(ApiError::NotFound)?;
    Ok((proj, b))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/jobs/{job_id}", responses((status = 200, body = JobDetail)), tag = "ci")]
pub async fn show_job(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, job_id)): Path<(String, i64)>,
) -> ApiResult<Json<JobDetail>> {
    let (proj, b) = load_job(&state, &v, &id, job_id).await?;
    let pipeline = cidb::pipeline(&state.db, b.pipeline_id).await?.ok_or(ApiError::NotFound)?;
    let runner_description = match b.runner_id {
        Some(r) => cidb::runner(&state.db, r).await?.map(|r| if r.description.is_empty() { format!("#{}", r.id) } else { r.description }),
        None => None,
    };
    let mut attempts: Vec<i64> =
        cidb::builds_of_pipeline(&state.db, b.pipeline_id).await?.into_iter().filter(|o| o.name == b.name).map(|o| o.id).collect();
    attempts.sort_unstable_by(|a, b| b.cmp(a));
    let artifacts = cidb::artifacts_of_build(&state.db, b.id).await?.into_iter().filter(|a| a.file_type != "trace").collect();
    let can_run = crate::ci::may_run(&state, &v, &proj, &pipeline.git_ref, pipeline.tag).await?;
    let image = pl::StoredSpec::of(&b).ok().and_then(|s| s.job.image).map(|i| i.name);
    let mut job = [JobDto::from(&b)];
    enrich(&state, &v, &mut job, b.pipeline_id).await?;
    let [job] = job;
    Ok(Json(JobDetail {
        job,
        pipeline_iid: pipeline.iid,
        git_ref: pipeline.git_ref,
        sha: pipeline.sha,
        runner_description,
        artifacts,
        attempts,
        can_run,
        image,
    }))
}

#[derive(Deserialize)]
pub struct TraceQuery {
    #[serde(default)]
    offset: i64,
}

#[derive(Serialize, ToSchema)]
pub struct TraceChunk {
    /// Byte offset of `content` in the log.
    pub offset: i64,
    /// Total size known now.
    pub size: i64,
    /// Log text from `offset` (raw bytes as UTF-8, invalid sequences replaced).
    pub content: String,
    pub status: String,
    /// The job ended and this is the end of its log: no more bytes will come.
    pub complete: bool,
    /// More bytes are there already: read again from `size` (a log is sent in
    /// pieces of at most 1 MB).
    pub more: bool,
}

/// Largest piece of a log one trace read returns.
const TRACE_PIECE: i64 = 1 << 20;

/// Log bytes from `offset`; poll again (or follow SSE `job` events) for more.
#[utoipa::path(get, path = "/api/v1/projects/{id}/jobs/{job_id}/trace", responses((status = 200, body = TraceChunk)), tag = "ci")]
pub async fn job_trace(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, job_id)): Path<(String, i64)>,
    Query(q): Query<TraceQuery>,
) -> ApiResult<Json<TraceChunk>> {
    let (_, b) = load_job(&state, &v, &id, job_id).await?;
    let offset = q.offset.clamp(0, b.trace_size);
    let bytes = trace::read(&state, &b, offset, TRACE_PIECE).await?;
    let more = offset + (bytes.len() as i64) < b.trace_size;
    // never split a UTF-8 sequence: an incomplete tail waits for the next read
    let ended = pl::is_complete(&b.status);
    let end = match std::str::from_utf8(&bytes) {
        Err(e) if e.error_len().is_none() && (more || !ended) => e.valid_up_to(),
        _ => bytes.len(),
    };
    Ok(Json(TraceChunk {
        offset,
        size: offset + end as i64,
        content: String::from_utf8_lossy(&bytes[..end]).into_owned(),
        status: b.status.clone(),
        complete: ended && !more,
        more,
    }))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/jobs/{job_id}/raw", responses((status = 200, content_type = "text/plain")), tag = "ci")]
pub async fn job_raw(State(state): State<AppState>, Viewer(v): Viewer, Path((id, job_id)): Path<(String, i64)>) -> ApiResult<Response> {
    let (_, b) = load_job(&state, &v, &id, job_id).await?;
    if b.trace_archived
        && let Some(r) = trace::blob_download(&state, &trace::blob_key(b.project_id, b.id), "text/plain; charset=utf-8", None).await?
    {
        return Ok(r);
    }
    let bytes = trace::read(&state, &b, 0, i64::MAX).await?;
    let mut r = Response::new(Body::from(bytes));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    r.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(r)
}

async fn runnable_job(state: &AppState, p: &Principal, id: &str, job_id: i64) -> ApiResult<Build> {
    let (proj, b) = load_job(state, p, id, job_id).await?;
    let pipeline = cidb::pipeline(&state.db, b.pipeline_id).await?.ok_or(ApiError::NotFound)?;
    if !crate::ci::may_run(state, p, &proj, &pipeline.git_ref, pipeline.tag).await? {
        return Err(ApiError::forbidden("you cannot run jobs for this ref"));
    }
    Ok(b)
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/jobs/{job_id}/cancel", responses((status = 200, body = JobDto)), tag = "ci")]
pub async fn cancel_job(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, job_id)): Path<(String, i64)>,
) -> ApiResult<Json<JobDto>> {
    let b = runnable_job(&state, &p, &id, job_id).await?;
    pl::cancel_job(&state, &b).await?;
    let b = cidb::build(&state.db, b.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(JobDto::from(&b)))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/jobs/{job_id}/retry", responses((status = 200, body = JobDto)), tag = "ci")]
pub async fn retry_job(
    State(state): State<AppState>,
    SignedIn(me, p): SignedIn,
    Path((id, job_id)): Path<(String, i64)>,
) -> ApiResult<Json<JobDto>> {
    let b = runnable_job(&state, &p, &id, job_id).await?;
    if !pl::is_complete(&b.status) || b.retried {
        return Err(ApiError::conflict("only the latest attempt of a finished job can be retried"));
    }
    let new_id = pl::retry_job(&state, &b, Some(me.id)).await?;
    let b = cidb::build(&state.db, new_id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(JobDto::from(&b)))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/jobs/{job_id}/play", responses((status = 200, body = JobDto)), tag = "ci")]
pub async fn play_job(
    State(state): State<AppState>,
    SignedIn(me, p): SignedIn,
    Path((id, job_id)): Path<(String, i64)>,
) -> ApiResult<Json<JobDto>> {
    let b = runnable_job(&state, &p, &id, job_id).await?;
    if b.status != "manual" && b.status != "scheduled" {
        return Err(ApiError::conflict("only manual and delayed jobs can be started"));
    }
    pl::play_job(&state, &b, Some(me.id)).await?;
    let b = cidb::build(&state.db, b.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(JobDto::from(&b)))
}

/// A delayed job waits for someone instead (it becomes manual).
pub async fn unschedule_job(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, job_id)): Path<(String, i64)>,
) -> ApiResult<Json<JobDto>> {
    let b = runnable_job(&state, &p, &id, job_id).await?;
    let mut tx = state.db.begin().await?;
    let Some(locked) = cidb::build_for_update(&mut tx, b.id).await? else { return Err(ApiError::NotFound) };
    if locked.status != "scheduled" {
        return Err(ApiError::conflict("only delayed jobs can be unscheduled"));
    }
    cidb::set_build_status(&mut *tx, b.id, "manual", None).await?;
    cidb::set_scheduled_at(&mut *tx, b.id, None).await?;
    tx.commit().await?;
    crate::ci::hooks::announce(&state, &b, "manual").await;
    pl::process(&state, b.pipeline_id).await?;
    let b = cidb::build(&state.db, b.id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(JobDto::from(&b)))
}

/// Download a job's artifacts archive.
#[utoipa::path(get, path = "/api/v1/projects/{id}/jobs/{job_id}/artifacts", responses((status = 200, content_type = "application/zip")), tag = "ci")]
pub async fn job_artifacts(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, job_id)): Path<(String, i64)>,
) -> ApiResult<Response> {
    let (_, b) = load_job(&state, &v, &id, job_id).await?;
    let a = cidb::artifact(&state.db, b.id, "archive").await?.ok_or(ApiError::NotFound)?;
    let name = format!("{}-{}-artifacts.zip", b.name, b.id);
    let disposition = crate::blob::disposition(&name, false);
    if let Some(url) =
        state.blobs.presigned_download(&a.blob_key, std::time::Duration::from_secs(300), "application/zip", &disposition).await?
    {
        return Ok((StatusCode::FOUND, [(header::LOCATION, url.to_string())]).into_response());
    }
    trace::blob_download(&state, &a.blob_key, "application/zip", Some(&name)).await?.ok_or(ApiError::NotFound)
}

#[derive(Deserialize)]
pub struct ArtifactPath {
    #[serde(default)]
    path: String,
}

#[derive(Serialize, ToSchema)]
pub struct ArtifactListing {
    /// The folder listed (`""` for the top, else ending with `/`).
    pub path: String,
    pub entries: Vec<crate::ci::archive::Listed>,
    /// The archive has more entries than can be listed.
    pub truncated: bool,
}

/// The archive of a job, readable by the viewer and not expired.
async fn job_archive(state: &AppState, v: &Principal, id: &str, job_id: i64) -> ApiResult<Artifact> {
    let (_, b) = load_job(state, v, id, job_id).await?;
    cidb::artifact(&state.db, b.id, "archive").await?.filter(|a| a.file_format == "zip").ok_or(ApiError::NotFound)
}

/// One folder of a job's artifacts archive.
#[utoipa::path(get, path = "/api/v1/projects/{id}/jobs/{job_id}/artifacts/tree", responses((status = 200, body = ArtifactListing)), tag = "ci")]
pub async fn artifact_tree(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, job_id)): Path<(String, i64)>,
    Query(q): Query<ArtifactPath>,
) -> ApiResult<Json<ArtifactListing>> {
    let a = job_archive(&state, &v, &id, job_id).await?;
    let dir = q.path.trim_start_matches('/');
    let dir = if dir.is_empty() || dir.ends_with('/') { dir.to_string() } else { format!("{dir}/") };
    let (all, truncated) =
        crate::ci::archive::entries(&state, &a.blob_key).await.map_err(|e| ApiError::bad(format!("the archive cannot be read: {e}")))?;
    let entries = crate::ci::archive::children(&all, &dir);
    if !dir.is_empty() && entries.is_empty() {
        return Err(ApiError::NotFound);
    }
    Ok(Json(ArtifactListing { path: dir, entries, truncated }))
}

/// One file of a job's artifacts archive (text and a few media types shown,
/// anything else downloaded).
#[utoipa::path(get, path = "/api/v1/projects/{id}/jobs/{job_id}/artifacts/file", responses((status = 200)), tag = "ci")]
pub async fn artifact_file(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, job_id)): Path<(String, i64)>,
    Query(q): Query<ArtifactPath>,
) -> ApiResult<Response> {
    let a = job_archive(&state, &v, &id, job_id).await?;
    serve_artifact_file(&state, &a, q.path.trim_start_matches('/')).await
}

pub async fn serve_artifact_file(state: &AppState, a: &Artifact, path: &str) -> ApiResult<Response> {
    let (all, _) =
        crate::ci::archive::entries(state, &a.blob_key).await.map_err(|e| ApiError::bad(format!("the archive cannot be read: {e}")))?;
    let e = all.iter().find(|e| e.name == path && !e.is_dir()).ok_or(ApiError::NotFound)?;
    let stream = crate::ci::archive::open(state, &a.blob_key, e).await.map_err(|err| ApiError::bad(err.to_string()))?;
    Ok(crate::services::uploads::safe_file(Body::from_stream(stream), &e.name, Some(e.size), true))
}

// ================================================================ settings and variables

#[utoipa::path(get, path = "/api/v1/projects/{id}/ci/settings", responses((status = 200, body = ProjectCiSettings)), tag = "ci")]
pub async fn ci_settings(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<ProjectCiSettings>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(cidb::settings(&state.db, proj.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct CiSettingsBody {
    pub ci_config_path: String,
    pub instance_runners_enabled: bool,
    pub default_timeout_secs: i32,
    pub default_git_depth: i32,
    pub only_merge_if_pipeline_succeeds: bool,
    pub job_token_scope_enabled: bool,
    /// Cancel older pipelines of a branch when a new commit's pipeline starts.
    #[serde(default = "yes")]
    pub auto_cancel_redundant: bool,
    /// Merge requests merge through a merge train (needs pipelines to succeed).
    #[serde(default)]
    pub merge_trains_enabled: bool,
    /// No new critical or high security finding left undismissed (a merge check).
    #[serde(default)]
    pub block_new_security_findings: bool,
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/ci/settings", request_body = CiSettingsBody, responses((status = 200, body = ProjectCiSettings)), tag = "ci")]
pub async fn update_ci_settings(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<CiSettingsBody>,
) -> ApiResult<Json<ProjectCiSettings>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    let path = b.ci_config_path.trim();
    if path.is_empty() || path.starts_with('/') || path.contains("..") || path.len() > 255 {
        return Err(ApiError::bad("the CI configuration path must be a relative file path in the repository"));
    }
    if !(600..=2_592_000).contains(&b.default_timeout_secs) {
        return Err(ApiError::bad("the job timeout must be between 10 minutes and 30 days"));
    }
    if !(0..=1000).contains(&b.default_git_depth) {
        return Err(ApiError::bad("the git depth must be between 0 and 1000"));
    }
    let s = ProjectCiSettings {
        project_id: proj.id,
        ci_config_path: path.to_string(),
        instance_runners_enabled: b.instance_runners_enabled,
        default_timeout_secs: b.default_timeout_secs,
        default_git_depth: b.default_git_depth,
        only_merge_if_pipeline_succeeds: b.only_merge_if_pipeline_succeeds,
        job_token_scope_enabled: b.job_token_scope_enabled,
        auto_cancel_redundant: b.auto_cancel_redundant,
        merge_trains_enabled: b.merge_trains_enabled,
        block_new_security_findings: b.block_new_security_findings,
    };
    if s.merge_trains_enabled && !s.only_merge_if_pipeline_succeeds {
        return Err(ApiError::bad("merge trains need \"pipelines must succeed\" (a train merges what its pipeline tested)"));
    }
    cidb::update_settings(&state.db, &s).await?;
    Ok(Json(s))
}

#[derive(Serialize, ToSchema)]
pub struct VariableDto {
    pub key: String,
    /// Absent for masked variables: they are write-only.
    pub value: Option<String>,
    pub variable_type: String,
    pub protected: bool,
    pub masked: bool,
    pub raw: bool,
    pub description: String,
}

#[derive(Deserialize, ToSchema)]
pub struct VariableBody {
    pub key: String,
    /// Omit to keep the current value (masked variables are write-only).
    pub value: Option<String>,
    #[serde(default = "env_var")]
    pub variable_type: String,
    /// Omit to keep the current setting (false for a new variable).
    #[serde(default)]
    pub protected: Option<bool>,
    #[serde(default)]
    pub masked: Option<bool>,
    #[serde(default)]
    pub raw: Option<bool>,
    #[serde(default)]
    pub description: String,
}

fn env_var() -> String {
    "env_var".into()
}

/// GitLab's masking rules: one line, at least 8 characters, no spaces.
pub fn maskable(value: &str) -> bool {
    value.len() >= 8 && !value.chars().any(|c| c.is_whitespace())
}

fn to_dto(state: &AppState, v: forgust_db::ci_variables::Variable) -> ApiResult<VariableDto> {
    let value = if v.masked { None } else { Some(state.secrets.decrypt_str(&v.value_enc)?) };
    Ok(VariableDto {
        key: v.key,
        value,
        variable_type: v.variable_type,
        protected: v.protected,
        masked: v.masked,
        raw: v.raw,
        description: v.description,
    })
}

/// The variables of a project or a group, values hidden when masked.
async fn variable_dtos(state: &AppState, project_id: Option<i64>, group_id: Option<i64>) -> ApiResult<Vec<VariableDto>> {
    forgust_db::ci_variables::list(&state.db, project_id, group_id).await?.into_iter().map(|v| to_dto(state, v)).collect()
}

async fn save_variable(state: &AppState, project_id: Option<i64>, group_id: Option<i64>, b: VariableBody) -> ApiResult<()> {
    check_variable_name(&b.key)?;
    if !["env_var", "file"].contains(&b.variable_type.as_str()) {
        return Err(ApiError::bad("variable_type is env_var or file"));
    }
    let existing = forgust_db::ci_variables::list(&state.db, project_id, group_id).await?.into_iter().find(|v| v.key == b.key);
    let value = match (&b.value, &existing) {
        (Some(v), _) => v.clone(),
        (None, Some(e)) => state.secrets.decrypt_str(&e.value_enc)?,
        (None, None) => return Err(ApiError::bad("a value is required")),
    };
    if value.len() > 10_000 {
        return Err(ApiError::bad("values are at most 10,000 characters"));
    }
    let keep = |asked: Option<bool>, now: Option<bool>| asked.or(now).unwrap_or(false);
    let masked = keep(b.masked, existing.as_ref().map(|e| e.masked));
    let protected = keep(b.protected, existing.as_ref().map(|e| e.protected));
    let raw = keep(b.raw, existing.as_ref().map(|e| e.raw));
    // a masked value is write-only: unmasking it (which shows it) needs it again
    if b.value.is_none() && existing.as_ref().is_some_and(|e| e.masked) && !masked {
        return Err(ApiError::bad("enter the value again to unmask this variable"));
    }
    if masked && !maskable(&value) {
        return Err(ApiError::bad("masked values need at least 8 characters on one line, without spaces"));
    }
    let enc = state.secrets.encrypt(value.as_bytes())?;
    forgust_db::ci_variables::upsert(
        &state.db,
        project_id,
        group_id,
        forgust_db::ci_variables::VariableInput {
            key: &b.key,
            value_enc: &enc,
            variable_type: &b.variable_type,
            protected,
            masked,
            raw,
            description: b.description.trim(),
        },
    )
    .await?;
    Ok(())
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/variables", responses((status = 200, body = [VariableDto])), tag = "ci")]
pub async fn project_variables(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<VariableDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(variable_dtos(&state, Some(proj.id), None).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/variables", request_body = VariableBody, responses((status = 200, body = [VariableDto])), tag = "ci")]
pub async fn set_project_variable(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<VariableBody>,
) -> ApiResult<Json<Vec<VariableDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    save_variable(&state, Some(proj.id), None, b).await?;
    Ok(Json(variable_dtos(&state, Some(proj.id), None).await?))
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/variables/{key}", responses((status = 200)), tag = "ci")]
pub async fn delete_project_variable(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, key)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    if !forgust_db::ci_variables::delete(&state.db, Some(proj.id), None, &key).await? {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

#[utoipa::path(get, path = "/api/v1/groups/{id}/variables", responses((status = 200, body = [VariableDto])), tag = "ci")]
pub async fn group_variables(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<VariableDto>>> {
    let g = group_for(&state, &p, &id, GroupAction::Admin).await?;
    Ok(Json(variable_dtos(&state, None, Some(g.id)).await?))
}

#[utoipa::path(post, path = "/api/v1/groups/{id}/variables", request_body = VariableBody, responses((status = 200, body = [VariableDto])), tag = "ci")]
pub async fn set_group_variable(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<VariableBody>,
) -> ApiResult<Json<Vec<VariableDto>>> {
    let g = group_for(&state, &p, &id, GroupAction::Admin).await?;
    save_variable(&state, None, Some(g.id), b).await?;
    Ok(Json(variable_dtos(&state, None, Some(g.id)).await?))
}

#[utoipa::path(delete, path = "/api/v1/groups/{id}/variables/{key}", responses((status = 200)), tag = "ci")]
pub async fn delete_group_variable(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, key)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    let g = group_for(&state, &p, &id, GroupAction::Admin).await?;
    if !forgust_db::ci_variables::delete(&state.db, None, Some(g.id), &key).await? {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

// ================================================================ job token scope

#[derive(Deserialize, ToSchema)]
pub struct AllowProject {
    /// Path (`group/project`) or numeric id of the project whose job tokens may read this one.
    pub project: String,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/job_token_allowlist", responses((status = 200, body = [forgust_db::ci::AllowedProject])), tag = "ci")]
pub async fn job_token_allowlist(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<forgust_db::ci::AllowedProject>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(cidb::allowlist(&state.db, proj.id).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/job_token_allowlist", request_body = AllowProject, responses((status = 200, body = [forgust_db::ci::AllowedProject])), tag = "ci")]
pub async fn allow_job_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<AllowProject>,
) -> ApiResult<Json<Vec<forgust_db::ci::AllowedProject>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    // you can only name projects you can see
    let source = project_for(&state, &p, b.project.trim().trim_matches('/'), ProjectAction::Read).await?;
    if source.id == proj.id {
        return Err(ApiError::bad("a project's own jobs always have access"));
    }
    cidb::allow(&state.db, proj.id, source.id).await?;
    Ok(Json(cidb::allowlist(&state.db, proj.id).await?))
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/job_token_allowlist/{source_id}", responses((status = 200)), tag = "ci")]
pub async fn disallow_job_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, source_id)): Path<(String, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    if !cidb::disallow(&state.db, proj.id, source_id).await? {
        return Err(ApiError::NotFound);
    }
    Ok(Json(json!({ "deleted": true })))
}

// ================================================================ schedules

#[derive(Serialize, ToSchema)]
pub struct ScheduleDto {
    #[serde(flatten)]
    pub schedule: forgust_db::ci::Schedule,
    pub variables: Vec<KeyValueOut>,
    pub last_pipeline: Option<(i64, String)>,
}

#[derive(Serialize, ToSchema)]
pub struct KeyValueOut {
    pub key: String,
    pub value: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ScheduleBody {
    #[serde(default)]
    pub description: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub cron: String,
    #[serde(default = "utc")]
    pub cron_timezone: String,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub variables: Vec<KeyValue>,
}

fn utc() -> String {
    "UTC".into()
}

/// Whether `p` may change, run or read the values of schedule `s`: its owner or a maintainer.
async fn owns_schedule(state: &AppState, p: &Principal, proj: &Project, s: &forgust_db::ci::Schedule) -> ApiResult<bool> {
    Ok(s.owner_id.is_some() && s.owner_id == p.user().map(|u| u.id) || can_project(state, p, ProjectAction::Admin, proj).await?)
}

/// A schedule; its variables' values only for `owner` (they may be the
/// secrets a protected branch's pipeline needs).
async fn schedule_dto(state: &AppState, s: forgust_db::ci::Schedule, owner: bool) -> ApiResult<ScheduleDto> {
    let mut variables: Vec<(String, String)> = match &s.variables_enc {
        Some(enc) => state.secrets.decrypt_json(enc)?,
        None => Vec::new(),
    };
    if !owner {
        variables.iter_mut().for_each(|(_, v)| v.clear());
    }
    let last_pipeline = match s.last_pipeline_id {
        Some(id) => cidb::pipeline(&state.db, id).await?.map(|p| (p.iid, p.status)),
        None => None,
    };
    Ok(ScheduleDto {
        variables: variables.into_iter().map(|(key, value)| KeyValueOut { key, value }).collect(),
        last_pipeline,
        schedule: s,
    })
}

async fn schedule_input<'a>(
    state: &AppState,
    p: &Principal,
    proj: &Project,
    b: &'a ScheduleBody,
    enc: &'a mut Option<String>,
) -> ApiResult<cidb::ScheduleInput<'a>> {
    crate::ci::schedule::validate(b.cron.trim(), b.cron_timezone.trim()).map_err(ApiError::BadRequest)?;
    runnable_ref(state, p, proj, &b.git_ref).await?;
    for v in &b.variables {
        check_variable_name(&v.key)?;
    }
    *enc = if b.variables.is_empty() {
        None
    } else {
        let pairs: Vec<(&str, &str)> = b.variables.iter().map(|v| (v.key.as_str(), v.value.as_str())).collect();
        Some(state.secrets.encrypt_json(&pairs)?)
    };
    Ok(cidb::ScheduleInput {
        description: b.description.trim(),
        git_ref: &b.git_ref,
        cron: b.cron.trim(),
        cron_timezone: b.cron_timezone.trim(),
        active: b.active,
        variables_enc: enc.as_deref(),
        next_run_at: if b.active { crate::ci::schedule::next_run(b.cron.trim(), b.cron_timezone.trim(), Utc::now()) } else { None },
    })
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/pipeline_schedules", responses((status = 200, body = [ScheduleDto])), tag = "ci")]
pub async fn list_schedules(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<ScheduleDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Push).await?;
    let mut out = Vec::new();
    // Maintainer or not: asked once, and only when a schedule is someone
    // else's (an owner's own list causes no admin check, nor its audit)
    let mut maintainer: Option<bool> = None;
    let me = p.user().map(|u| u.id);
    for s in cidb::schedules(&state.db, proj.id).await? {
        let owner = if s.owner_id.is_some() && s.owner_id == me {
            true
        } else {
            match maintainer {
                Some(m) => m,
                None => *maintainer.insert(can_project(&state, &p, ProjectAction::Admin, &proj).await?),
            }
        };
        out.push(schedule_dto(&state, s, owner).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/pipeline_schedules", request_body = ScheduleBody, responses((status = 200, body = ScheduleDto)), tag = "ci")]
pub async fn create_schedule(
    State(state): State<AppState>,
    SignedIn(me, p): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<ScheduleBody>,
) -> ApiResult<Json<ScheduleDto>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Push).await?;
    let mut enc = None;
    let input = schedule_input(&state, &p, &proj, &b, &mut enc).await?;
    let sid = cidb::create_schedule(&state.db, proj.id, me.id, input).await?;
    let s = cidb::schedule(&state.db, proj.id, sid).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(schedule_dto(&state, s, true).await?))
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/pipeline_schedules/{sid}", request_body = ScheduleBody, responses((status = 200, body = ScheduleDto)), tag = "ci")]
pub async fn update_schedule(
    State(state): State<AppState>,
    SignedIn(me, p): SignedIn,
    Path((id, sid)): Path<(String, i64)>,
    Json(b): Json<ScheduleBody>,
) -> ApiResult<Json<ScheduleDto>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Push).await?;
    let current = cidb::schedule(&state.db, proj.id, sid).await?.ok_or(ApiError::NotFound)?;
    // saving makes you the owner: someone else's schedule takes a maintainer
    if !owns_schedule(&state, &p, &proj, &current).await? {
        return Err(ApiError::forbidden("only the owner or a maintainer can edit this schedule"));
    }
    let mut enc = None;
    let input = schedule_input(&state, &p, &proj, &b, &mut enc).await?;
    cidb::update_schedule(&state.db, sid, me.id, input).await?;
    let s = cidb::schedule(&state.db, proj.id, sid).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(schedule_dto(&state, s, true).await?))
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/pipeline_schedules/{sid}", responses((status = 200)), tag = "ci")]
pub async fn delete_schedule(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, sid)): Path<(String, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Push).await?;
    let s = cidb::schedule(&state.db, proj.id, sid).await?.ok_or(ApiError::NotFound)?;
    // others' schedules: maintainers only
    if !owns_schedule(&state, &p, &proj, &s).await? {
        return Err(ApiError::forbidden("only the owner or a maintainer can delete this schedule"));
    }
    cidb::delete_schedule(&state.db, proj.id, sid).await?;
    Ok(Json(json!({ "deleted": true })))
}

/// Run a schedule's pipeline now (as its owner).
#[utoipa::path(post, path = "/api/v1/projects/{id}/pipeline_schedules/{sid}/play", responses((status = 200)), tag = "ci")]
pub async fn play_schedule(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, sid)): Path<(String, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Push).await?;
    let s = cidb::schedule(&state.db, proj.id, sid).await?.ok_or(ApiError::NotFound)?;
    // it runs as its owner: anyone else would borrow the owner's rights
    if !owns_schedule(&state, &p, &proj, &s).await? {
        return Err(ApiError::forbidden("only the owner or a maintainer can run this schedule"));
    }
    // the caller must be allowed to run pipelines for the ref, too
    runnable_ref(&state, &p, &proj, &s.git_ref).await?;
    let id = crate::ci::schedule::run(&state, &s).await.map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if let Some(pid) = id {
        cidb::schedule_ran(&state.db, s.id, s.next_run_at, Some(pid)).await?;
    }
    let iid = match id {
        Some(pid) => cidb::pipeline(&state.db, pid).await?.map(|p| p.iid),
        None => None,
    };
    Ok(Json(json!({ "pipeline_iid": iid })))
}
