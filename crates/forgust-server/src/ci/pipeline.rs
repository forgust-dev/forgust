//! Pipelines: creation from `.gitlab-ci.yml` and the job state machine.
//!
//! Every state change goes through [`process`], which locks the pipeline's jobs,
//! moves `created` jobs whose prerequisites are done to `pending` (or `manual`,
//! or `skipped`), enqueues pending jobs for runners, and recomputes the
//! pipeline status. It is idempotent: calling it again changes nothing.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use forgust_db::ci::{Build, NewBuild, NewPipeline};

use crate::events::Event;
use crate::state::AppState;

pub use forgust_storage::ZERO_OID;

/// Statuses a job or pipeline ends in.
pub fn is_complete(status: &str) -> bool {
    matches!(status, "success" | "failed" | "canceled" | "skipped")
}

/// A runner holds the job (it reports to us until it finishes).
pub fn holds_runner(status: &str) -> bool {
    matches!(status, "running" | "canceling")
}

/// Not started yet: cancelling just marks it canceled.
fn not_started(status: &str) -> bool {
    matches!(status, "created" | "pending" | "manual" | "scheduled" | "preparing" | "waiting_for_resource")
}

/// What triggered a pipeline.
pub struct Trigger<'a> {
    pub project_id: i64,
    /// Branch or tag name (without `refs/heads/` / `refs/tags/`).
    pub git_ref: &'a str,
    pub tag: bool,
    pub sha: &'a str,
    pub before_sha: &'a str,
    pub source: &'a str,
    pub user_id: Option<i64>,
    pub merge_request_id: Option<i64>,
    pub schedule_id: Option<i64>,
    /// Extra variables (web form, API, schedule), stored encrypted.
    pub variables: Vec<(String, String)>,
    /// A downstream pipeline: the `trigger` job that starts it.
    pub upstream: Option<Upstream>,
    /// A config Forgust writes itself (built-in Renovate, scanners), instead of the repository's.
    pub config: Option<String>,
}

/// A pipeline Forgust runs itself (built-in Renovate, security scans): instance
/// runners only, no project CI variables, never a ref's or commit's status (the
/// database's `is_system_source` says the same).
pub fn is_system(source: &str) -> bool {
    matches!(source, crate::services::renovate::SOURCE | crate::services::scanning::SOURCE)
}

impl<'a> Trigger<'a> {
    /// A branch pipeline of `sha`: no previous commit, merge request, schedule,
    /// variables, upstream or config of Forgust's (set those that apply).
    pub fn new(project_id: i64, git_ref: &'a str, sha: &'a str, source: &'a str, user_id: Option<i64>) -> Self {
        Trigger {
            project_id,
            git_ref,
            tag: false,
            sha,
            before_sha: ZERO_OID,
            source,
            user_id,
            merge_request_id: None,
            schedule_id: None,
            variables: Vec::new(),
            upstream: None,
            config: None,
        }
    }
}

/// Where a downstream pipeline comes from.
pub struct Upstream {
    pub build_id: i64,
    pub pipeline_id: i64,
    /// A child pipeline's config (its includes), instead of the repository's.
    pub config: Option<String>,
}

/// Whether `branch` matches a protected-branch rule of the project.
pub async fn ref_is_protected(state: &AppState, project_id: i64, git_ref: &str, tag: bool) -> anyhow::Result<bool> {
    if tag {
        // protected tags are not modeled yet: tags are unprotected
        return Ok(false);
    }
    let rules = forgust_db::protected_branches::list(&state.db, project_id).await?;
    Ok(rules.iter().any(|r| r.matches(git_ref)))
}

/// The stored job: the planned job plus the global variables it inherits.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredSpec {
    #[serde(flatten)]
    pub job: forgust_ci_config::Job,
    /// Top-level YAML variables (after `inherit:variables` and workflow rules).
    #[serde(default)]
    pub global_variables: forgust_ci_config::IndexMap<String, String>,
    #[serde(default)]
    pub raw_globals: Vec<String>,
}

impl StoredSpec {
    pub fn of(build: &Build) -> anyhow::Result<StoredSpec> {
        // from a reference: no copy of the whole job (scripts, variables) first
        Ok(<StoredSpec as serde::Deserialize>::deserialize(&build.spec)?)
    }
}

/// Result of [`create`].
#[derive(Debug)]
pub enum Created {
    Pipeline(i64),
    /// No CI config file at this commit.
    NoConfig,
    /// The rules left nothing to run (`workflow:rules`, or no job matched).
    Skipped(String),
}

impl Created {
    pub fn id(&self) -> Option<i64> {
        match self {
            Created::Pipeline(id) => Some(*id),
            _ => None,
        }
    }
}

