//! The GitLab Runner API under `/api/v4` (ADR-0003, ADR-0017), as the stock
//! `gitlab-runner` calls it. Status codes and headers follow GitLab's
//! `lib/api/ci/runner.rb` and the runner's `network/gitlab.go`.
// helpers return the ready HTTP response to send when a request is refused
#![allow(clippy::result_large_err)]

use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Router, body::Bytes};
use forgust_db::ci::{Append, Build, Runner};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::ClientIp;
use crate::ci::{payload, pipeline, variables};
use crate::crypto;
use crate::events::Event;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/runners", post(legacy_register).delete(legacy_unregister))
        .route("/runners/verify", post(verify))
        .route("/runners/reset_authentication_token", post(reset_token))
        .route("/runners/managers", delete(delete_manager))
        .route("/runners/router/discovery", get(router_discovery))
        .route("/jobs/request", post(request_job))
        .route("/jobs/{id}", put(update_job))
        // the runner sends at most 1 MiB per patch (DefaultTracePatchLimit)
        .route("/jobs/{id}/trace", patch(patch_trace).layer(DefaultBodyLimit::max(4 << 20)))
        .route("/jobs/{id}/artifacts", post(upload_artifacts).get(download_artifacts).layer(DefaultBodyLimit::disable()))
}

fn plain(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "message": msg }))).into_response()
}

fn forbidden() -> Response {
    plain(StatusCode::FORBIDDEN, "403 Forbidden")
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "runner API error");
    plain(StatusCode::INTERNAL_SERVER_ERROR, "500 Internal Server Error")
}

pub const RUNNER_TOKEN_PREFIX: &str = "glrt-";

/// A new `glrt-` token and its last four characters (shown to tell tokens apart).
pub fn new_runner_token() -> (String, String) {
    let token = crypto::random_token(RUNNER_TOKEN_PREFIX);
    let last4 = token[token.len() - 4..].to_string();
    (token, last4)
}

/// Replace a runner's token; returns the new one (shown once).
pub async fn rotate_token(state: &AppState, runner_id: i64) -> anyhow::Result<String> {
    let (token, last4) = new_runner_token();
    forgust_db::ci::set_runner_token(&state.db, runner_id, &crypto::token_hash(&token), &last4).await?;
    Ok(token)
}

