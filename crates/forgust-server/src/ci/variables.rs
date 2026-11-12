//! CI/CD variables, in GitLab's precedence order (lowest first; the runner
//! takes the last value of a key): predefined, top-level YAML (and
//! `workflow:rules:variables`), job YAML, group, project, then pipeline
//! variables (web form, API, schedule).

use forgust_ci_config::Job;
use forgust_db::ci::{Build, Runner};
use forgust_db::projects::Project;
use serde_json::{Value, json};

use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct Var {
    pub key: String,
    pub value: String,
    /// Not secret (predefined and YAML values).
    pub public: bool,
    pub masked: bool,
    pub file: bool,
    /// Sent as-is, without `$VAR` expansion.
    pub raw: bool,
}

impl Var {
    pub fn public(key: &str, value: impl Into<String>) -> Var {
        Var { key: key.into(), value: value.into(), public: true, masked: false, file: false, raw: false }
    }
    pub fn secret(key: &str, value: impl Into<String>) -> Var {
        Var { key: key.into(), value: value.into(), public: false, masked: false, file: false, raw: false }
    }
}

pub fn to_runner(vars: &[Var]) -> Value {
    Value::Array(
        vars.iter()
            .map(|v| json!({ "key": v.key, "value": v.value, "public": v.public, "masked": v.masked, "file": v.file, "raw": v.raw }))
            .collect(),
    )
}

/// Lowercased, non-alphanumerics replaced by `-`, at most 63 bytes, no
/// leading/trailing `-` (GitLab's `CI_COMMIT_REF_SLUG` rules).
pub fn slugify(s: &str) -> String {
    let mut out: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    out.truncate(63);
    out.trim_matches('-').to_string()
}

fn rfc3339(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// What every job of a pipeline shares (also what `rules` are evaluated with).
pub struct PipelineInfo<'a> {
    pub project: &'a Project,
    pub git_ref: &'a str,
    pub tag: bool,
    pub sha: &'a str,
    pub before_sha: &'a str,
    pub source: &'a str,
    pub protected: bool,
    /// Once the pipeline exists: id, iid, created_at.
    pub pipeline: Option<&'a forgust_db::ci::Pipeline>,
    pub config_path: &'a str,
    pub user: Option<&'a forgust_db::users::User>,
    pub commit: Option<&'a forgust_storage::CommitInfo>,
    pub mr: Option<&'a forgust_db::merge_requests::MergeRequest>,
}