async fn read_config(state: &AppState, project_id: i64, sha: &str, path: &str) -> anyhow::Result<Option<Result<String, String>>> {
    match state.storage.blob(project_id, sha, path, 1 << 20).await {
        Ok(b) if b.truncated => Ok(Some(Err(format!("{path} is larger than 1 MB")))),
        Ok(b) => Ok(Some(String::from_utf8(b.data).map_err(|_| format!("{path} is not UTF-8")))),
        Err(forgust_storage::StorageError::PathNotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn uses_changes(cfg: &forgust_ci_config::Config) -> bool {
    let rule = |r: &forgust_ci_config::Rule| r.changes.is_some();
    cfg.workflow_rules.iter().flatten().any(rule)
        || cfg.jobs.values().any(|j| {
            j.rules.iter().flatten().any(rule)
                || j.only.as_ref().is_some_and(|f| !f.changes.is_empty())
                || j.except.as_ref().is_some_and(|f| !f.changes.is_empty())
        })
}

fn uses_exists(cfg: &forgust_ci_config::Config) -> bool {
    let rule = |r: &forgust_ci_config::Rule| r.exists.is_some();
    cfg.workflow_rules.iter().flatten().any(rule) || cfg.jobs.values().any(|j| j.rules.iter().flatten().any(rule))
}

async fn changed_paths(
    state: &AppState,
    t: &Trigger<'_>,
    mr: Option<&forgust_db::merge_requests::MergeRequest>,
) -> anyhow::Result<Option<Vec<String>>> {
    let from = match (t.source, mr) {
        ("merge_request_event", Some(m)) => {
            let Some(target) = state.storage.resolve(t.project_id, &format!("refs/heads/{}", m.target_branch)).await? else {
                return Ok(None);
            };
            state.storage.merge_base(t.project_id, &target, t.sha).await?
        }
        ("push", _) if t.before_sha != ZERO_OID => Some(t.before_sha.to_string()),
        // new branches, web and scheduled pipelines: `changes` is always true
        _ => None,
    };
    let Some(from) = from else { return Ok(None) };
    let opts = forgust_storage::DiffOptions { offset: usize::MAX, limit: 1, ..Default::default() };
    // the old commit may be gone (force push): like GitLab, `changes` is then true
    let Ok(diff) = state.storage.diff(t.project_id, Some(&from), t.sha, opts).await else { return Ok(None) };
    let mut out = Vec::new();
    for f in diff.summary {
        if !f.old_path.is_empty() && f.old_path != f.new_path {
            out.push(f.old_path);
        }
        out.push(f.new_path);
    }
    Ok(Some(out))
}

/// Create a pipeline for `t`: read the config (with includes), apply rules,
/// and insert the jobs. A broken config still makes a (failed) pipeline that
/// says why.
pub async fn create(state: &AppState, t: Trigger<'_>) -> anyhow::Result<Created> {
    let settings = forgust_db::ci::settings(&state.db, t.project_id).await?;
    // a child pipeline's config comes from its trigger job
    let child_config = t.upstream.as_ref().and_then(|u| u.config.clone());
    let config_path = match (&t.config, &child_config) {
        (Some(_), _) => t.source.to_string(),
        (None, Some(_)) => "child pipeline".to_string(),
        (None, None) => settings.ci_config_path.clone(),
    };
    let source = match t.config.clone().or(child_config) {
        Some(c) => Ok(c),
        None => match read_config(state, t.project_id, t.sha, &settings.ci_config_path).await? {
            Some(s) => s,
            None => return Ok(Created::NoConfig),
        },
    };
    // independent: at once
    let (project, user, mr, mut protected, commit) = tokio::try_join!(
        async { forgust_db::projects::find(&state.db, t.project_id).await?.context("project vanished") },
        async {
            Ok::<_, anyhow::Error>(match t.user_id {
                Some(u) => forgust_db::users::find(&state.db, u).await?,
                None => None,
            })
        },
        async {
            Ok::<_, anyhow::Error>(match t.merge_request_id {
                Some(id) => forgust_db::merge_requests::find_by_id(&state.db, id).await?,
                None => None,
            })
        },
        ref_is_protected(state, t.project_id, t.git_ref, t.tag),
        async { Ok::<_, anyhow::Error>(state.storage.commit(t.project_id, t.sha).await.ok().flatten()) },
    )?;
    // a merge request's pipeline (a train car's too: the merge of both) runs
    // code headed for the target: protected only when both ends are, and only
    // for someone who may run pipelines on the source (opening a merge request
    // from a protected branch must not buy protected secrets on demand)
    if protected && let Some(m) = &mr {
        protected = ref_is_protected(state, t.project_id, &m.target_branch, false).await?
            && match &user {
                Some(u) => crate::ci::may_run(
                    state,
                    &crate::auth::Principal::User { user: u.clone(), scopes: None },
                    &project,
                    &m.source_branch,
                    false,
                )
                .await
                .unwrap_or(false),
                None => false,
            };
    }

    let info = crate::ci::variables::PipelineInfo {
        project: &project,
        git_ref: t.git_ref,
        tag: t.tag,
        sha: t.sha,
        before_sha: t.before_sha,
        source: t.source,
        protected,
        pipeline: None,
        config_path: &config_path,
        user: user.as_ref(),
        commit: commit.as_ref(),
        mr: mr.as_ref(),
    };
    let mut vars = crate::ci::rule_variables(state, &info, &t.variables).await?;
    if let Some(u) = &t.upstream {
        vars.insert("CI_PARENT_PIPELINE_ID".into(), u.pipeline_id.to_string());
    }
    let resolver = crate::ci::resolver::RepoResolver {
        state: state.clone(),
        project_id: t.project_id,
        sha: t.sha.to_string(),
        user: user.clone(),
        vars: vars.clone(),
    };
    // config problems make a failed pipeline that says why; internal errors
    // (database, git) abort instead, so a hiccup never reads as a YAML error
    let planned: Result<(forgust_ci_config::Config, forgust_ci_config::Plan), String> = match source {
        Err(e) => Err(e),
        Ok(source) => match forgust_ci_config::load(&source, &config_path, &resolver).await {
            Err(e) => Err(e.to_string()),
            Ok(cfg) => {
                let changed = if uses_changes(&cfg) { changed_paths(state, &t, mr.as_ref()).await? } else { None };
                let files = if uses_exists(&cfg) { Some(state.storage.list_paths(t.project_id, t.sha, 100_000).await?.0) } else { None };
                // `rules:changes:compare_to`: against each ref (merge base), unknown when it does not resolve
                let mut changed_vs = HashMap::new();
                for r in cfg.compare_to_refs() {
                    let r = crate::ci::variables::expand(&r, &vars);
                    let full = crate::services::edits::qualify(state, t.project_id, &r).await.unwrap_or(r.clone());
                    if let Some(base) = state.storage.resolve(t.project_id, &full).await? {
                        let from = state.storage.merge_base(t.project_id, &base, t.sha).await?.unwrap_or(base);
                        changed_vs.insert(r, state.storage.changed_paths(t.project_id, None, &from, t.sha).await?);
                    }
                }
                // `rules:exists:project`: files of projects the pipeline's user can read
                let mut files_in = HashMap::new();
                for (proj, git_ref) in cfg.exists_projects() {
                    let (proj, git_ref) =
                        (crate::ci::variables::expand(&proj, &vars), git_ref.map(|g| crate::ci::variables::expand(&g, &vars)));
                    if let Ok(paths) = resolver.paths_of(&proj, git_ref.as_deref()).await {
                        files_in.insert(forgust_ci_config::plan::files_key(&proj, git_ref.as_deref()), paths);
                    }
                }
                let ctx = forgust_ci_config::Context {
                    vars: &vars,
                    source: t.source,
                    git_ref: t.git_ref,
                    tag: t.tag,
                    changed: changed.as_deref(),
                    files: files.as_deref(),
                    changed_vs: Some(&changed_vs),
                    files_in: Some(&files_in),
                };
                match forgust_ci_config::plan::plan(&cfg, &ctx) {
                    Err(e) => Err(e.to_string()),
                    Ok(forgust_ci_config::Outcome::Run(plan)) => Ok((cfg, plan)),
                    Ok(forgust_ci_config::Outcome::Skip(why)) => return Ok(Created::Skipped(why)),
                }
            }
        },
    };
    let (plan, errors) = (planned.as_ref().ok(), planned.as_ref().err());
    let variables_enc = if t.variables.is_empty() { None } else { Some(state.secrets.encrypt_json(&t.variables)?) };

    let mut tx = state.db.begin().await?;
    let iid = forgust_db::merge_requests::next_iid(&mut *tx, t.project_id, "ci_pipelines").await?;
    let pipeline_id = forgust_db::ci::insert_pipeline(
        &mut *tx,
        NewPipeline {
            project_id: t.project_id,
            iid,
            git_ref: t.git_ref,
            tag: t.tag,
            sha: t.sha,
            before_sha: t.before_sha,
            source: t.source,
            protected,
            merge_request_id: t.merge_request_id,
            schedule_id: t.schedule_id,
            user_id: t.user_id,
            yaml_errors: errors.map(String::as_str),
            variables_enc: variables_enc.as_deref(),
            commit_title: commit.as_ref().map(|c| c.subject.as_str()),
            auto_cancel: plan.map(|(cfg, _)| cfg.auto_cancel.as_str()).unwrap_or("conservative"),
            auto_cancel_on_job_failure: plan.map(|(cfg, _)| cfg.auto_cancel_on_job_failure.as_str()).unwrap_or("none"),
            upstream_build_id: t.upstream.as_ref().map(|u| u.build_id),
        },
    )
    .await?;
    if let Some((cfg, plan)) = plan {
        for pj in &plan.jobs {
            // each job carries the global variables it inherits
            let mut globals: forgust_ci_config::IndexMap<String, String> = cfg
                .variables
                .iter()
                .filter(|(k, _)| pj.job.inherit_variables.as_ref().is_none_or(|keep| keep.contains(k)))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, v) in &plan.workflow_variables {
                globals.insert(k.clone(), v.clone());
            }
            // names that take variables are expanded now (environment, resource group)
            let mut job = pj.job.clone();
            // (a copy of the pipeline's variables only for the jobs that need one)
            if job.environment.is_some() || job.resource_group.is_some() {
                let mut job_vars = vars.clone();
                for (k, v) in globals.iter().chain(job.variables.iter()) {
                    job_vars.entry(k.clone()).or_insert_with(|| v.clone());
                }
                if let Some(env) = job.environment.as_mut() {
                    env.name = crate::ci::variables::expand(&env.name, &job_vars);
                    env.url = env.url.as_deref().map(|u| crate::ci::variables::expand(u, &job_vars));
                }
                job.resource_group = job.resource_group.as_deref().map(|g| crate::ci::variables::expand(g, &job_vars));
            }
            let spec = StoredSpec { job, global_variables: globals, raw_globals: cfg.raw_variables.clone() };
            let timeout =
                pj.job.timeout.as_deref().and_then(parse_duration).unwrap_or(settings.default_timeout_secs as i64).clamp(60, 30 * 86400)
                    as i32;
            let retry = pj.job.retry.clone().unwrap_or_default();
            let mut retry_when = retry.when.clone();
            retry_when.extend(retry.exit_codes.iter().map(|c| format!("exit_code:{c}")));
            let build_id = forgust_db::ci::insert_build(
                &mut *tx,
                NewBuild {
                    pipeline_id,
                    project_id: t.project_id,
                    name: &pj.name,
                    stage: &pj.stage,
                    stage_idx: pj.stage_idx,
                    status: "created",
                    when_: &pj.when,
                    allow_failure: pj.allow_failure,
                    needs: pj.needs.as_deref(),
                    spec: &serde_json::to_value(&spec)?,
                    retry_max: retry.max as i32,
                    retry_when: &retry_when,
                    attempt: 1,
                    tags: &pj.job.tags,
                    timeout_secs: timeout,
                    user_id: t.user_id,
                    resource_group: spec.job.resource_group.as_deref(),
                },
            )
            .await?;
            if let Some(env) = &spec.job.environment {
                crate::ci::environments::on_job_created(&mut tx, &project, build_id, env, t.git_ref, t.tag, t.sha, t.user_id).await?;
            }
        }
    }
    tx.commit().await?;
    state.events.publish(Event::Pipeline {
        project_id: t.project_id,
        pipeline_id,
        status: if errors.is_some() { "failed".into() } else { "created".into() },
    });
    if errors.is_none() {
        process(state, pipeline_id).await?;
        if settings.auto_cancel_redundant && !t.tag && matches!(t.source, "push" | "merge_request_event") {
            auto_cancel_redundant(state, pipeline_id).await?;
        }
    }
    Ok(Created::Pipeline(pipeline_id))
}

/// GitLab's auto-cancel of redundant pipelines, run when a pipeline for a new
/// commit starts: older unfinished pipelines of the branch (same kind) go, by
/// their own `workflow:auto_cancel:on_new_commit`:
/// - `conservative` (default): the whole pipeline, unless a job that is not
///   `interruptible` has started (jobs not started count as interruptible);
/// - `interruptible`: only its unfinished `interruptible` jobs;
/// - `none`: nothing.
async fn auto_cancel_redundant(state: &AppState, newer_id: i64) -> anyhow::Result<()> {
    let Some(newer) = forgust_db::ci::pipeline(&state.db, newer_id).await? else { return Ok(()) };
    for (id, mode) in forgust_db::ci::redundant_pipelines(&state.db, &newer).await? {
        let builds: Vec<Build> = forgust_db::ci::builds_of_pipeline(&state.db, id).await?.into_iter().filter(|b| !b.retried).collect();
        let interruptible = |b: &Build| StoredSpec::of(b).is_ok_and(|s| s.job.interruptible);
        match mode.as_str() {
            "none" => {}
            "interruptible" => {
                for b in builds.iter().filter(|b| interruptible(b) && !is_complete(&b.status)) {
                    cancel_job(state, b).await?;
                }
            }
            _ => {
                let pinned = builds.iter().any(|b| b.started_at.is_some() && !interruptible(b));
                if !pinned {
                    cancel(state, id).await?;
                    tracing::info!(pipeline = id, by = newer_id, "redundant pipeline canceled");
                }
            }
        }
    }
    Ok(())
}

/// `1h 30m`, `90 minutes`, `3600`, `2 hours and 5 min`… GitLab's duration
/// syntax (a bare number is seconds).
pub fn parse_duration(s: &str) -> Option<i64> {
    // tokens: numbers and words; "and" and punctuation separate them
    let lower = s.to_lowercase();
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut digit = false;
    for c in lower.chars() {
        let d = c.is_ascii_digit();
        if c.is_ascii_alphanumeric() {
            if !cur.is_empty() && d != digit {
                tokens.push(std::mem::take(&mut cur));
            }
            digit = d;
            cur.push(c);
        } else if !cur.is_empty() {
            tokens.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let tokens: Vec<&str> = tokens.iter().map(String::as_str).filter(|t| *t != "and").collect();
    if tokens.is_empty() {
        return None;
    }
    let mut total = 0i64;
    let mut i = 0;
    while i < tokens.len() {
        let n: i64 = tokens[i].parse().ok()?;
        match tokens.get(i + 1) {
            Some(u) if u.chars().all(|c| c.is_ascii_alphabetic()) => {
                total = total.checked_add(n.checked_mul(unit_secs(u)?)?)?;
                i += 2;
            }
            _ => {
                total = total.checked_add(n)?;
                i += 1;
            }
        }
    }
    Some(total)
}

fn unit_secs(u: &str) -> Option<i64> {
    Some(match u {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86400,
        "w" | "wk" | "week" | "weeks" => 7 * 86400,
        "mo" | "month" | "months" => 30 * 86400,
        "y" | "yr" | "year" | "years" => 365 * 86400,
        _ => return None,
    })
}

/// Advance the pipeline: start jobs whose prerequisites are done, skip jobs
/// that can no longer run, and recompute the pipeline status.
pub async fn process(state: &AppState, pipeline_id: i64) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    let pipeline = forgust_db::ci::pipeline(&mut *tx, pipeline_id).await?.context("pipeline vanished")?;
    let mut builds = forgust_db::ci::lock_pipeline_builds(&mut tx, pipeline_id).await?;
    let mut queued = false;
    let mut changed: Vec<usize> = Vec::new();

    loop {
        let mut progress = false;
        for i in 0..builds.len() {
            if builds[i].status != "created" {
                continue;
            }
            let Some(next) = next_status(&builds, i) else { continue };
            builds[i].status = next.to_string();
            changed.push(i);
            progress = true;
        }
        if !progress {
            break;
        }
    }
    for &i in &changed {
        if builds[i].status == "pending" {
            let status = admit(state, &mut tx, &builds[i]).await?;
            builds[i].status = if status == "bridge" { "pending".into() } else { status.to_string() };
            queued |= status == "pending";
            continue;
        }
        let b = &builds[i];
        if b.status == "scheduled" {
            let secs = StoredSpec::of(b).ok().and_then(|s| s.job.start_in).as_deref().and_then(parse_duration).unwrap_or(0);
            forgust_db::ci::set_scheduled_at(&mut *tx, b.id, Some(chrono::Utc::now() + chrono::Duration::seconds(secs))).await?;
        }
        forgust_db::ci::set_build_status(&mut *tx, b.id, &b.status, None).await?;
    }
    let status = pipeline_status(&builds);
    let pipeline_changed = forgust_db::ci::set_pipeline_status(&mut *tx, pipeline_id, status).await?;
    tx.commit().await?;

    for &i in &changed {
        crate::ci::hooks::announce(state, &builds[i], &builds[i].status).await;
    }
    if queued {
        state.events.publish(Event::JobsQueued { project_id: pipeline.project_id });
    }
    if pipeline_changed {
        // a downstream pipeline: its `trigger` job (strategy: depend) follows
        if let Err(e) = Box::pin(crate::ci::bridge::downstream_changed(state, pipeline_id, status)).await {
            tracing::warn!(pipeline_id, error = %e, "upstream job not updated");
        }
        // a merge train car: merges when it passed, leaves when it failed
        if let Err(e) = Box::pin(crate::services::merge_train::pipeline_changed(state, pipeline_id, status)).await {
            tracing::warn!(pipeline_id, error = %e, "merge train not advanced");
        }
        crate::services::renovate::on_pipeline_finished(state, &pipeline, status).await;
        state.events.publish(Event::Pipeline { project_id: pipeline.project_id, pipeline_id, status: status.to_string() });
        crate::ci::hooks::pipeline(state, pipeline.project_id, pipeline_id).await;
        crate::ci::on_pipeline_status(state, &pipeline).await;
        // `refs/pipelines/<id>` keeps the commit fetchable while jobs may run
        // (a retry or a played job makes the pipeline active again)
        if is_complete(status) {
            // findings are resolved by the default branch's scans only: not a
            // tag's or a merge request's pipeline (no job queued for nothing)
            if !pipeline.tag && pipeline.merge_request_id.is_none() {
                crate::services::security::after_pipeline(state, pipeline_id, pipeline.project_id).await;
            }
            crate::ci::unpin_commit(state, pipeline.project_id, pipeline_id).await;
        } else {
            crate::ci::pin_commit(state, pipeline.project_id, pipeline_id, &pipeline.sha).await;
        }
    }
    Ok(())
}

/// A job is ready to run (its prerequisites are done, or it was played or its
/// delay passed): queue it for a runner, unless its user may not deploy to
/// its protected environment (it fails), its resource group is busy (it
/// waits), or it is a `trigger` job (it starts its downstream pipeline).
/// Writes the status and returns it.
pub async fn admit(state: &AppState, tx: &mut sqlx::PgConnection, b: &Build) -> anyhow::Result<&'static str> {
    if !crate::ci::environments::may_deploy(state, b).await? {
        forgust_db::ci::set_build_status(&mut *tx, b.id, "failed", Some("protected_environment_failure")).await?;
        return Ok("failed");
    }
    if let Some((_, g)) = forgust_db::ci::resource_group_of(&mut *tx, b.id).await?
        && forgust_db::ci::resource_busy(&mut *tx, b.project_id, &g, b.id).await?
    {
        forgust_db::ci::set_build_status(&mut *tx, b.id, "waiting_for_resource", None).await?;
        return Ok("waiting_for_resource");
    }
    forgust_db::ci::set_build_status(&mut *tx, b.id, "pending", None).await?;
    if StoredSpec::of(b).is_ok_and(|s| s.job.trigger.is_some()) {
        state.jobs.enqueue(&mut *tx, crate::jobs::StartBridge { build_id: b.id }.request()).await?;
        return Ok("bridge");
    }
    forgust_db::ci::enqueue(&mut *tx, b.id).await?;
    Ok("pending")
}

/// Start a job that waited (a delayed job whose time came, a job whose
/// resource group freed up) and advance its pipeline.
pub async fn start_waiting(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    let Some(b) = forgust_db::ci::build_for_update(&mut tx, build_id).await? else { return Ok(()) };
    if b.status != "scheduled" && b.status != "waiting_for_resource" {
        return Ok(());
    }
    let status = admit(state, &mut tx, &b).await?;
    tx.commit().await?;
    let shown = if status == "bridge" { "pending" } else { status };
    crate::ci::hooks::announce(state, &b, shown).await;
    if status == "pending" {
        state.events.publish(Event::JobsQueued { project_id: b.project_id });
    }
    process(state, b.pipeline_id).await
}

/// The status a `created` job moves to, or None while it must wait.
fn next_status(builds: &[Build], i: usize) -> Option<&'static str> {
    let b = &builds[i];
    let deps: Vec<&Build> = match &b.needs {
        Some(names) => builds.iter().filter(|o| names.contains(&o.name)).collect(),
        None => builds.iter().filter(|o| o.stage_idx < b.stage_idx).collect(),
    };
    // wait for every prerequisite (an optional manual job does not block)
    if !deps.iter().all(|d| is_complete(&d.status) || (d.status == "manual" && d.allow_failure)) {
        return None;
    }
    let failed = deps.iter().any(|d| (d.status == "failed" && !d.allow_failure) || d.status == "canceled");
    Some(match b.when_.as_str() {
        "on_success" if !failed => "pending",
        "on_failure" if failed => "pending",
        "always" => "pending",
        "manual" if !failed => "manual",
        "delayed" if !failed => "scheduled",
        _ => "skipped",
    })
}

/// GitLab's composite status, over the latest attempt of each job.
pub fn pipeline_status<'a>(builds: impl IntoIterator<Item = &'a Build>) -> &'static str {
    let live: Vec<&Build> = builds.into_iter().filter(|b| !b.retried).collect();
    if live.is_empty() {
        return "skipped";
    }
    let has = |s: &str| live.iter().any(|b| b.status == s);
    if has("running") || has("canceling") {
        return "running";
    }
    if has("pending") || has("preparing") {
        // some finished already: the pipeline is under way
        return if live.iter().any(|b| is_complete(&b.status)) { "running" } else { "pending" };
    }
    if has("waiting_for_resource") {
        return "waiting_for_resource";
    }
    if has("created") {
        return if live.iter().any(|b| b.status == "manual" && !b.allow_failure) { "manual" } else { "created" };
    }
    if has("scheduled") {
        return "scheduled";
    }
    if live.iter().any(|b| b.status == "manual" && !b.allow_failure) {
        return "manual";
    }
    if live.iter().any(|b| b.status == "failed" && !b.allow_failure) {
        return "failed";
    }
    if has("canceled") {
        return "canceled";
    }
    if live.iter().all(|b| b.status == "skipped" || b.status == "manual") {
        return if has("manual") { "manual" } else { "skipped" };
    }
    "success"
}