/// The runner a `glrt-` token belongs to, or the response to send instead.
async fn authed_runner(state: &AppState, token: &str) -> Result<Runner, Response> {
    if token.is_empty() {
        return Err(forbidden());
    }
    match forgust_db::ci::runner_by_token(&state.db, &crypto::token_hash(token)).await {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(forbidden()),
        Err(e) => Err(internal(e)),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Info {
    version: String,
    revision: String,
    platform: String,
    architecture: String,
    executor: String,
}

fn manager_info<'a>(i: &'a Info, ip: &'a str) -> forgust_db::ci::ManagerInfo<'a> {
    forgust_db::ci::ManagerInfo {
        version: &i.version,
        revision: &i.revision,
        platform: &i.platform,
        architecture: &i.architecture,
        executor: &i.executor,
        ip_address: ip,
    }
}

// ---------------------------------------------------------------- runners

/// What `verify` and `jobs/request` send.
#[derive(Deserialize)]
struct RunnerReq {
    #[serde(default)]
    token: String,
    #[serde(default)]
    system_id: String,
    #[serde(default)]
    info: Info,
}

async fn verify(State(state): State<AppState>, ip: ClientIp, Json(req): Json<RunnerReq>) -> Response {
    let r = match authed_runner(&state, &req.token).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !req.system_id.is_empty()
        && let Err(e) =
            forgust_db::ci::touch_manager(&state.db, r.id, &req.system_id, manager_info(&req.info, &ip.text().unwrap_or_default())).await
    {
        return internal(e);
    }
    (StatusCode::OK, Json(json!({ "id": r.id, "token": req.token, "token_expires_at": null }))).into_response()
}

#[derive(Deserialize)]
struct TokenReq {
    #[serde(default)]
    token: String,
}

async fn reset_token(State(state): State<AppState>, Json(req): Json<TokenReq>) -> Response {
    let r = match authed_runner(&state, &req.token).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let token = match rotate_token(&state, r.id).await {
        Ok(t) => t,
        Err(e) => return internal(e),
    };
    (StatusCode::CREATED, Json(json!({ "token": token, "token_expires_at": null }))).into_response()
}

#[derive(Deserialize)]
struct ManagerReq {
    #[serde(default)]
    token: String,
    #[serde(default)]
    system_id: String,
}

async fn delete_manager(State(state): State<AppState>, Json(req): Json<ManagerReq>) -> Response {
    let r = match authed_runner(&state, &req.token).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match forgust_db::ci::delete_manager(&state.db, r.id, &req.system_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => plain(StatusCode::NOT_FOUND, "404 Runner manager not found"),
        Err(e) => internal(e),
    }
}

/// Registration tokens are GitLab's legacy flow (ADR-0017): runners are
/// created in Forgust and use their `glrt-` token with `gitlab-runner register`.
async fn legacy_register() -> Response {
    plain(StatusCode::GONE, "Runner registration tokens are disabled. Create the runner in Forgust and register with its glrt- token.")
}

async fn legacy_unregister() -> Response {
    plain(StatusCode::FORBIDDEN, "Runners are deleted in Forgust; gitlab-runner unregister removes this machine only.")
}

async fn router_discovery() -> Response {
    plain(StatusCode::NOT_IMPLEMENTED, "Job Router is not available.")
}

// ---------------------------------------------------------------- jobs/request

async fn request_job(State(state): State<AppState>, ip: ClientIp, Json(req): Json<RunnerReq>) -> Response {
    let runner = match authed_runner(&state, &req.token).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let system_id = if req.system_id.is_empty() { "default".to_string() } else { req.system_id.clone() };
    let manager_id = match forgust_db::ci::touch_manager(
        &state.db,
        runner.id,
        &system_id,
        manager_info(&req.info, &ip.text().unwrap_or_default()),
    )
    .await
    {
        Ok(id) => id,
        Err(e) => return internal(e),
    };
    if runner.paused {
        return no_job();
    }
    // subscribe before the first look, so a job queued in between still wakes us
    let mut wake = state.events.subscribe();
    let deadline = Instant::now() + Duration::from_secs(state.cfg.ci.long_poll_secs);
    loop {
        match try_claim(&state, &runner, manager_id).await {
            Ok(Claimed::Job(body)) => {
                metrics::counter!("forgust_ci_jobs_started_total").increment(1);
                return (StatusCode::CREATED, Json(body)).into_response();
            }
            Ok(Claimed::Failed) => continue,
            Ok(Claimed::Empty) => {}
            Err(e) => return internal(e),
        }
        // wait for any JobsQueued event (a cheap re-check) or the deadline
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return no_job();
        }
        let woke = tokio::time::timeout(left, async {
            loop {
                match wake.recv().await {
                    Ok(Event::JobsQueued { .. }) => return true,
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return true,
                    Err(_) => return false,
                }
            }
        })
        .await;
        if !matches!(woke, Ok(true)) {
            return no_job();
        }
    }
}

fn no_job() -> Response {
    let mut r = StatusCode::NO_CONTENT.into_response();
    r.headers_mut().insert("X-GitLab-Last-Update", HeaderValue::from_static("0"));
    r
}

enum Claimed {
    Job(Value),
    Empty,
    /// The claimed job could not be prepared (and was failed): look again.
    Failed,
}

/// Claim a job in a short transaction (claim + job token), then build its
/// payload outside it. A job whose payload cannot be built is failed, so it
/// never sits at the head of the queue making every runner poll fail.
async fn try_claim(state: &AppState, runner: &Runner, manager_id: i64) -> anyhow::Result<Claimed> {
    let (mut tx, mut build) = loop {
        let mut tx = state.db.begin().await?;
        let Some(build) = forgust_db::ci::claim(&mut tx, runner, manager_id).await? else { return Ok(Claimed::Empty) };
        if build.user_id.is_some() {
            break (tx, build);
        }
        // jobs run as a person (their token acts for them): none, no run
        forgust_db::ci::finish_running(&mut *tx, build.id, "failed", Some("runner_configuration_error"), None).await?;
        tx.commit().await?;
        pipeline::process(state, build.pipeline_id).await?;
    };
    if let Some(max) = runner.maximum_timeout_secs
        && build.timeout_secs > max
    {
        build.timeout_secs = max;
        forgust_db::ci::set_build_timeout(&mut *tx, build.id, max).await?;
    }
    let token = crypto::random_token(crypto::prefix::JOB);
    let expires = chrono::Utc::now() + chrono::Duration::seconds(build.timeout_secs as i64 + 3600);
    let user_id = build.user_id.expect("checked above");
    forgust_db::ci::create_build_token(&mut *tx, &crypto::token_hash(&token), build.project_id, user_id, build.id, expires).await?;
    tx.commit().await?;

    match job_payload(state, runner, &build, &token).await {
        Ok((body, pipeline)) => {
            crate::ci::hooks::announce(state, &build, "running").await;
            if pipeline.status != "running" {
                // first job of the pipeline started
                pipeline::process(state, pipeline.id).await?;
            }
            Ok(Claimed::Job(body))
        }
        Err(e) => {
            tracing::error!(job = build.id, error = ?e, "job payload could not be built; failing the job");
            pipeline::finish_job(state, build.id, "failed", Some("unknown_failure"), None).await?;
            Ok(Claimed::Failed)
        }
    }
}

/// Everything the runner needs for a claimed job (see `payload`).
async fn job_payload(state: &AppState, runner: &Runner, build: &Build, token: &str) -> anyhow::Result<(Value, forgust_db::ci::Pipeline)> {
    let db = &state.db;
    let spec = pipeline::StoredSpec::of(build)?;
    let (pipeline, project, user, all) = tokio::try_join!(
        async { forgust_db::ci::pipeline(db, build.pipeline_id).await?.ok_or_else(|| anyhow::anyhow!("pipeline missing")) },
        async { forgust_db::projects::find(db, build.project_id).await?.ok_or_else(|| anyhow::anyhow!("project missing")) },
        async {
            Ok::<_, anyhow::Error>(match build.user_id {
                Some(u) => forgust_db::users::find(db, u).await?,
                None => None,
            })
        },
        async { Ok::<_, anyhow::Error>(forgust_db::ci::builds_of_pipeline(db, build.pipeline_id).await?) },
    )?;
    let (mr, pipeline_vars, settings, settings_vars, dependencies, commit, env_rules, upstream, renovate) = tokio::try_join!(
        async {
            Ok::<_, anyhow::Error>(match pipeline.merge_request_id {
                Some(id) => forgust_db::merge_requests::find_by_id(db, id).await?,
                None => None,
            })
        },
        async {
            Ok::<Vec<(String, String)>, anyhow::Error>(match forgust_db::ci::pipeline_variables(db, pipeline.id).await? {
                Some(enc) => state.secrets.decrypt_json(&enc)?,
                None => Vec::new(),
            })
        },
        async { Ok::<_, anyhow::Error>(forgust_db::ci::settings(db, project.id).await?) },
        async {
            // built-in Renovate and scanners (ADR-0023, ADR-0024): none of the
            // project's, groups' or instance's variables
            if crate::ci::pipeline::is_system(&pipeline.source) {
                Ok(Vec::new())
            } else {
                crate::ci::settings_variables(state, &project, pipeline.protected).await
            }
        },
        dependencies_of(state, build, &spec, &all, user.as_ref()),
        async { Ok::<_, anyhow::Error>(state.storage.commit(project.id, &pipeline.sha).await.ok().flatten()) },
        async {
            Ok::<_, anyhow::Error>(match &spec.job.environment {
                Some(_) => forgust_db::environments::protected(db, project.id).await?,
                None => Vec::new(),
            })
        },
        async {
            Ok::<_, anyhow::Error>(match pipeline.source.as_str() {
                "parent_pipeline" => forgust_db::ci::upstream_pipeline(db, pipeline.id).await?,
                _ => None,
            })
        },
        async {
            Ok::<_, anyhow::Error>(match pipeline.source.as_str() {
                crate::services::renovate::SOURCE => forgust_db::renovate::get(db, project.id).await?,
                _ => None,
            })
        },
    )?;
    let mut predefined = variables::pipeline_vars(
        state,
        &variables::PipelineInfo {
            project: &project,
            git_ref: &pipeline.git_ref,
            tag: pipeline.tag,
            sha: &pipeline.sha,
            before_sha: &pipeline.before_sha,
            source: &pipeline.source,
            protected: pipeline.protected,
            pipeline: Some(&pipeline),
            config_path: &settings.ci_config_path,
            user: user.as_ref(),
            commit: commit.as_ref(),
            mr: mr.as_ref(),
        },
    );
    predefined.extend(variables::job_vars(state, &project, build, runner, token));
    if pipeline.source == crate::services::renovate::SOURCE {
        // only ever for the project's own Renovate bot, with the feature on
        let settings = renovate.as_ref().filter(|s| s.enabled);
        let bot = user
            .as_ref()
            .filter(|u| u.user_type == "renovate_bot" && settings.and_then(|s| s.bot_user_id) == Some(u.id))
            .ok_or_else(|| anyhow::anyhow!("a Renovate job not run by the project's Renovate bot"))?;
        predefined.extend(crate::services::renovate::job_variables(state, &project, settings, bot, build.timeout_secs as i64).await?);
    }
    // the job's environment
    let mut environment = None;
    if let Some(env) = &spec.job.environment {
        let slug = crate::ci::environments::slug(&env.name);
        let tier = crate::ci::environments::tier_of(env);
        let protected = env_rules.iter().any(|r| r.matches(&env.name));
        predefined.extend([
            variables::Var::public("CI_ENVIRONMENT_NAME", env.name.clone()),
            variables::Var::public("CI_ENVIRONMENT_SLUG", slug),
            variables::Var::public("CI_ENVIRONMENT_ACTION", env.action.clone()),
            variables::Var::public("CI_ENVIRONMENT_TIER", tier.clone()),
        ]);
        if let Some(url) = &env.url {
            predefined.push(variables::Var::public("CI_ENVIRONMENT_URL", url.clone()));
        }
        environment = Some((env.name.clone(), tier, protected));
    }
    if let Some(up) = &upstream {
        predefined.push(variables::Var::public("CI_PARENT_PIPELINE_ID", up.id.to_string()));
    }
    // `id_tokens`: OIDC tokens, masked
    let namespace_path = project.full_path.rsplit_once('/').map(|(n, _)| n.to_string()).unwrap_or_default();
    let facts = crate::ci::oidc::JobFacts {
        project: &project,
        namespace_path: &namespace_path,
        user: user.as_ref(),
        pipeline: &pipeline,
        job_id: build.id,
        runner_id: runner.id,
        environment: environment.as_ref().map(|(n, t, p)| (n.as_str(), t.as_str(), *p)),
        timeout_secs: build.timeout_secs as i64,
    };
    for (name, jwt) in crate::ci::oidc::tokens(state, &facts, &spec.job.id_tokens).await? {
        predefined.push(variables::Var { key: name, value: jwt, public: false, masked: true, file: false, raw: true });
    }
    // dotenv reports of the jobs whose artifacts this one downloads, earlier jobs first
    let mut deps: Vec<&Build> = dependencies.iter().map(|(d, _)| d).collect();
    deps.sort_by_key(|d| (d.stage_idx, d.id));
    let dep_ids: Vec<i64> = deps.iter().map(|d| d.id).collect();
    let mut found = forgust_db::ci::dotenv_of(db, &dep_ids).await?;
    found.sort_by_key(|(id, _, _)| dep_ids.iter().position(|d| d == id));
    let dotenv: Vec<variables::Var> = found.into_iter().map(|(_, k, v)| variables::Var::public(&k, v)).collect();
    let vars = variables::all(
        predefined,
        variables::Yaml { globals: &spec.global_variables, raw_globals: &spec.raw_globals, job: &spec.job },
        dotenv,
        settings_vars,
        pipeline_vars,
    );
    let cache_keys = payload::cache_keys(state, project.id, &pipeline.sha, &spec.job.cache).await;
    let body = payload::job_response(
        state,
        &payload::Context {
            build,
            spec: &spec,
            pipeline: &pipeline,
            project: &project,
            token,
            variables: vars,
            dependencies,
            git_depth: settings.default_git_depth,
            cache_keys,
        },
    )?;
    Ok((body, pipeline))
}

/// Whether `instance` is `original` or one of its `parallel` instances (`build 1/2`, `test: [a]`).
fn base_name(instance: &str, original: &str) -> bool {
    instance == original || instance.strip_prefix(original).is_some_and(|rest| rest.starts_with(' ') || rest.starts_with(": ["))
}

/// Whether `job` downloads `dep`'s artifacts: `dependencies:` when given, else
/// `needs:` entries with `artifacts: true`, else every job of earlier stages.
pub fn depends_on(job: &Build, spec: &pipeline::StoredSpec, dep: &Build) -> bool {
    if dep.id == job.id || dep.pipeline_id != job.pipeline_id {
        return false;
    }
    match (&spec.job.dependencies, &job.needs) {
        (Some(d), _) => d.contains(&dep.name),
        (None, Some(needed)) => {
            needed.contains(&dep.name)
                && spec.job.needs.iter().flatten().any(|n| n.from.is_none() && n.artifacts && base_name(&dep.name, &n.job))
        }
        (None, None) => dep.stage_idx < job.stage_idx,
    }
}

async fn dependencies_of(
    state: &AppState,
    build: &Build,
    spec: &pipeline::StoredSpec,
    all: &[Build],
    user: Option<&forgust_db::users::User>,
) -> anyhow::Result<Vec<(Build, Option<(String, i64)>)>> {
    let mut deps: Vec<&Build> = all.iter().filter(|d| !d.retried && d.status == "success" && depends_on(build, spec, d)).collect();
    let cross = cross_dependencies(state, build, spec, user).await?;
    deps.extend(cross.iter());
    let ids: Vec<i64> = deps.iter().map(|d| d.id).collect();
    let archives = forgust_db::ci::archives_of(&state.db, &ids).await?;
    Ok(deps.into_iter().map(|d| (d.clone(), archives.iter().find(|a| a.build_id == d.id).map(|a| (a.filename.clone(), a.size)))).collect())
}

/// The jobs of other pipelines whose artifacts `build` takes (`needs:project`,
/// `needs:pipeline`). Another project counts only when the pipeline's
/// user may read its code; a pipeline only when it is one of this project's.
/// A need that finds nothing is left out (the job then fails on what it
/// misses, as in GitLab when the artifacts expired).
/// `user`: the job's user when the caller has it already (else looked up).
pub async fn cross_dependencies(
    state: &AppState,
    build: &Build,
    spec: &pipeline::StoredSpec,
    user: Option<&forgust_db::users::User>,
) -> anyhow::Result<Vec<Build>> {
    use forgust_ci_config::NeedFrom;
    let cross: Vec<_> = spec.job.needs.iter().flatten().filter(|n| n.artifacts && n.from.is_some()).collect();
    if cross.is_empty() {
        return Ok(Vec::new());
    }
    let user = match (user, build.user_id) {
        (Some(u), _) => Some(u.clone()),
        (None, Some(u)) => forgust_db::users::find(&state.db, u).await?,
        (None, None) => None,
    };
    let principal = match user {
        Some(u) => crate::auth::Principal::User { user: u, scopes: None },
        None => crate::auth::Principal::Anonymous,
    };
    let mut out: Vec<Build> = Vec::new();
    for n in cross {
        let found = match n.from.as_ref() {
            Some(NeedFrom::Project { project, git_ref }) => {
                let Ok(p) = crate::api::load_project(state, project).await else { continue };
                // the job's user may read it, and the project lets this one's
                // job tokens in (its allowlist), as for any other job-token read
                let readable = p.id == build.project_id
                    || (crate::auth::authz::can_project(state, &principal, crate::auth::authz::ProjectAction::ReadCode, &p)
                        .await
                        .unwrap_or(false)
                        && forgust_db::ci::job_token_allowed(&state.db, p.id, build.project_id).await?);
                if !readable {
                    continue;
                }
                forgust_db::ci::latest_successful_job(&state.db, p.id, git_ref, &n.job).await?
            }
            Some(NeedFrom::Pipeline { pipeline }) => {
                let Some(id) = pipeline_id_of(state, build, spec, pipeline).await? else { continue };
                match forgust_db::ci::pipeline(&state.db, id).await? {
                    Some(pl) if pl.project_id == build.project_id && pl.id != build.pipeline_id => {
                        forgust_db::ci::successful_job_in(&state.db, pl.id, &n.job).await?
                    }
                    _ => None,
                }
            }
            None => None,
        };
        if let Some(b) = found
            && !out.iter().any(|o| o.id == b.id)
        {
            out.push(b);
        }
    }
    Ok(out)
}

/// `needs:pipeline`'s value: an id, or a variable holding one (`$PARENT_PIPELINE_ID`
/// passed down by the trigger job, or `$CI_PARENT_PIPELINE_ID`).
async fn pipeline_id_of(state: &AppState, build: &Build, spec: &pipeline::StoredSpec, value: &str) -> anyhow::Result<Option<i64>> {
    let v = value.trim();
    let Some(name) = v.strip_prefix('$') else { return Ok(v.parse().ok()) };
    let name = name.trim_start_matches('{').trim_end_matches('}');
    if name == "CI_PARENT_PIPELINE_ID" {
        return Ok(forgust_db::ci::upstream_pipeline(&state.db, build.pipeline_id).await?.map(|u| u.id));
    }
    let from_pipeline: Vec<(String, String)> = match forgust_db::ci::pipeline_variables(&state.db, build.pipeline_id).await? {
        Some(enc) => state.secrets.decrypt_json(&enc)?,
        None => Vec::new(),
    };
    let found = from_pipeline
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .or_else(|| spec.job.variables.get(name).cloned())
        .or_else(|| spec.global_variables.get(name).cloned());
    Ok(found.and_then(|v| v.trim().parse().ok()))
}

// ---------------------------------------------------------------- job token auth

fn job_token<'a>(headers: &'a HeaderMap, body_token: Option<&'a str>, query_token: Option<&'a str>) -> Option<&'a str> {
    headers.get("JOB-TOKEN").and_then(|v| v.to_str().ok()).or(body_token).or(query_token).filter(|t| !t.is_empty())
}

