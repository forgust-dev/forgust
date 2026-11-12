//! The job payload sent to the runner on `POST /jobs/request`, field for field
//! after GitLab's `Entities::Ci::JobRequest::Response` (see the CI compatibility guide).

use forgust_db::ci::{Build, Pipeline};
use forgust_db::projects::Project;
use serde_json::{Value, json};

use crate::ci::variables::{self, Var};
use crate::state::AppState;

/// Failure reasons we understand; the runner maps anything else to
/// `unknown_failure` (failure_reason_mapper.go).
pub const FAILURE_REASONS: &[&str] = &[
    "script_failure",
    "runner_system_failure",
    "job_execution_timeout",
    "image_pull_failure",
    "unknown_failure",
    "runner_configuration_error",
    "runner_external_dependency_failure",
    "runner_interrupted",
    "stuck_or_timeout_failure",
];

/// Persistent ref that pins the pipeline's commit while jobs fetch it.
pub fn pipeline_ref(pipeline_id: i64) -> String {
    format!("refs/pipelines/{pipeline_id}")
}

/// `http://gitlab-ci-token:<token>@host/path.git`
pub fn repo_url_with_token(state: &AppState, full_path: &str, token: &str) -> String {
    let url = state.cfg.http_clone_url(full_path);
    match url.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://gitlab-ci-token:{token}@{rest}"),
        None => url,
    }
}

pub struct Context<'a> {
    pub build: &'a Build,
    pub spec: &'a crate::ci::pipeline::StoredSpec,
    pub pipeline: &'a Pipeline,
    pub project: &'a Project,
    pub token: &'a str,
    pub variables: Vec<Var>,
    pub dependencies: Vec<(Build, Option<(String, i64)>)>,
    pub git_depth: i32,
    /// Keys computed for `cache:key:files` (None: use the literal key).
    pub cache_keys: Vec<Option<String>>,
}

/// `cache:key:files`: `<prefix>-<last commit touching any of the files>`, as
/// GitLab computes it, so the cache changes when a lockfile does.
pub async fn cache_keys(state: &AppState, project_id: i64, sha: &str, caches: &[forgust_ci_config::Cache]) -> Vec<Option<String>> {
    let mut out = Vec::new();
    for c in caches {
        if c.key_files.is_empty() {
            out.push(None);
            continue;
        }
        let mut latest: Option<(i64, String)> = None;
        for f in &c.key_files {
            if let Ok(commits) = state.storage.log(project_id, sha, Some(f.trim_start_matches('/')), 0, 1).await
                && let Some(commit) = commits.first()
                && latest.as_ref().is_none_or(|(t, _)| commit.committer.time > *t)
            {
                latest = Some((commit.committer.time, commit.id.clone()));
            }
        }
        let prefix = c.key_prefix.clone().unwrap_or_else(|| "default".into());
        out.push(Some(match latest {
            Some((_, id)) => format!("{prefix}-{id}"),
            None => prefix,
        }));
    }
    out
}

fn image_json(i: &forgust_ci_config::Image, service: bool) -> Value {
    let mut v = json!({
        "name": i.name,
        "entrypoint": i.entrypoint,
        "ports": [],
        "pull_policy": if i.pull_policy.is_empty() { Value::Null } else { json!(i.pull_policy) },
    });
    let mut docker = serde_json::Map::new();
    if let Some(p) = &i.docker_platform {
        docker.insert("platform".into(), json!(p));
    }
    if let Some(u) = &i.docker_user {
        docker.insert("user".into(), json!(u));
    }
    if !docker.is_empty() {
        v["executor_opts"] = json!({ "docker": docker });
    }
    if service {
        v["alias"] = json!(i.alias.clone().unwrap_or_default());
        v["command"] = json!(i.command);
        v["variables"] = Value::Array(i.variables.iter().map(|(k, val)| json!({ "key": k, "value": val, "public": true })).collect());
    }
    v
}

/// GitLab's report artifact formats (`artifact_format` per type).
fn report_format(kind: &str) -> &'static str {
    match kind {
        "dotenv"
        | "cobertura"
        | "junit"
        | "terraform"
        | "metrics"
        | "codequality"
        | "sast"
        | "dependency_scanning"
        | "container_scanning"
        | "dast"
        | "secret_detection"
        | "license_scanning"
        | "requirements"
        | "accessibility"
        | "browser_performance"
        | "load_performance"
        | "annotations"
        | "cyclonedx"
        | "coverage_report" => "gzip",
        _ => "zip",
    }
}