/// GitLab's automatic retry rules: the failure reason matches `retry:when`
/// (default `always` unless `exit_codes` is set), or the exit code matches.
pub fn should_retry(rules: &[String], reason: &str, exit_code: Option<i32>) -> bool {
    let codes: Vec<i32> = rules.iter().filter_map(|r| r.strip_prefix("exit_code:")).filter_map(|c| c.parse().ok()).collect();
    let mut when: Vec<&str> = rules.iter().filter(|r| !r.starts_with("exit_code:")).map(String::as_str).collect();
    if when.is_empty() && codes.is_empty() {
        when.push("always");
    }
    when.contains(&"always") || when.contains(&reason) || exit_code.is_some_and(|c| codes.contains(&c))
}

/// Cancel one locked job: not started → `canceled`; running → `canceling`
/// (the runner learns it through `Job-Status` on its next update).
async fn cancel_one(tx: &mut sqlx::PgConnection, b: &Build) -> anyhow::Result<Option<&'static str>> {
    if not_started(&b.status) {
        forgust_db::ci::dequeue(&mut *tx, b.id).await?;
        forgust_db::ci::set_build_status(&mut *tx, b.id, "canceled", None).await?;
        Ok(Some("canceled"))
    } else if b.status == "running" {
        forgust_db::ci::request_cancel(&mut *tx, b.id).await?;
        Ok(Some("canceling"))
    } else {
        Ok(None)
    }
}