/// The job a token belongs to, which must be `id` and still running (or the
/// response to send instead).
async fn running_job(state: &AppState, id: i64, token: Option<&str>) -> Result<Box<Build>, Response> {
    let Some(token) = token else { return Err(forbidden()) };
    match forgust_db::ci::build_by_token(&state.db, &crypto::token_hash(token)).await {
        Ok(Some(b)) if b.id != id => Err(forbidden()),
        Ok(Some(b)) if pipeline::holds_runner(&b.status) => Ok(Box::new(b)),
        // finished or canceled: the runner aborts on `Job-Status: failed|canceled` + 403
        Ok(Some(b)) => Err(with_status(forbidden(), &b.status, None)),
        Ok(None) => Err(forbidden()),
        Err(e) => Err(internal(e)),
    }
}

fn with_status(mut r: Response, status: &str, interval: Option<u64>) -> Response {
    if let Ok(v) = HeaderValue::from_str(status) {
        r.headers_mut().insert("Job-Status", v);
    }
    if let Some(i) = interval {
        r.headers_mut().insert("X-GitLab-Trace-Update-Interval", HeaderValue::from(i));
    }
    r
}

fn update_interval(state: &AppState, status: &str) -> u64 {
    if status == "canceling" { 1 } else { state.cfg.ci.trace_update_interval_secs.max(1) }
}

