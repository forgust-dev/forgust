//! Built-in security scanners (ADR-0024). A Maintainer turns
//! scanning on; Forgust writes the pipeline itself (source `security`): pinned
//! images, instance runners only, none of the project's CI variables, run as
//! who turned it on. The jobs upload reports slice 1 reads into findings.

use chrono::{DateTime, Duration, Utc};
use forgust_db::projects::Project;
use forgust_db::security as sdb;
use serde_json::{Value, json};

use crate::auth::Principal;
use crate::auth::authz::ProjectAction;
use crate::config::SecurityScanningConfig;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const SOURCE: &str = "security";
/// What a project may turn on, and what it gets by default (containers only
/// mean something with images in the registry).
pub const SCANNERS: &[&str] = &["sast", "secret_detection", "dependency_scanning", "iac", "container_scanning"];
pub const DEFAULT_SCANNERS: &[&str] = &["sast", "secret_detection", "dependency_scanning", "iac"];
/// Images scanned per run, at most (each repository's newest tag).
const MAX_IMAGES: usize = 5;

/// The config file's `[security_scanning]`, with what administrators changed.
pub async fn config(state: &AppState) -> anyhow::Result<SecurityScanningConfig> {
    let mut v = serde_json::to_value(&state.cfg.security_scanning)?;
    if let (Some(base), Value::Object(over)) = (v.as_object_mut(), sdb::scanning_overrides(&state.db).await?) {
        for (k, x) in over {
            if base.contains_key(&k) {
                base.insert(k, x);
            }
        }
    }
    Ok(serde_json::from_value(v)?)
}

pub fn next_after(from: DateTime<Utc>, project_id: i64, interval_hours: i32) -> DateTime<Utc> {
    // spread projects over the interval so they do not all start at once
    from + Duration::hours(interval_hours as i64) + Duration::minutes(project_id % 30)
}

pub struct Change {
    pub enabled: bool,
    pub scanners: Vec<String>,
    pub interval_hours: i32,
    /// `None`: as it was.
    pub sbom_on_tags: Option<bool>,
}

/// Turn scanning on (the first run is due now, as the person turning it on)
/// or off (runs going on stop; findings stay).
pub async fn configure(state: &AppState, principal: &Principal, project: &Project, c: Change) -> ApiResult<()> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    if c.enabled && !config(state).await.map_err(ApiError::Internal)?.enabled {
        return Err(ApiError::conflict("security scanning is turned off on this instance"));
    }
    if !(1..=168).contains(&c.interval_hours) {
        return Err(ApiError::bad("scans are 1 to 168 hours apart"));
    }
    let mut scanners: Vec<String> = Vec::new();
    for s in &c.scanners {
        if !SCANNERS.contains(&s.as_str()) {
            return Err(ApiError::bad(format!("unknown scanner `{s}` (one of {})", SCANNERS.join(", "))));
        }
        if !scanners.contains(s) {
            scanners.push(s.clone());
        }
    }
    if c.enabled && scanners.is_empty() {
        return Err(ApiError::bad("pick at least one scanner"));
    }
    let before = sdb::scanning(&state.db, project.id).await?;
    let was_on = before.as_ref().is_some_and(|s| s.enabled);
    let next = if c.enabled {
        match &before {
            Some(s) if was_on => s.next_run_at.or(Some(Utc::now())),
            _ => Some(Utc::now()),
        }
    } else {
        for pl in sdb::active_scans(&state.db, Some(project.id)).await? {
            crate::ci::pipeline::cancel(state, pl).await.map_err(ApiError::Internal)?;
        }
        None
    };
    let owner = if c.enabled { Some(user.id) } else { before.as_ref().and_then(|s| s.user_id) };
    let had_sbom = before.as_ref().is_some_and(|s| s.sbom_on_tags);
    let sbom = c.sbom_on_tags.unwrap_or(had_sbom);
    // SBOM runs going on stop with scanning, or with SBOMs on tags
    if !c.enabled || (had_sbom && !sbom) {
        super::sbom::cancel_runs(state, project.id).await.map_err(ApiError::Internal)?;
    }
    sdb::save_scanning(
        &state.db,
        sdb::SaveScanning {
            project_id: project.id,
            enabled: c.enabled,
            scanners: &scanners,
            interval_hours: c.interval_hours,
            user_id: owner,
            next_run_at: next,
            sbom_on_tags: sbom,
        },
    )
    .await?;
    if c.enabled != was_on {
        let action = if c.enabled { "security_scanning_enabled" } else { "security_scanning_disabled" };
        super::audit(state, principal, action, Some(("project", project.id)), json!({ "scanners": scanners }), None).await;
    }
    if sbom != had_sbom {
        let action = if sbom { "sbom_on_tags_enabled" } else { "sbom_on_tags_disabled" };
        super::audit(state, principal, action, Some(("project", project.id)), json!({}), None).await;
    }
    Ok(())
}