/// Cancel every unfinished job of the pipeline.
pub async fn cancel(state: &AppState, pipeline_id: i64) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    let builds = forgust_db::ci::lock_pipeline_builds(&mut tx, pipeline_id).await?;
    let mut touched = Vec::new();
    for b in &builds {
        if let Some(s) = cancel_one(&mut tx, b).await? {
            touched.push((b.clone(), s));
        }
    }
    tx.commit().await?;
    for (b, s) in &touched {
        crate::ci::hooks::announce(state, b, s).await;
    }
    process(state, pipeline_id).await?;
    for (b, s) in &touched {
        // jobs that held (or waited for) a resource group let the next one go
        if *s == "canceled" {
            Box::pin(crate::ci::keywords::release_resource(state, b.id)).await?;
        }
        cancel_downstream(state, b, s).await?;
    }
    Ok(())
}

/// A bridge being canceled has no runner to report back: cancel its downstream
/// pipeline, whose end ends the bridge (`bridge::downstream_changed`).
async fn cancel_downstream(state: &AppState, b: &Build, status: &str) -> anyhow::Result<()> {
    if status == "canceling"
        && StoredSpec::of(b).is_ok_and(|s| s.job.trigger.is_some())
        && let Some(down) = forgust_db::ci::downstream_of(&state.db, b.id).await?
    {
        Box::pin(cancel(state, down)).await?;
    }
    Ok(())
}