// ---------------------------------------------------------------- PUT /jobs/:id

#[derive(Deserialize, Default)]
#[serde(default)]
struct UpdateReq {
    token: String,
    state: String,
    failure_reason: String,
    exit_code: Option<i32>,
    output: TraceOutput,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TraceOutput {
    bytesize: Option<i64>,
}

/// Final updates refused for missing log bytes before the job is accepted
/// anyway, so a lost patch cannot wedge it.
const MAX_TRACE_REWINDS: i16 = 3;

async fn update_job(State(state): State<AppState>, Path(id): Path<i64>, headers: HeaderMap, Json(req): Json<UpdateReq>) -> Response {
    let b = match running_job(&state, id, job_token(&headers, Some(&req.token), None)).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    match req.state.as_str() {
        "success" | "failed" => {
            // the runner thinks it sent more log than we have: 412 makes it
            // rewind and resend from our offset (the PATCH answers 416 + Range)
            if req.output.bytesize.is_some_and(|size| size > b.trace_size) {
                match forgust_db::ci::bump_trace_rewinds(&state.db, b.id).await {
                    Ok(n) if n <= MAX_TRACE_REWINDS => {
                        return with_status((StatusCode::PRECONDITION_FAILED, "412").into_response(), &b.status, Some(1));
                    }
                    Ok(_) => {}
                    Err(e) => return internal(e),
                }
            }
            let (status, reason) = if req.state == "success" {
                ("success", None)
            } else {
                ("failed", Some(req.failure_reason.as_str()).filter(|r| !r.is_empty()))
            };
            match pipeline::finish_job(&state, b.id, status, reason, req.exit_code).await {
                Ok(status) => with_status((StatusCode::OK, "200").into_response(), &status, None),
                Err(e) => internal(e),
            }
        }
        // keep-alive (`running`) or anything else: just note the contact
        _ => {
            if let Err(e) = forgust_db::ci::touch_build(&state.db, b.id).await {
                return internal(e);
            }
            with_status((StatusCode::OK, "200").into_response(), &b.status, Some(update_interval(&state, &b.status)))
        }
    }
}

// ---------------------------------------------------------------- PATCH trace

#[derive(Deserialize, Default)]
#[serde(default)]
struct TokenQuery {
    token: Option<String>,
}

async fn patch_trace(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let b = match running_job(&state, id, job_token(&headers, None, q.token.as_deref())).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let Some(range) = headers.get("Content-Range").and_then(|v| v.to_str().ok()) else {
        return plain(StatusCode::BAD_REQUEST, "400 Missing header Content-Range");
    };
    // "start-end", end inclusive
    let Some(start) = range.trim().trim_start_matches("bytes ").split('-').next().and_then(|s| s.trim().parse::<i64>().ok()) else {
        return plain(StatusCode::BAD_REQUEST, "400 Bad Content-Range");
    };
    // the cap holds whoever writes: the job's own script has its token too
    if (start.max(0) as u64).saturating_add(body.len() as u64) > state.cfg.ci.max_trace_bytes {
        if let Err(e) = pipeline::finish_job(&state, b.id, "failed", Some("trace_size_exceeded"), None).await {
            return internal(e);
        }
        return plain(StatusCode::FORBIDDEN, "403 Forbidden - the job log is over the size limit");
    }
    let result = async {
        let mut tx = state.db.begin().await?;
        let Some(locked) = forgust_db::ci::build_for_update(&mut tx, b.id).await? else { anyhow::bail!("job vanished") };
        let r = forgust_db::ci::append_trace(&mut tx, &locked, start, &body).await?;
        tx.commit().await?;
        Ok::<_, anyhow::Error>((r, locked.status))
    }
    .await;
    match result {
        Ok((Append::Ok(size), status)) => {
            state.events.publish(Event::JobTrace { project_id: b.project_id, job_id: b.id, trace_size: size });
            let mut r = with_status(StatusCode::ACCEPTED.into_response(), &status, Some(update_interval(&state, &status)));
            r.headers_mut().insert("Range", HeaderValue::from_str(&format!("0-{size}")).expect("ascii"));
            r
        }
        Ok((Append::RangeMismatch(size), status)) => {
            let mut r = with_status(plain(StatusCode::RANGE_NOT_SATISFIABLE, "416 Range Not Satisfiable"), &status, None);
            r.headers_mut().insert("Range", HeaderValue::from_str(&format!("0-{size}")).expect("ascii"));
            r
        }
        Err(e) => internal(e),
    }
}

// ---------------------------------------------------------------- artifacts

#[derive(Deserialize, Default)]
#[serde(default)]
struct ArtifactQuery {
    token: Option<String>,
    expire_in: Option<String>,
    artifact_type: Option<String>,
    artifact_format: Option<String>,
}

const ARTIFACT_TYPES: &[&str] = &[
    "archive",
    "metadata",
    "junit",
    "codequality",
    "coverage_report",
    "cobertura",
    "dotenv",
    "terraform",
    "metrics",
    "sast",
    "dependency_scanning",
    "container_scanning",
    "dast",
    "secret_detection",
    "license_scanning",
    "requirements",
    "accessibility",
    "browser_performance",
    "load_performance",
    "annotations",
    "repository_xray",
    "cyclonedx",
];

async fn upload_artifacts(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<ArtifactQuery>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let b = match running_job(&state, id, job_token(&headers, None, q.token.as_deref())).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let file_type = q.artifact_type.clone().unwrap_or_else(|| "archive".into());
    if !ARTIFACT_TYPES.contains(&file_type.as_str()) {
        return plain(StatusCode::BAD_REQUEST, "400 artifact_type is not supported");
    }
    let file_format = q.artifact_format.clone().unwrap_or_else(|| "zip".into());
    if !["zip", "gzip", "raw"].contains(&file_format.as_str()) {
        return plain(StatusCode::BAD_REQUEST, "400 artifact_format is not supported");
    }
    let mut limit = state.cfg.ci.max_artifact_bytes;
    let quota = state.cfg.ci.max_artifact_bytes_per_project;
    if quota > 0 {
        let used = match forgust_db::ci::artifact_bytes(&state.db, b.project_id).await {
            Ok(n) => n.max(0) as u64,
            Err(e) => return internal(e),
        };
        if used >= quota {
            return plain(StatusCode::PAYLOAD_TOO_LARGE, "413 the project's artifact storage is full");
        }
        limit = limit.min(quota - used);
    }
    // the project's and its namespace's storage quotas
    limit = match crate::services::quotas::cap(&state, b.project_id, limit).await {
        Ok(Ok(n)) => n,
        Ok(Err(msg)) => return plain(StatusCode::PAYLOAD_TOO_LARGE, &format!("413 {msg}")),
        Err(e) => return internal(e),
    };
    let key = format!("ci/artifacts/{}/{}/{}-{}", b.project_id, b.id, file_type, crypto::random_urlsafe(6));
    let mut stored: Option<(String, i64)> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return plain(StatusCode::BAD_REQUEST, &format!("400 bad multipart body: {e}")),
        };
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().unwrap_or("artifacts.zip").to_string();
        match crate::blob::stream_field(&state.blobs, field, &key, limit).await {
            Ok(n) => stored = Some((filename, n as i64)),
            Err(crate::blob::StreamError::TooLarge) => return plain(StatusCode::PAYLOAD_TOO_LARGE, "413 Request Entity Too Large"),
            Err(crate::blob::StreamError::Interrupted(e)) => return plain(StatusCode::BAD_REQUEST, &format!("400 {e}")),
            Err(crate::blob::StreamError::Store(e)) => return internal(e),
        }
    }
    let Some((filename, size)) = stored else { return plain(StatusCode::BAD_REQUEST, "400 missing file") };
    // a dotenv report is checked now: GitLab refuses a bad one at upload
    let dotenv = if file_type == "dotenv" {
        let parsed = crate::ci::reports::read(&state, &key, &file_format, crate::ci::reports::DOTENV_MAX_BYTES)
            .await
            .map_err(|e| format!("dotenv: {e}"))
            .and_then(|d| crate::ci::reports::parse_dotenv(&d));
        match parsed {
            Ok(v) => Some(v),
            Err(e) => {
                let _ = state.blobs.delete(&key).await;
                return plain(StatusCode::BAD_REQUEST, &format!("400 {e}"));
            }
        }
    } else {
        None
    };
    let expire_in = q.expire_in.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| state.cfg.ci.default_artifact_expire_in.clone());
    // unreadable values get the default, not "forever"; huge ones are capped
    const MAX_KEEP_SECS: i64 = 100 * 365 * 86400;
    let expire_at = if expire_in.trim().eq_ignore_ascii_case("never") {
        None
    } else {
        let secs = pipeline::parse_duration(&expire_in)
            .or_else(|| pipeline::parse_duration(&state.cfg.ci.default_artifact_expire_in))
            .unwrap_or(30 * 86400)
            .clamp(0, MAX_KEEP_SECS);
        Some(chrono::Utc::now() + chrono::Duration::seconds(secs))
    };
    let result = async {
        let mut tx = state.db.begin().await?;
        let old = forgust_db::ci::upsert_artifact(
            &mut tx,
            forgust_db::ci::NewArtifact {
                build_id: b.id,
                project_id: b.project_id,
                file_type: &file_type,
                file_format: &file_format,
                filename: &filename,
                size,
                blob_key: &key,
                expire_at,
            },
        )
        .await?;
        if let Some(vars) = &dotenv {
            forgust_db::ci::set_dotenv(&mut tx, b.id, vars).await?;
        }
        tx.commit().await?;
        Ok::<_, anyhow::Error>(old)
    }
    .await;
    match result {
        Ok(old) => {
            if let Some(old) = old {
                let _ = state.blobs.delete(&old).await;
            }
            metrics::counter!("forgust_ci_artifact_bytes_total").increment(size as u64);
            (StatusCode::CREATED, "201").into_response()
        }
        Err(e) => {
            let _ = state.blobs.delete(&key).await;
            internal(e)
        }
    }
}