/// A group's default for its projects (those that never chose, and new ones).
pub async fn set_group_default(
    state: &AppState,
    principal: &Principal,
    ns: &forgust_db::namespaces::Namespace,
    enabled: bool,
) -> ApiResult<usize> {
    super::builtin::set_group_default::<Scanning>(state, principal, ns, enabled).await
}

fn defaults() -> Change {
    Change { enabled: true, scanners: DEFAULT_SCANNERS.iter().map(|s| s.to_string()).collect(), interval_hours: 24, sbom_on_tags: None }
}

/// A new project under a group that turned scanning on gets it.
pub async fn on_project_created(state: &AppState, principal: &Principal, project: &Project) {
    super::builtin::on_project_created::<Scanning>(state, principal, project).await
}

/// Built-in security scanning for the shared scheduler.
struct Scanning;

impl super::builtin::Builtin for Scanning {
    const FEATURE: &'static str = "security scanning";
    const RUN: &'static str = "security scan";
    const GROUP_AUDIT: &'static str = "security_scanning_group_default";

    async fn limits(state: &AppState) -> anyhow::Result<Option<(i64, f64)>> {
        let cfg = config(state).await?;
        Ok(cfg.enabled.then_some((cfg.max_concurrent as i64, cfg.timeout_secs as f64)))
    }
    async fn stale(state: &AppState, secs: f64) -> anyhow::Result<Vec<i64>> {
        Ok(sdb::stale_scans(&state.db, secs).await?)
    }
    async fn active(state: &AppState) -> anyhow::Result<i64> {
        Ok(sdb::active_scans(&state.db, None).await?.len() as i64)
    }
    async fn due(state: &AppState, n: i64) -> anyhow::Result<Vec<i64>> {
        Ok(sdb::due_scans(&state.db, n).await?)
    }
    async fn run(state: &AppState, project: &Project) -> ApiResult<Option<i64>> {
        run(state, project).await
    }
    async fn later(state: &AppState, project_id: i64, at: DateTime<Utc>) -> anyhow::Result<()> {
        Ok(sdb::scan_started(&state.db, project_id, None, at).await?)
    }
    async fn set_group(state: &AppState, namespace_id: i64, enabled: bool) -> anyhow::Result<()> {
        Ok(sdb::set_scanning_group_default(&state.db, namespace_id, enabled).await?)
    }
    async fn undecided_under(state: &AppState, namespace_id: i64) -> anyhow::Result<Vec<i64>> {
        Ok(sdb::scanning_undecided_under(&state.db, namespace_id).await?)
    }
    async fn inherited(state: &AppState, traversal_ids: &[i64]) -> anyhow::Result<bool> {
        Ok(sdb::scanning_inherited(&state.db, traversal_ids).await?)
    }
    async fn turn_on(state: &AppState, principal: &Principal, project: &Project) -> ApiResult<()> {
        configure(state, principal, project, defaults()).await
    }
}

/// `job` with the keys of `extra` set (replacing those it had).
fn with(mut job: Value, extra: Value) -> Value {
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        job[k] = v;
    }
    job
}