/// Cancel one job.
pub async fn cancel_job(state: &AppState, build: &Build) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    let Some(b) = forgust_db::ci::build_for_update(&mut tx, build.id).await? else { return Ok(()) };
    let Some(status) = cancel_one(&mut tx, &b).await? else { return Ok(()) };
    tx.commit().await?;
    crate::ci::hooks::announce(state, &b, status).await;
    process(state, b.pipeline_id).await?;
    if status == "canceled" {
        Box::pin(crate::ci::keywords::release_resource(state, b.id)).await?;
    }
    cancel_downstream(state, &b, status).await
}

/// A job ended (runner report, payload failure, stuck-job sweep): record it if
/// it is still running, apply `allow_failure:exit_codes`, end its token, archive
/// its log, then retry it (`retry:`) or advance the pipeline. Returns the
/// recorded status, or the one another path recorded first.
pub async fn finish_job(state: &AppState, id: i64, status: &str, reason: Option<&str>, exit_code: Option<i32>) -> anyhow::Result<String> {
    let mut tx = state.db.begin().await?;
    let Some(b) = forgust_db::ci::build_for_update(&mut tx, id).await? else { return Ok("failed".into()) };
    if !holds_runner(&b.status) {
        return Ok(b.status);
    }
    // a job canceled while running ends canceled, whatever the runner says
    let status = if b.status == "canceling" { "canceled" } else { status };
    let reason = if status == "failed" { Some(reason.unwrap_or("script_failure")) } else { None };
    forgust_db::ci::finish_running(&mut *tx, id, status, reason, exit_code).await?;
    if status == "failed"
        && let Some(code) = exit_code
        && StoredSpec::of(&b).is_ok_and(|s| s.job.allow_failure_exit_codes.contains(&code))
    {
        forgust_db::ci::set_allow_failure(&mut *tx, id, true).await?;
    }
    forgust_db::ci::expire_build_tokens(&mut *tx, id).await?;
    tx.commit().await?;
    crate::ci::hooks::announce(state, &b, status).await;
    metrics::counter!("forgust_ci_jobs_finished_total", "status" => status.to_string()).increment(1);
    crate::ci::trace::archive_later(state, id);
    crate::ci::reports::after_job(state, id, b.project_id).await;
    crate::ci::keywords::after_job(state, &b, status).await;
    if status == "failed" && b.attempt <= b.retry_max && should_retry(&b.retry_when, reason.unwrap_or(""), exit_code) {
        let ended = forgust_db::ci::build(&state.db, id).await?.context("job vanished")?;
        retry_job(state, &ended, None).await?;
    } else {
        process(state, b.pipeline_id).await?;
    }
    Ok(status.to_string())
}