/// Artifacts of `id`, for a running job of the same pipeline that depends on it.
async fn download_artifacts(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = job_token(&headers, None, q.token.as_deref()) else { return plain(StatusCode::UNAUTHORIZED, "401 Unauthorized") };
    let current = match forgust_db::ci::build_id_for_token(&state.db, &crypto::token_hash(token)).await {
        Ok(Some(b)) => b,
        Ok(None) => return plain(StatusCode::UNAUTHORIZED, "401 Unauthorized"),
        Err(e) => return internal(e),
    };
    let (Ok(Some(cur)), Ok(Some(dep))) = (forgust_db::ci::build(&state.db, current).await, forgust_db::ci::build(&state.db, id).await)
    else {
        return plain(StatusCode::NOT_FOUND, "404 Not Found");
    };
    if !pipeline::holds_runner(&cur.status) {
        return forbidden();
    }
    let spec = match pipeline::StoredSpec::of(&cur) {
        Ok(j) => j,
        Err(e) => return internal(e),
    };
    let allowed = if cur.pipeline_id == dep.pipeline_id {
        depends_on(&cur, &spec, &dep)
    } else {
        match cross_dependencies(&state, &cur, &spec, None).await {
            Ok(cross) => cross.iter().any(|c| c.id == dep.id),
            Err(e) => return internal(e),
        }
    };
    if !allowed {
        return forbidden();
    }
    let art = match forgust_db::ci::artifact(&state.db, dep.id, "archive").await {
        Ok(Some(a)) => a,
        Ok(None) => return plain(StatusCode::NOT_FOUND, "404 Artifact Not Found"),
        Err(e) => return internal(e),
    };
    match crate::ci::trace::blob_download(&state, &art.blob_key, "application/zip", Some(&art.filename)).await {
        Ok(Some(r)) => r,
        Ok(None) => plain(StatusCode::NOT_FOUND, "404 Artifact Not Found"),
        Err(e) => internal(e),
    }
}