/// A job of Forgust's security pipelines: its image, the instance's runner
/// tags and timeout, always run.
fn job(cfg: &SecurityScanningConfig, image: Value, extra: Value) -> Value {
    let base = json!({
        "image": image,
        "tags": cfg.tags,
        "timeout": format!("{} minutes", cfg.timeout_secs.div_ceil(60).max(1)),
        "rules": [{ "when": "always" }],
    });
    with(base, extra)
}

/// A Trivy job (scans, SBOMs): the instance's Trivy, its cache, where it
/// reads its databases (`mirror`), and `variables` of its own. Scripts call
/// it with `--config /dev/null`: never a `trivy.yaml` from the branch (it
/// could point Trivy, and the job's credentials, elsewhere).
pub(super) fn trivy_job(
    cfg: &SecurityScanningConfig,
    mirror: &super::vulnerability_dbs::TrivyMirror,
    variables: Value,
    extra: Value,
) -> Value {
    let mut vars = json!({ "TRIVY_CACHE_DIR": ".trivycache", "TRIVY_NO_PROGRESS": "true" });
    for (k, v) in mirror.variables().into_iter().chain(variables.as_object().cloned().unwrap_or_default()) {
        vars[k] = v;
    }
    let image = json!({ "name": cfg.trivy_image, "entrypoint": [""] });
    let cache = json!({ "key": "trivy", "paths": [".trivycache/"], "when": "always" });
    job(cfg, image, with(json!({ "variables": vars, "cache": cache }), extra))
}

/// The pipeline Forgust writes: one job per scanner, each uploading a report
/// slice 1 reads. `history`: the secrets scanner looks at every commit.
/// `mirror`: where Trivy reads its databases, when this server holds them.
pub fn job_config(
    cfg: &SecurityScanningConfig,
    scanners: &[String],
    history: bool,
    images: &[String],
    mirror: &super::vulnerability_dbs::TrivyMirror,
) -> String {
    let mut jobs = serde_json::Map::new();
    let on = |s: &str| scanners.iter().any(|x| x == s);
    if on("sast") {
        let extra = json!({ "script": ["/analyzer run"], "artifacts": { "reports": { "sast": "gl-sast-report.json" } } });
        jobs.insert("code".into(), job(cfg, json!(cfg.sast_image), extra));
    }
    if on("secret_detection") {
        let mut vars = json!({});
        if history {
            // the first run reads every commit
            vars = json!({ "GIT_DEPTH": "0", "SECRET_DETECTION_HISTORIC_SCAN": "true" });
        }
        let extra = json!({ "variables": vars, "script": ["/analyzer run"], "artifacts": { "reports": { "secret_detection": "gl-secret-detection-report.json" } } });
        jobs.insert("secrets".into(), job(cfg, json!(cfg.secrets_image), extra));
    }
    if on("dependency_scanning") {
        let extra = json!({
            "script": ["trivy fs --config /dev/null --scanners vuln --format sarif --output trivy-dependencies.sarif --exit-code 0 ."],
            "artifacts": { "reports": { "dependency_scanning": "trivy-dependencies.sarif" } },
        });
        jobs.insert("dependencies".into(), trivy_job(cfg, mirror, json!({}), extra));
    }
    if on("iac") {
        let extra = json!({
            "script": ["trivy config --config /dev/null --format sarif --output trivy-iac.sarif --exit-code 0 ."],
            "artifacts": { "reports": { "sast": "trivy-iac.sarif" } },
        });
        jobs.insert("infrastructure".into(), trivy_job(cfg, mirror, json!({}), extra));
    }
    if on("container_scanning") && !images.is_empty() {
        let script: Vec<String> = images
            .iter()
            .enumerate()
            .map(|(i, img)| {
                format!(
                    "trivy image --config /dev/null --format sarif --output trivy-container-{i}.sarif --exit-code 0 '{}'",
                    img.replace('\'', "")
                )
            })
            .collect();
        let files: Vec<String> = (0..images.len()).map(|i| format!("trivy-container-{i}.sarif")).collect();
        // pulls from this instance's registry with the job's own credentials
        let vars = json!({ "GIT_STRATEGY": "none", "TRIVY_USERNAME": "$CI_REGISTRY_USER", "TRIVY_PASSWORD": "$CI_REGISTRY_PASSWORD" });
        let extra = json!({ "script": script, "artifacts": { "reports": { "container_scanning": files } } });
        jobs.insert("containers".into(), trivy_job(cfg, mirror, vars, extra));
    }
    // JSON is YAML
    Value::Object(jobs).to_string()
}