/// Run a job again as a new attempt (the old one stays, marked retried).
pub async fn retry_job(state: &AppState, build: &Build, user_id: Option<i64>) -> anyhow::Result<i64> {
    let mut tx = state.db.begin().await?;
    let b = forgust_db::ci::build_for_update(&mut tx, build.id).await?.context("job vanished")?;
    anyhow::ensure!(is_complete(&b.status) && !b.retried, "only finished jobs can be retried");
    forgust_db::ci::mark_retried(&mut *tx, b.id).await?;
    let resource_group = forgust_db::ci::resource_group_of(&mut *tx, b.id).await?.map(|(_, g)| g);
    // a built-in Renovate job runs as its bot, a scanner as who turned it
    // on, whoever retries it (ADR-0023, ADR-0024)
    let pipeline = forgust_db::ci::pipeline(&mut *tx, b.pipeline_id).await?.context("pipeline vanished")?;
    let system = is_system(&pipeline.source);
    let user_id = if system { b.user_id } else { user_id.or(b.user_id) };
    let id = forgust_db::ci::insert_build(
        &mut *tx,
        NewBuild {
            pipeline_id: b.pipeline_id,
            project_id: b.project_id,
            name: &b.name,
            stage: &b.stage,
            stage_idx: b.stage_idx,
            status: "created",
            // a manual job that was played runs again; one never played stays manual
            when_: if b.when_ == "manual" && b.queued_at.is_some() { "on_success" } else { &b.when_ },
            allow_failure: b.allow_failure,
            needs: b.needs.as_deref(),
            spec: &b.spec,
            retry_max: b.retry_max,
            retry_when: &b.retry_when,
            attempt: b.attempt + 1,
            tags: &b.tags,
            timeout_secs: b.timeout_secs,
            user_id,
            resource_group: resource_group.as_deref(),
        },
    )
    .await?;
    if let Some(env) = StoredSpec::of(&b).ok().and_then(|s| s.job.environment) {
        let project = forgust_db::projects::find(&mut *tx, b.project_id).await?.context("project vanished")?;
        crate::ci::environments::on_job_created(
            &mut tx,
            &project,
            id,
            &env,
            &pipeline.git_ref,
            pipeline.tag,
            &pipeline.sha,
            user_id.or(b.user_id),
        )
        .await?;
    }
    // jobs that were skipped because this one failed get another chance
    reset_downstream(&mut tx, b.pipeline_id, &b).await?;
    tx.commit().await?;
    process(state, b.pipeline_id).await?;
    Ok(id)
}