pub fn pipeline_vars(state: &AppState, i: &PipelineInfo<'_>) -> Vec<Var> {
    let ext = state.cfg.server.external_url.as_str();
    let (scheme, hostport) = ext.split_once("://").unwrap_or(("http", ext));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), p.to_string()),
        _ => (hostport.to_string(), if scheme == "https" { "443".into() } else { "80".into() }),
    };
    let p = i.project;
    let project_url = format!("{ext}/{}", p.full_path);
    let namespace = p.full_path.rsplit_once('/').map(|(n, _)| n.to_string()).unwrap_or_default();
    let root_namespace = p.full_path.split('/').next().unwrap_or_default().to_string();
    let mut v = vec![
        Var::public("CI", "true"),
        Var::public("GITLAB_CI", "true"),
        Var::public("CI_SERVER", "yes"),
        Var::public("CI_SERVER_NAME", "Forgust"),
        Var::public("CI_SERVER_VERSION", env!("CARGO_PKG_VERSION")),
        Var::public("CI_SERVER_URL", ext),
        Var::public("CI_SERVER_HOST", host.clone()),
        Var::public("CI_SERVER_PORT", port),
        Var::public("CI_SERVER_PROTOCOL", scheme),
        Var::public("CI_SERVER_FQDN", host),
        Var::public("CI_API_V4_URL", format!("{ext}/api/v4")),
        Var::public("CI_PROJECT_ID", p.id.to_string()),
        Var::public("CI_PROJECT_NAME", p.path.clone()),
        Var::public("CI_PROJECT_TITLE", p.name.clone()),
        Var::public("CI_PROJECT_DESCRIPTION", p.description.clone()),
        Var::public("CI_PROJECT_PATH", p.full_path.clone()),
        Var::public("CI_PROJECT_PATH_SLUG", slugify(&p.full_path)),
        Var::public("CI_PROJECT_NAMESPACE", namespace),
        Var::public("CI_PROJECT_NAMESPACE_ID", p.namespace_id.to_string()),
        Var::public("CI_PROJECT_ROOT_NAMESPACE", root_namespace),
        Var::public("CI_PROJECT_URL", project_url.clone()),
        Var::public("CI_PROJECT_VISIBILITY", forgust_db::visibility::name(p.visibility)),
        Var::public("CI_DEFAULT_BRANCH", p.default_branch.clone().unwrap_or_default()),
        Var::public("CI_CONFIG_PATH", i.config_path),
        Var::public("CI_PIPELINE_SOURCE", i.source),
        Var::public("CI_COMMIT_SHA", i.sha),
        Var::public("CI_COMMIT_SHORT_SHA", i.sha.chars().take(8).collect::<String>()),
        Var::public("CI_COMMIT_BEFORE_SHA", i.before_sha),
        Var::public("CI_COMMIT_REF_NAME", i.git_ref),
        Var::public("CI_COMMIT_REF_SLUG", slugify(i.git_ref)),
        Var::public("CI_COMMIT_REF_PROTECTED", i.protected.to_string()),
    ];
    // Pages, when the instance serves sites
    if let Some(url) = state.cfg.pages.url(&p.full_path) {
        v.push(Var::public("CI_PAGES_DOMAIN", state.cfg.pages.domain.clone()));
        v.push(Var::public("CI_PAGES_URL", url.trim_end_matches('/').to_string()));
    }
    if let Some(pl) = i.pipeline {
        v.push(Var::public("CI_PIPELINE_ID", pl.id.to_string()));
        v.push(Var::public("CI_PIPELINE_IID", pl.iid.to_string()));
        v.push(Var::public("CI_PIPELINE_URL", format!("{project_url}/-/pipelines/{}", pl.iid)));
        v.push(Var::public("CI_PIPELINE_CREATED_AT", rfc3339(pl.created_at)));
    }
    if i.tag {
        v.push(Var::public("CI_COMMIT_TAG", i.git_ref));
    } else if i.source != "merge_request_event" {
        v.push(Var::public("CI_COMMIT_BRANCH", i.git_ref));
    }
    if let Some(c) = i.commit {
        let title = c.subject.clone();
        let description = c.body.trim_start_matches('\n').to_string();
        let message = if description.is_empty() { title.clone() } else { format!("{title}\n\n{description}") };
        v.push(Var::public("CI_COMMIT_MESSAGE", message));
        v.push(Var::public("CI_COMMIT_TITLE", title));
        v.push(Var::public("CI_COMMIT_DESCRIPTION", description));
        v.push(Var::public("CI_COMMIT_AUTHOR", format!("{} <{}>", c.author.name, c.author.email)));
        if let Some(t) = chrono::DateTime::from_timestamp(c.committer.time, 0) {
            v.push(Var::public("CI_COMMIT_TIMESTAMP", t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)));
        }
    }
    if let Some(u) = i.user {
        v.push(Var::public("GITLAB_USER_ID", u.id.to_string()));
        v.push(Var::public("GITLAB_USER_LOGIN", u.username.clone()));
        v.push(Var::public("GITLAB_USER_NAME", if u.name.is_empty() { u.username.clone() } else { u.name.clone() }));
        v.push(Var::public("GITLAB_USER_EMAIL", u.email.clone()));
    }
    if let Some(m) = i.mr {
        v.push(Var::public("CI_MERGE_REQUEST_ID", m.id.to_string()));
        v.push(Var::public("CI_MERGE_REQUEST_IID", m.iid.to_string()));
        v.push(Var::public("CI_MERGE_REQUEST_PROJECT_ID", p.id.to_string()));
        v.push(Var::public("CI_MERGE_REQUEST_PROJECT_PATH", p.full_path.clone()));
        v.push(Var::public("CI_MERGE_REQUEST_PROJECT_URL", project_url.clone()));
        v.push(Var::public("CI_MERGE_REQUEST_REF_PATH", format!("refs/merge-requests/{}/head", m.iid)));
        v.push(Var::public("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME", m.source_branch.clone()));
        v.push(Var::public("CI_MERGE_REQUEST_SOURCE_BRANCH_SHA", i.sha));
        v.push(Var::public("CI_MERGE_REQUEST_SOURCE_PROJECT_ID", p.id.to_string()));
        v.push(Var::public("CI_MERGE_REQUEST_SOURCE_PROJECT_PATH", p.full_path.clone()));
        v.push(Var::public("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", m.target_branch.clone()));
        v.push(Var::public("CI_MERGE_REQUEST_TITLE", m.title.clone()));
        v.push(Var::public("CI_MERGE_REQUEST_DRAFT", m.draft.to_string()));
        v.push(Var::public("CI_MERGE_REQUEST_EVENT_TYPE", "detached"));
    }
    v
}