/// The newest tag of each of the project's image repositories.
async fn images_of(state: &AppState, project: &Project) -> anyhow::Result<Vec<String>> {
    let base = crate::registry::image_of(state, &project.full_path);
    // each repository's newest tag, in one query
    Ok(forgust_db::registry::newest_tags(&state.db, project.id)
        .await?
        .into_iter()
        .take(MAX_IMAGES)
        .map(|(repo, tag)| if repo.is_empty() { format!("{base}:{tag}") } else { format!("{base}/{repo}:{tag}") })
        .collect())
}

/// The project's scanner bot the runs are made as (a Reporter: their job
/// token can clone and pull images, nothing of whoever turned scanning on),
/// while who turned scanning on may still read the code.
pub(super) async fn run_as(state: &AppState, project: &Project, s: &sdb::ScanningSettings) -> ApiResult<Option<forgust_db::users::User>> {
    if runner_user(state, project, s.user_id).await.map_err(ApiError::Internal)?.is_none() {
        return Ok(None);
    }
    let kind = super::renovate::ProjectBot {
        username: format!("security-bot-{}", project.id),
        name: "Security scanning",
        user_type: "security_bot",
        level: forgust_db::access::REPORTER,
    };
    Ok(Some(super::renovate::ensure_project_bot(state, project, &kind, None).await?))
}

/// Who the runs belong to, if they still may read the code.
async fn runner_user(state: &AppState, project: &Project, user_id: Option<i64>) -> anyhow::Result<Option<forgust_db::users::User>> {
    let Some(id) = user_id else { return Ok(None) };
    let Some(u) = forgust_db::users::find(&state.db, id).await? else { return Ok(None) };
    if u.state != "active" {
        return Ok(None);
    }
    let p = Principal::User { user: u.clone(), scopes: None };
    Ok(crate::auth::authz::can_project(state, &p, ProjectAction::ReadCode, project).await?.then_some(u))
}

/// Scan `git_ref` at `sha` (the default branch, or a merge request's head).
async fn start(
    state: &AppState,
    project: &Project,
    s: &sdb::ScanningSettings,
    git_ref: &str,
    sha: &str,
    merge_request_id: Option<i64>,
) -> ApiResult<Option<i64>> {
    let cfg = config(state).await.map_err(ApiError::Internal)?;
    if !cfg.enabled {
        return Err(ApiError::conflict("security scanning is turned off on this instance"));
    }
    let Some(user) = run_as(state, project, s).await? else {
        return Err(ApiError::conflict("who turned security scanning on can no longer read this project: turn it on again"));
    };
    let images = if s.scanners.iter().any(|x| x == "container_scanning") {
        images_of(state, project).await.map_err(ApiError::Internal)?
    } else {
        vec![]
    };
    let mirror = super::vulnerability_dbs::trivy_mirror(state, &cfg).await.map_err(ApiError::Internal)?;
    let config_text = job_config(&cfg, &s.scanners, !s.history_scanned && merge_request_id.is_none(), &images, &mirror);
    if config_text == "{}" {
        return Ok(None);
    }
    let created = crate::ci::pipeline::create(
        state,
        crate::ci::pipeline::Trigger {
            merge_request_id,
            config: Some(config_text),
            ..crate::ci::pipeline::Trigger::new(project.id, git_ref, sha, SOURCE, Some(user.id))
        },
    )
    .await
    .map_err(ApiError::Internal)?;
    Ok(created.id())
}

