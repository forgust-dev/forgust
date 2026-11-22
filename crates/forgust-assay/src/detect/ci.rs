//! The CI: its configuration files are one block. Its jobs say which blocks they build: those
//! holding the paths their `rules:changes` (GitLab, and so Forgust) or their workflow's
//! `paths` filter (GitHub, Forgejo, Gitea) name.

use serde_json::{Map, Value};

use super::{Ctx, Draft, Job, dir_of, file_name};
use crate::model::Kind;
use crate::yaml;

const FILES: &[&str] = &[
    ".gitlab-ci.yml",
    ".woodpecker.yml",
    "Jenkinsfile",
    "azure-pipelines.yml",
    ".drone.yml",
    "bitbucket-pipelines.yml",
    ".circleci/config.yml",
];
const DIRS: &[&str] = &[".gitlab/ci", ".github/workflows", ".forgejo/workflows", ".gitea/workflows", ".woodpecker"];
/// The folders of workflows whose `paths` filters are read (GitHub, Forgejo, Gitea).
const WORKFLOWS: &[&str] = &[".github/workflows", ".forgejo/workflows", ".gitea/workflows"];
/// The top-level keys of a .gitlab-ci.yml that are no jobs.
const GITLAB_GLOBALS: &[&str] = &[
    "image",
    "services",
    "stages",
    "types",
    "before_script",
    "after_script",
    "variables",
    "cache",
    "include",
    "default",
    "workflow",
    "spec",
];

/// Whether its text is read: the .gitlab-ci.yml, a workflow.
pub(super) fn reads(path: &str) -> bool {
    path == ".gitlab-ci.yml" || is_workflow(path)
}

fn is_workflow(path: &str) -> bool {
    WORKFLOWS.contains(&dir_of(path).as_str()) && (path.ends_with(".yml") || path.ends_with(".yaml"))
}

pub(super) fn detect(cx: &mut Ctx) {
    let mut roots: Vec<String> = FILES.iter().filter(|f| cx.exists(f)).map(|f| f.to_string()).collect();
    roots.extend(DIRS.iter().filter(|d| cx.any_at(d)).map(|d| d.to_string()));
    if roots.is_empty() {
        return;
    }
    let mut d = Draft::new("ci".to_string(), "CI", Kind::Ci);
    d.roots = roots.clone();
    d.manifests = roots;
    cx.add(d);
    if cx.exists(".gitlab-ci.yml") {
        gitlab(cx, ".gitlab-ci.yml");
    }
    let flows: Vec<&str> = cx.files.iter().map(String::as_str).filter(|f| is_workflow(f)).collect();
    for f in flows {
        workflow(cx, f);
    }
}

fn load(cx: &mut Ctx, path: &str) -> Option<Value> {
    let text = cx.read(path)?;
    match yaml::parse(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            cx.warn(path, format!("invalid YAML: {e}"));
            None
        }
    }
}

/// The jobs of a .gitlab-ci.yml (its includes are not read), with their `rules:changes` and
/// `only:changes` paths, those they get by `extends` included.
fn gitlab(cx: &mut Ctx, path: &str) {
    let Some(Value::Object(top)) = load(cx, path) else { return };
    for (name, job) in &top {
        if name.starts_with('.') || GITLAB_GLOBALS.contains(&name.as_str()) || !job.is_object() {
            continue;
        }
        let job = extended(&top, job, 0);
        let mut paths = Vec::new();
        for rule in job.get("rules").and_then(Value::as_array).into_iter().flatten() {
            match rule.get("changes") {
                Some(Value::Object(c)) => paths.extend(yaml::strings(c.get("paths"))),
                other => paths.extend(yaml::strings(other)),
            }
        }
        paths.extend(yaml::strings(job.get("only").and_then(|o| o.get("changes"))));
        cx.jobs.push(Job { name: name.clone(), paths });
    }
}

/// A job as GitLab runs it: what it `extends` (a job or a template of the file, by name, or a
/// list of them) merged in order under its own keys; hashes merge, the rest (a list of `rules`)
/// is replaced. GitLab follows 11 levels.
fn extended(top: &Map<String, Value>, job: &Value, depth: usize) -> Value {
    let mut out = Value::Object(Map::new());
    if depth < 11 {
        for parent in yaml::strings(job.get("extends")) {
            // one from an include is not read
            if let Some(p) = top.get(&parent).filter(|p| p.is_object()) {
                merge(&mut out, extended(top, p, depth + 1));
            }
        }
    }
    merge(&mut out, job.clone());
    out
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (k, v) in over {
                match base.get_mut(&k) {
                    Some(have) => merge(have, v),
                    None => {
                        base.insert(k, v);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

/// The jobs of a GitHub-style workflow, `workflow/job`, with its `push` and `pull_request`
/// paths filters.
fn workflow(cx: &mut Ctx, path: &str) {
    let Some(doc) = load(cx, path) else { return };
    let on = doc.get("on");
    let paths: Vec<String> = ["push", "pull_request", "pull_request_target"]
        .iter()
        .flat_map(|e| yaml::strings(on.and_then(|o| o.get(*e)).and_then(|e| e.get("paths"))))
        .collect();
    let stem = file_name(path).trim_end_matches(".yml").trim_end_matches(".yaml");
    for id in doc.get("jobs").and_then(Value::as_object).into_iter().flat_map(|j| j.keys()) {
        cx.jobs.push(Job { name: format!("{stem}/{id}"), paths: paths.clone() });
    }
}