/// After a retry, skipped jobs that depend on the retried one go back to `created`.
async fn reset_downstream(tx: &mut sqlx::PgConnection, pipeline_id: i64, retried: &Build) -> anyhow::Result<()> {
    let builds = forgust_db::ci::lock_pipeline_builds(tx, pipeline_id).await?;
    let mut affected: HashSet<String> = HashSet::from([retried.name.clone()]);
    let by_name: HashMap<&str, &Build> = builds.iter().map(|b| (b.name.as_str(), b)).collect();
    // transitive: later-stage jobs, or jobs that need an affected job
    loop {
        let before = affected.len();
        for b in &builds {
            let depends = match &b.needs {
                Some(n) => n.iter().any(|x| affected.contains(x)),
                None => affected.iter().filter_map(|a| by_name.get(a.as_str())).any(|a| a.stage_idx < b.stage_idx),
            };
            if depends {
                affected.insert(b.name.clone());
            }
        }
        if affected.len() == before {
            break;
        }
    }
    for b in &builds {
        if b.id != retried.id && affected.contains(&b.name) && b.status == "skipped" {
            forgust_db::ci::set_build_status(&mut *tx, b.id, "created", None).await?;
        }
    }
    Ok(())
}

/// Start a manual job.
pub async fn play_job(state: &AppState, build: &Build, user_id: Option<i64>) -> anyhow::Result<()> {
    let mut tx = state.db.begin().await?;
    let mut b = forgust_db::ci::build_for_update(&mut tx, build.id).await?.context("job vanished")?;
    anyhow::ensure!(b.status == "manual" || b.status == "scheduled", "only manual and delayed jobs can be started");
    if let Some(u) = user_id {
        forgust_db::ci::set_build_user(&mut *tx, b.id, u).await?;
        b.user_id = Some(u);
    }
    let status = admit(state, &mut tx, &b).await?;
    tx.commit().await?;
    crate::ci::hooks::announce(state, &b, if status == "bridge" { "pending" } else { status }).await;
    if status == "pending" {
        state.events.publish(Event::JobsQueued { project_id: b.project_id });
    }
    process(state, b.pipeline_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_rules_follow_gitlab() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(should_retry(&[], "script_failure", Some(1)), "max alone: always");
        assert!(should_retry(&s(&["runner_system_failure"]), "runner_system_failure", None));
        assert!(!should_retry(&s(&["runner_system_failure"]), "script_failure", Some(1)));
        assert!(should_retry(&s(&["exit_code:137"]), "script_failure", Some(137)));
        assert!(!should_retry(&s(&["exit_code:137"]), "script_failure", Some(1)), "exit_codes alone: no default always");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("3600"), Some(3600));
        assert_eq!(parse_duration("1h 30m"), Some(5400));
        assert_eq!(parse_duration("1h30m"), Some(5400));
        assert_eq!(parse_duration("2 hours"), Some(7200));
        assert_eq!(parse_duration("1 day and 2 hours"), Some(93600));
        assert_eq!(parse_duration("1 week"), Some(604800));
        assert_eq!(parse_duration("never"), None);
    }
}