/// Scan the default branch now.
pub async fn run(state: &AppState, project: &Project) -> ApiResult<Option<i64>> {
    crate::auth::authz::writable(project)?;
    let Some(s) = sdb::scanning(&state.db, project.id).await?.filter(|s| s.enabled) else {
        return Err(ApiError::conflict("security scanning is off for this project"));
    };
    let _lock = super::builtin::start_lock::<Scanning>(state, project.id).await?;
    if !sdb::active_scans(&state.db, Some(project.id)).await?.is_empty() {
        return Err(ApiError::conflict("a scan is already running"));
    }
    let next = next_after(Utc::now(), project.id, s.interval_hours);
    let branch = project.default_branch.clone().unwrap_or_else(|| "main".into());
    let Some(sha) = state.storage.resolve(project.id, &format!("refs/heads/{branch}")).await? else {
        sdb::scan_started(&state.db, project.id, None, next).await?;
        return Ok(None);
    };
    let id = start(state, project, &s, &branch, &sha, None).await?;
    sdb::scan_started(&state.db, project.id, id, next).await?;
    Ok(id)
}

/// Every minute: start due scans (shared with Renovate: [`super::builtin`]).
pub async fn run_due(state: &AppState) -> anyhow::Result<()> {
    super::builtin::run_due::<Scanning>(state).await
}

/// A push to the default branch: scan it soon (not more than every 10 minutes).
pub async fn on_default_branch_push(state: &AppState, project_id: i64) {
    let _ = sdb::bring_scan_forward(&state.db, project_id, Utc::now() + Duration::minutes(5), 600.0).await;
}

/// A merge request's head moved (or it opened): scan it, once per commit.
pub async fn on_merge_request_head(state: &AppState, project: &Project, mr: &forgust_db::merge_requests::MergeRequest, sha: &str) {
    let r: ApiResult<()> = async {
        let Some(s) = sdb::scanning(&state.db, project.id).await?.filter(|s| s.enabled) else { return Ok(()) };
        if project.archived() || mr.state != "opened" {
            return Ok(());
        }
        // the push and the merge request's opening may both get here: one scan
        let mut lock = state.db.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("mr-scan:{}:{sha}", mr.id))
            .execute(&mut *lock)
            .await?;
        if !sdb::mr_scanned(&state.db, mr.id, sha).await? {
            start(state, project, &s, &mr.source_branch, sha, Some(mr.id)).await?;
        }
        lock.commit().await?;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::info!(project_id = project.id, mr = mr.iid, error = ?e, "merge request not scanned");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipeline_parses() {
        let cfg = SecurityScanningConfig::default();
        let all: Vec<String> = SCANNERS.iter().map(|s| s.to_string()).collect();
        let mirror = super::super::vulnerability_dbs::TrivyMirror(vec![
            ("TRIVY_DB_REPOSITORY", "git.example.com/admin/mirrors/trivy-db:2".into()),
            ("TRIVY_CHECKS_BUNDLE_REPOSITORY", "git.example.com/admin/mirrors/trivy-checks:1".into()),
        ]);
        let text = job_config(&cfg, &all, true, &["registry.example.com/acme/shop:1.0".into()], &mirror);
        let parsed = forgust_ci_config::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        let mut names: Vec<&String> = parsed.jobs.keys().collect();
        names.sort();
        assert_eq!(names, ["code", "containers", "dependencies", "infrastructure", "secrets"]);
        for job in ["dependencies", "infrastructure", "containers"] {
            let vars = &parsed.jobs[job].variables;
            assert_eq!(vars.get("TRIVY_DB_REPOSITORY").map(String::as_str), Some("git.example.com/admin/mirrors/trivy-db:2"), "{job}");
            assert!(vars.contains_key("TRIVY_CACHE_DIR"), "{job}");
        }
    }
}