/// Variables that exist only once a job runs.
pub fn job_vars(state: &AppState, project: &Project, build: &Build, runner: &Runner, token: &str) -> Vec<Var> {
    let ext = state.cfg.server.external_url.as_str();
    let b = build;
    let mut v = vec![
        Var::public("CI_JOB_ID", b.id.to_string()),
        Var::public("CI_JOB_NAME", b.name.clone()),
        Var::public("CI_JOB_NAME_SLUG", slugify(&b.name)),
        Var::public("CI_JOB_STAGE", b.stage.clone()),
        Var::public("CI_JOB_URL", format!("{ext}/{}/-/jobs/{}", project.full_path, b.id)),
        Var::public("CI_JOB_TIMEOUT", b.timeout_secs.to_string()),
        Var::public("CI_JOB_STARTED_AT", rfc3339(b.started_at.unwrap_or_else(chrono::Utc::now))),
        Var::public("CI_JOB_MANUAL", (b.when_ == "manual").to_string()),
        Var::public("CI_NODE_TOTAL", "1"),
        Var::public("CI_RUNNER_ID", runner.id.to_string()),
        Var::public("CI_RUNNER_DESCRIPTION", runner.description.clone()),
        Var::public("CI_RUNNER_TAGS", serde_json::to_string(&runner.tags).unwrap_or_default()),
        Var { key: "CI_JOB_TOKEN".into(), value: token.into(), public: false, masked: true, file: false, raw: false },
        Var { key: "CI_BUILD_TOKEN".into(), value: token.into(), public: false, masked: true, file: false, raw: false },
        // carries the real job token, like GitLab's (the runner masks the token in logs)
        Var::secret("CI_REPOSITORY_URL", crate::ci::payload::repo_url_with_token(state, &project.full_path, token)),
    ];
    if state.cfg.registry.enabled {
        // the registry (ADR-0022): log in with the job token
        v.extend([
            Var::public("CI_REGISTRY", crate::registry::host(state)),
            Var::public("CI_REGISTRY_IMAGE", crate::registry::image_of(state, &project.full_path)),
            Var::public("CI_REGISTRY_USER", "gitlab-ci-token"),
            Var { key: "CI_REGISTRY_PASSWORD".into(), value: token.into(), public: false, masked: true, file: false, raw: false },
        ]);
    }
    v
}

pub struct Yaml<'a> {
    pub globals: &'a forgust_ci_config::IndexMap<String, String>,
    pub raw_globals: &'a [String],
    pub job: &'a Job,
}

/// Everything a job sees, lowest precedence first (GitLab's order): predefined,
/// YAML (top-level, then the job's), dotenv reports of its dependencies,
/// instance / group / project settings, then pipeline variables.
pub fn all(
    predefined: Vec<Var>,
    yaml: Yaml<'_>,
    dotenv: Vec<Var>,
    settings_vars: Vec<Var>,
    pipeline_vars: Vec<(String, String)>,
) -> Vec<Var> {
    let mut v = predefined;
    for (k, val) in yaml.globals {
        v.push(Var { raw: yaml.raw_globals.contains(k), ..Var::public(k, val.clone()) });
    }
    for (k, val) in &yaml.job.variables {
        v.push(Var { raw: yaml.job.raw_variables.contains(k), ..Var::public(k, val.clone()) });
    }
    // values from a job's output: never expanded again
    v.extend(dotenv.into_iter().map(|d| Var { raw: true, ..d }));
    v.extend(settings_vars);
    for (k, val) in pipeline_vars {
        v.push(Var::secret(&k, val));
    }
    v
}

/// `$VAR` and `${VAR}` replaced from `vars` (unknown ones become empty), as
/// GitLab expands environment names, URLs and resource groups.
pub fn expand(s: &str, vars: &std::collections::HashMap<String, String>) -> String {
    expand_with(s, vars, false)
}

/// [`expand`], leaving the variables `vars` does not know as written (for
/// values a later step expands again: variables passed downstream).
pub fn expand_known(s: &str, vars: &std::collections::HashMap<String, String>) -> String {
    expand_with(s, vars, true)
}

fn expand_with(s: &str, vars: &std::collections::HashMap<String, String>, keep_unknown: bool) -> String {
    if !s.contains('$') {
        return s.to_string();
    }
    let b: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '$' {
            let braced = b.get(i + 1) == Some(&'{');
            let start = if braced { i + 2 } else { i + 1 };
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == '_') {
                j += 1;
            }
            if j > start && (!braced || b.get(j) == Some(&'}')) {
                let name: String = b[start..j].iter().collect();
                match vars.get(&name) {
                    Some(v) => out.push_str(v),
                    None if keep_unknown => out.extend(&b[i..if braced { j + 1 } else { j }]),
                    None => {}
                }
                i = if braced { j + 1 } else { j };
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("feature/Add_Login"), "feature-add-login");
        assert_eq!(slugify("-x-"), "x");
        assert_eq!(slugify(&"a".repeat(80)).len(), 63);
    }

    #[test]
    fn expanding_only_what_is_known() {
        let vars = std::collections::HashMap::from([("A".to_string(), "1".to_string())]);
        assert_eq!(expand("$A-$B-${A}", &vars), "1--1");
        assert_eq!(expand_known("$A-$B-${A}-${C}-$", &vars), "1-$B-1-${C}-$");
    }
}