pub fn job_response(state: &AppState, c: &Context<'_>) -> anyhow::Result<Value> {
    let job = &c.spec.job;
    let b = c.build;
    let mut script = job.before_script.clone();
    script.extend(job.script.iter().cloned());
    let mut steps = vec![json!({
        "name": "script", "script": script, "timeout": b.timeout_secs, "when": "on_success", "allow_failure": false,
    })];
    if !job.after_script.is_empty() {
        steps.push(json!({
            "name": "after_script", "script": job.after_script, "timeout": b.timeout_secs, "when": "always", "allow_failure": true,
        }));
    }

    let image = job.image.as_ref().map(|i| image_json(i, false));
    let services: Vec<Value> = job.services.iter().map(|s| image_json(s, true)).collect();

    let mut artifacts = Vec::new();
    if let Some(a) = &job.artifacts {
        if !a.paths.is_empty() || a.untracked {
            artifacts.push(json!({
                "name": a.name.clone().unwrap_or_else(|| "artifacts".into()),
                "untracked": a.untracked,
                "paths": a.paths,
                "exclude": a.exclude,
                "when": a.when.clone().unwrap_or_else(|| "on_success".into()),
                "expire_in": a.expire_in,
                "artifact_type": "archive",
                "artifact_format": "zip",
            }));
        }
        for (kind, paths) in &a.reports {
            let format = report_format(kind);
            artifacts.push(json!({
                "name": format!("{kind}.{}", if format == "gzip" { "gz" } else { "zip" }),
                "paths": paths,
                "when": "always",
                "expire_in": a.expire_in,
                "artifact_type": kind,
                "artifact_format": format,
            }));
        }
    }

    // protected refs never share a cache with the rest: any Developer can fill
    // an unprotected one, and protected jobs run with protected variables
    // (GitLab's "separate caches for protected branches", always on here)
    // Forgust's own pipelines (built-in Renovate, scanners) have a cache no
    // project job can name (theirs always end in one of the two above): a job
    // could otherwise plant files (a Renovate config.js) that run with the
    // instance's credentials
    let scope = match c.pipeline.source.as_str() {
        s if crate::ci::pipeline::is_system(s) => format!("-forgust_{s}"),
        _ if c.pipeline.protected => "-protected".to_string(),
        _ => "-non_protected".to_string(),
    };
    let cache: Vec<Value> = job
        .cache
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let key = c.cache_keys.get(i).cloned().flatten().or_else(|| k.key.clone()).unwrap_or_else(|| "default".into());
            let fallback: Vec<String> = k.fallback_keys.iter().map(|f| format!("{f}{scope}")).collect();
            json!({
                "key": format!("{key}{scope}"),
                "untracked": k.untracked,
                "policy": k.policy.clone().unwrap_or_else(|| "pull-push".into()),
                "paths": k.paths,
                "when": k.when.clone().unwrap_or_else(|| "on_success".into()),
                "fallback_keys": fallback,
            })
        })
        .collect();

    let dependencies: Vec<Value> = c
        .dependencies
        .iter()
        .map(|(d, file)| {
            let mut v = json!({ "id": d.id, "name": d.name, "token": c.token });
            if let Some((filename, size)) = file {
                v["artifacts_file"] = json!({ "filename": filename, "size": size });
            }
            v
        })
        .collect();

    let (git_ref, ref_type) = (c.pipeline.git_ref.clone(), if c.pipeline.tag { "tag" } else { "branch" });
    let mut refspecs = vec![format!("+{}:{}", c.pipeline.sha, pipeline_ref(c.pipeline.id))];
    if c.git_depth > 0 {
        if c.pipeline.tag {
            refspecs.push(format!("+refs/tags/{git_ref}:refs/tags/{git_ref}"));
        } else {
            refspecs.push(format!("+refs/heads/{git_ref}:refs/remotes/origin/{git_ref}"));
        }
    } else {
        refspecs.push("+refs/heads/*:refs/remotes/origin/*".into());
        refspecs.push("+refs/tags/*:refs/tags/*".into());
    }

    let root_ns = c.project.traversal_ids.first().copied().unwrap_or(c.project.namespace_id);
    let queued = b.queued_at.map(|q| (chrono::Utc::now() - q).num_milliseconds() as f64 / 1000.0).unwrap_or(0.0);

    Ok(json!({
        "id": b.id,
        "token": c.token,
        "allow_git_fetch": true,
        "job_info": {
            "id": b.id,
            "name": b.name,
            "stage": b.stage,
            "pipeline_id": c.pipeline.id,
            "project_id": c.project.id,
            "project_name": c.project.name,
            "project_full_path": c.project.full_path,
            "namespace_id": c.project.namespace_id,
            "root_namespace_id": root_ns,
            "organization_id": root_ns,
            "instance_id": "",
            "instance_uuid": "",
            "user_id": b.user_id.unwrap_or(0),
            "scoped_user_id": null,
            "time_in_queue_seconds": queued,
            "project_jobs_running_on_instance_runners_count": "+Inf",
            "queue_size": 0,
            "queue_depth": 0,
        },
        "git_info": {
            "repo_url": repo_url_with_token(state, &c.project.full_path, c.token),
            "repo_object_format": "sha1",
            "ref": git_ref,
            "sha": c.pipeline.sha,
            "before_sha": c.pipeline.before_sha,
            "ref_type": ref_type,
            "refspecs": refspecs,
            "depth": c.git_depth,
            "protected": c.pipeline.protected,
        },
        "runner_info": { "timeout": b.timeout_secs, "runner_session_url": null },
        "inputs": [],
        "variables": variables::to_runner(&c.variables),
        "steps": steps,
        "image": image,
        "services": services,
        "artifacts": artifacts,
        "cache": cache,
        // the runner logs in to Forgust's registry to pull private images
        "credentials": if state.cfg.registry.enabled {
            json!([{ "type": "registry", "url": crate::registry::host(state), "username": "gitlab-ci-token", "password": c.token }])
        } else {
            json!([])
        },
        "dependencies": dependencies,
        "features": {
            "trace_sections": true,
            "token_mask_prefixes": ["fgpat-", "fgcbt-", "fgdt-", "glrt-", "glpat-"],
            "failure_reasons": FAILURE_REASONS,
        },
    }))
}
