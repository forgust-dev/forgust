//! Job keywords: parsed from the composed YAML into normalized structs. Every
//! keyword is either supported, accepted without effect (listed), or rejected
//! with a clear error, so nothing is silently ignored.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::value::{Map, Value};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Image {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_policy: Vec<String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub variables: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_user: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Artifacts {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub untracked: bool,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub expire_in: Option<String>,
    /// `reports:` type → paths (uploaded as typed artifacts; see ci-compat.md).
    #[serde(default)]
    pub reports: IndexMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Cache {
    #[serde(default)]
    pub key: Option<String>,
    /// `key: {files: [...]}`; the runner computes the key from these files.
    #[serde(default)]
    pub key_files: Vec<String>,
    #[serde(default)]
    pub key_prefix: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub untracked: bool,
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub fallback_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Retry {
    pub max: u32,
    #[serde(default)]
    pub when: Vec<String>,
    #[serde(default)]
    pub exit_codes: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Need {
    pub job: String,
    #[serde(default = "yes")]
    pub artifacts: bool,
    #[serde(default)]
    pub optional: bool,
    /// `needs:parallel:matrix`: only these instances of a matrix job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matrix: Option<Vec<IndexMap<String, Vec<String>>>>,
    /// `needs:project` / `needs:pipeline`: the job is in another pipeline; only
    /// its artifacts come (no waiting for it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<NeedFrom>,
}

/// Where a need from outside this pipeline is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedFrom {
    /// The latest successful job of that name on `git_ref` of another project
    /// (or this one).
    Project { project: String, git_ref: String },
    /// A pipeline of this project, by id (usually `$PARENT_PIPELINE_ID`).
    Pipeline { pipeline: String },
}

/// `environment:`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Environment {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    /// start (default), prepare, stop, verify or access.
    #[serde(default = "start")]
    pub action: String,
    #[serde(default)]
    pub on_stop: Option<String>,
    #[serde(default)]
    pub auto_stop_in: Option<String>,
    #[serde(default)]
    pub deployment_tier: Option<String>,
}

fn start() -> String {
    "start".into()
}

/// `release:`: Forgust creates the release when the job succeeds.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub tag_message: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Where a new tag points (default: the pipeline's commit).
    #[serde(rename = "ref", default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub released_at: Option<String>,
    #[serde(default)]
    pub milestones: Vec<String>,
    #[serde(default)]
    pub links: Vec<ReleaseLink>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ReleaseLink {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub filepath: Option<String>,
    #[serde(default)]
    pub link_type: Option<String>,
}

/// Where a child pipeline's config comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildInclude {
    Local(String),
    /// A file another job of the pipeline made (`artifact`, `job`).
    Artifact {
        artifact: String,
        job: String,
    },
    Project {
        project: String,
        file: String,
        #[serde(default)]
        git_ref: Option<String>,
    },
}

/// `trigger:` (a bridge job: no runner, it starts a downstream pipeline).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Trigger {
    /// Another project (multi-project pipeline); None: a child pipeline.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub include: Vec<ChildInclude>,
    /// `strategy: depend`: the job mirrors the downstream pipeline's status.
    #[serde(default)]
    pub depend: bool,
    #[serde(default = "yes")]
    pub forward_yaml_variables: bool,
    #[serde(default)]
    pub forward_pipeline_variables: bool,
}

/// An `id_tokens` entry: an OIDC token for the job, for these audiences.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IdToken {
    pub aud: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Rule {
    #[serde(rename = "if", default)]
    pub if_: Option<String>,
    #[serde(default)]
    pub changes: Option<Vec<String>>,
    /// `rules:changes:compare_to`: diff against this ref instead of the push's base.
    #[serde(default)]
    pub compare_to: Option<String>,
    #[serde(default)]
    pub exists: Option<Vec<String>>,
    /// `rules:exists:project` (and `ref`): look in another project.
    #[serde(default)]
    pub exists_project: Option<String>,
    #[serde(default)]
    pub exists_ref: Option<String>,
    /// `rules:start_in` with `when: delayed`.
    #[serde(default)]
    pub start_in: Option<String>,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub allow_failure: Option<bool>,
    #[serde(default)]
    pub variables: IndexMap<String, String>,
}

/// `only:` / `except:` (legacy, still common).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Filter {
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub variables: Vec<String>,
    #[serde(default)]
    pub changes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parallel {
    Count(u32),
    /// Each entry: variable → values (a scalar is a one-value list).
    Matrix(Vec<IndexMap<String, Vec<String>>>),
}

/// A job as the runner needs it, plus the keywords the planner evaluates.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Job {
    pub stage: String,
    #[serde(default)]
    pub image: Option<Image>,
    #[serde(default)]
    pub services: Vec<Image>,
    #[serde(default)]
    pub before_script: Vec<String>,
    #[serde(default)]
    pub script: Vec<String>,
    #[serde(default)]
    pub after_script: Vec<String>,
    /// Job variables (after `inherit`); global ones are kept separately.
    #[serde(default)]
    pub variables: IndexMap<String, String>,
    /// Variables with `expand: false` (sent as raw).
    #[serde(default)]
    pub raw_variables: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "on_success")]
    pub when: String,
    #[serde(default)]
    pub allow_failure: Option<bool>,
    #[serde(default)]
    pub allow_failure_exit_codes: Vec<i32>,
    #[serde(default)]
    pub needs: Option<Vec<Need>>,
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
    #[serde(default)]
    pub artifacts: Option<Artifacts>,
    #[serde(default)]
    pub cache: Vec<Cache>,
    #[serde(default)]
    pub retry: Option<Retry>,
    #[serde(default)]
    pub timeout: Option<String>,
    #[serde(default)]
    pub interruptible: bool,
    #[serde(default)]
    pub coverage: Option<String>,
    #[serde(default)]
    pub rules: Option<Vec<Rule>>,
    #[serde(default)]
    pub only: Option<Filter>,
    #[serde(default)]
    pub except: Option<Filter>,
    #[serde(default)]
    pub parallel: Option<Parallel>,
    /// Which global variables this job inherits: None = all.
    #[serde(default)]
    pub inherit_variables: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Environment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_group: Option<String>,
    /// `when: delayed`: how long after it is ready the job starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_in: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<Release>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub id_tokens: IndexMap<String, IdToken>,
    /// Asked before someone runs this manual job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_confirmation: Option<String>,
    /// A Pages job: when it passes, the `publish` folder of its artifacts
    /// becomes the project's site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<Pages>,
}

/// `pages:` (or a job named `pages`): what is published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pages {
    /// The folder of the artifacts served as the site (`public` by default).
    pub publish: String,
}

fn on_success() -> String {
    "on_success".into()
}

// ---------------------------------------------------------------- keywords

pub const JOB_KEYWORDS: &[&str] = &[
    "pages",
    "publish",
    "stage",
    "image",
    "services",
    "script",
    "before_script",
    "after_script",
    "variables",
    "tags",
    "when",
    "allow_failure",
    "needs",
    "dependencies",
    "artifacts",
    "cache",
    "retry",
    "timeout",
    "interruptible",
    "coverage",
    "rules",
    "only",
    "except",
    "parallel",
    "inherit",
    "extends",
    "environment",
    "resource_group",
    "start_in",
    "release",
    "trigger",
    "id_tokens",
    "manual_confirmation",
];
/// Accepted and ignored (they change nothing about how Forgust runs the job).
pub const JOB_IGNORED: &[&str] = &["description"];
/// Keywords a `trigger` job may have (it runs no script).
const BRIDGE_KEYWORDS: &[&str] = &[
    "stage",
    "trigger",
    "variables",
    "rules",
    "only",
    "except",
    "needs",
    "when",
    "allow_failure",
    "extends",
    "inherit",
    "resource_group",
    "environment",
    "description",
    "dependencies",
    "parallel",
];
pub const JOB_UNSUPPORTED: &[(&str, &str)] = &[
    ("secrets", "external secrets (Vault and others) are not supported yet"),
    ("dast_configuration", "DAST profiles are not supported"),
    ("identity", "identity federation is not supported"),
    ("hooks", "job hooks are not supported yet"),
    ("run", "CI/CD steps (`run`) are not supported yet"),
    ("inputs", "job inputs are not supported yet"),
];
pub const DEFAULT_KEYWORDS: &[&str] =
    &["after_script", "artifacts", "before_script", "cache", "id_tokens", "image", "interruptible", "retry", "services", "tags", "timeout"];

fn err(job: &str, msg: impl std::fmt::Display) -> ConfigError {
    ConfigError::Invalid(format!("job `{job}`: {msg}"))
}

fn string(job: &str, key: &str, v: &Value) -> Result<String, ConfigError> {
    v.scalar_string().filter(|_| !matches!(v, Value::Null)).ok_or_else(|| err(job, format!("`{key}` must be a string")))
}

fn opt_string(job: &str, key: &str, v: Option<&Value>) -> Result<Option<String>, ConfigError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => string(job, key, v).map(Some),
    }
}

/// A string or a list of strings.
fn strings(job: &str, key: &str, v: &Value) -> Result<Vec<String>, ConfigError> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Seq(items) => items.iter().map(|i| string(job, key, i)).collect(),
        other => Ok(vec![string(job, key, other)?]),
    }
}

/// Scripts: a string, or arrays nested up to 10 levels (from `!reference`), flattened.
pub fn script(job: &str, key: &str, v: &Value) -> Result<Vec<String>, ConfigError> {
    fn walk(job: &str, key: &str, v: &Value, depth: usize, out: &mut Vec<String>) -> Result<(), ConfigError> {
        if depth > 10 {
            return Err(err(job, format!("`{key}` nests arrays more than 10 levels deep")));
        }
        match v {
            Value::Seq(items) => {
                for i in items {
                    walk(job, key, i, depth + 1, out)?;
                }
                Ok(())
            }
            Value::Map(_) => Err(err(job, format!("`{key}` must be a string or an array of strings"))),
            Value::Ref(r) => Err(err(job, format!("!reference {r:?} was not resolved"))),
            Value::Null => Ok(()),
            s => {
                out.push(s.scalar_string().unwrap_or_default());
                Ok(())
            }
        }
    }
    let mut out = Vec::new();
    walk(job, key, v, 0, &mut out)?;
    Ok(out)
}

/// `variables:` entries: scalars, or `{value, description, expand, options}`.
pub fn variables(owner: &str, v: &Value) -> Result<(IndexMap<String, String>, Vec<String>), ConfigError> {
    let mut out = IndexMap::new();
    let mut raw = Vec::new();
    let Some(m) = v.as_map() else {
        if matches!(v, Value::Null) {
            return Ok((out, raw));
        }
        return Err(ConfigError::Invalid(format!("{owner}: `variables` must be a hash")));
    };
    for (k, val) in m {
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(ConfigError::Invalid(format!("{owner}: invalid variable name `{k}`")));
        }
        let value = match val {
            Value::Map(o) => {
                if o.get("expand") == Some(&Value::Bool(false)) {
                    raw.push(k.clone());
                }
                o.get("value").and_then(Value::scalar_string).unwrap_or_default()
            }
            other => other.scalar_string().ok_or_else(|| ConfigError::Invalid(format!("{owner}: variable `{k}` must be a string")))?,
        };
        out.insert(k.clone(), value);
    }
    Ok((out, raw))
}

pub fn image(job: &str, key: &str, v: &Value) -> Result<Image, ConfigError> {
    match v {
        Value::Map(m) => {
            for k in m.keys() {
                if !["name", "entrypoint", "pull_policy", "docker", "alias", "command", "variables", "kubernetes"].contains(&k.as_str()) {
                    return Err(err(job, format!("`{key}:{k}` is not supported")));
                }
            }
            let name = opt_string(job, key, m.get("name"))?.ok_or_else(|| err(job, format!("`{key}:name` is required")))?;
            let docker = m.get("docker").and_then(Value::as_map);
            Ok(Image {
                name,
                alias: opt_string(job, "alias", m.get("alias"))?,
                entrypoint: m.get("entrypoint").map(|e| strings(job, "entrypoint", e)).transpose()?,
                command: m.get("command").map(|e| strings(job, "command", e)).transpose()?,
                pull_policy: m.get("pull_policy").map(|e| strings(job, "pull_policy", e)).transpose()?.unwrap_or_default(),
                variables: match m.get("variables") {
                    Some(v) => variables(&format!("job `{job}` service"), v)?.0,
                    None => IndexMap::new(),
                },
                docker_platform: opt_string(job, "docker:platform", docker.and_then(|d| d.get("platform")))?,
                docker_user: opt_string(job, "docker:user", docker.and_then(|d| d.get("user")))?,
            })
        }
        other => Ok(Image { name: string(job, key, other)?, ..Default::default() }),
    }
}

pub fn artifacts(job: &str, v: &Value) -> Result<Artifacts, ConfigError> {
    let m = v.as_map().ok_or_else(|| err(job, "`artifacts` must be a hash"))?;
    let mut a = Artifacts::default();
    for (k, val) in m {
        match k.as_str() {
            "paths" => a.paths = strings(job, "artifacts:paths", val)?,
            "exclude" => a.exclude = strings(job, "artifacts:exclude", val)?,
            "name" => a.name = opt_string(job, "artifacts:name", Some(val))?,
            "untracked" => a.untracked = matches!(val, Value::Bool(true)),
            "when" => {
                let w = string(job, "artifacts:when", val)?;
                if !["on_success", "on_failure", "always"].contains(&w.as_str()) {
                    return Err(err(job, format!("`artifacts:when: {w}` must be on_success, on_failure or always")));
                }
                a.when = Some(w);
            }
            "expire_in" => a.expire_in = opt_string(job, "artifacts:expire_in", Some(val))?,
            "expose_as" | "public" | "access" => {}
            "reports" => {
                let r = val.as_map().ok_or_else(|| err(job, "`artifacts:reports` must be a hash"))?;
                for (rk, rv) in r {
                    let paths = match rv {
                        Value::Map(cm) if rk == "coverage_report" => {
                            vec![opt_string(job, "coverage_report:path", cm.get("path"))?.unwrap_or_default()]
                        }
                        other => strings(job, &format!("artifacts:reports:{rk}"), other)?,
                    };
                    a.reports.insert(rk.clone(), paths);
                }
            }
            other => return Err(err(job, format!("`artifacts:{other}` is not supported"))),
        }
    }
    Ok(a)
}

pub fn caches(job: &str, v: &Value) -> Result<Vec<Cache>, ConfigError> {
    let items: Vec<&Value> = match v {
        Value::Seq(s) => s.iter().collect(),
        Value::Null => return Ok(Vec::new()),
        other => vec![other],
    };
    let mut out = Vec::new();
    for item in items {
        let m = item.as_map().ok_or_else(|| err(job, "`cache` must be a hash or a list of hashes"))?;
        let mut c = Cache::default();
        for (k, val) in m {
            match k.as_str() {
                "key" => match val {
                    Value::Map(km) => {
                        c.key_files = km.get("files").map(|f| strings(job, "cache:key:files", f)).transpose()?.unwrap_or_default();
                        c.key_prefix = opt_string(job, "cache:key:prefix", km.get("prefix"))?;
                    }
                    other => c.key = Some(string(job, "cache:key", other)?),
                },
                "paths" => c.paths = strings(job, "cache:paths", val)?,
                "untracked" => c.untracked = matches!(val, Value::Bool(true)),
                "policy" => c.policy = opt_string(job, "cache:policy", Some(val))?,
                "when" => c.when = opt_string(job, "cache:when", Some(val))?,
                "fallback_keys" => c.fallback_keys = strings(job, "cache:fallback_keys", val)?,
                "unprotect" => {}
                other => return Err(err(job, format!("`cache:{other}` is not supported"))),
            }
        }
        out.push(c);
    }
    Ok(out)
}

pub fn retry(job: &str, v: &Value) -> Result<Retry, ConfigError> {
    let r = match v {
        Value::Int(n) => Retry { max: *n as u32, ..Default::default() },
        Value::Map(m) => Retry {
            max: match m.get("max") {
                Some(Value::Int(n)) => *n as u32,
                None => 0,
                _ => return Err(err(job, "`retry:max` must be a number")),
            },
            when: m.get("when").map(|w| strings(job, "retry:when", w)).transpose()?.unwrap_or_default(),
            exit_codes: match m.get("exit_codes") {
                None => Vec::new(),
                Some(Value::Int(n)) => vec![*n as i32],
                Some(Value::Seq(s)) => s
                    .iter()
                    .map(|x| if let Value::Int(n) = x { Ok(*n as i32) } else { Err(err(job, "exit codes are numbers")) })
                    .collect::<Result<_, _>>()?,
                _ => return Err(err(job, "`retry:exit_codes` must be numbers")),
            },
        },
        _ => return Err(err(job, "`retry` must be a number or a hash")),
    };
    if r.max > 2 {
        return Err(err(job, "`retry:max` is at most 2"));
    }
    Ok(r)
}

pub fn needs(job: &str, v: &Value) -> Result<Vec<Need>, ConfigError> {
    let Value::Seq(items) = v else { return Err(err(job, "`needs` must be an array")) };
    items
        .iter()
        .map(|i| match i {
            Value::Map(m) => {
                for k in m.keys() {
                    match k.as_str() {
                        "job" | "artifacts" | "optional" | "parallel" | "pipeline" | "project" | "ref" => {}
                        other => return Err(err(job, format!("`needs:{other}` is not supported"))),
                    }
                }
                let from = match (m.get("project"), m.get("pipeline")) {
                    (Some(_), Some(_)) => return Err(err(job, "a `needs` entry takes `project` or `pipeline`, not both")),
                    (Some(p), None) => Some(NeedFrom::Project {
                        project: string(job, "needs:project", p)?,
                        git_ref: opt_string(job, "needs:ref", m.get("ref"))?.ok_or_else(|| err(job, "`needs:project` needs a `ref`"))?,
                    }),
                    (None, Some(p)) => Some(NeedFrom::Pipeline { pipeline: string(job, "needs:pipeline", p)? }),
                    (None, None) => {
                        if m.contains_key("ref") {
                            return Err(err(job, "`needs:ref` goes with `needs:project`"));
                        }
                        None
                    }
                };
                if from.is_some() && m.contains_key("parallel") {
                    return Err(err(job, "`needs:parallel` is for jobs of this pipeline"));
                }
                let matrix = match m.get("parallel") {
                    None => None,
                    Some(p) => {
                        let rows = p
                            .as_map()
                            .and_then(|pm| pm.get("matrix"))
                            .and_then(|x| if let Value::Seq(r) = x { Some(r) } else { None })
                            .ok_or_else(|| err(job, "`needs:parallel` takes a `matrix`"))?;
                        let mut out = Vec::new();
                        for row in rows {
                            let rm = row.as_map().ok_or_else(|| err(job, "each `needs:parallel:matrix` entry must be a hash"))?;
                            let mut entry = IndexMap::new();
                            for (k, v) in rm {
                                entry.insert(k.clone(), strings(job, "needs:parallel:matrix", v)?);
                            }
                            out.push(entry);
                        }
                        Some(out)
                    }
                };
                Ok(Need {
                    job: opt_string(job, "needs:job", m.get("job"))?.ok_or_else(|| err(job, "`needs` entries need a `job`"))?,
                    artifacts: !matches!(m.get("artifacts"), Some(Value::Bool(false))),
                    optional: matches!(m.get("optional"), Some(Value::Bool(true))),
                    matrix,
                    from,
                })
            }
            other => Ok(Need { job: string(job, "needs", other)?, artifacts: true, optional: false, matrix: None, from: None }),
        })
        .collect()
}

/// `pages: true | false | {publish}` and the older job-level `publish:`; a job
/// named `pages` publishes unless `pages: false`.
fn pages(job: &str, m: &IndexMap<String, Value>) -> Result<Option<Pages>, ConfigError> {
    let legacy = opt_string(job, "publish", m.get("publish"))?;
    let (on, publish) = match m.get("pages") {
        None => (job == "pages", None),
        Some(Value::Bool(b)) => (*b, None),
        Some(Value::Map(pm)) => {
            for k in pm.keys() {
                match k.as_str() {
                    "publish" => {}
                    "path_prefix" => return Err(err(job, "`pages:path_prefix` (parallel deployments) is not supported")),
                    "expire_in" => return Err(err(job, "`pages:expire_in` is not supported: a site stays until replaced or removed")),
                    other => return Err(err(job, format!("`pages:{other}` is not supported"))),
                }
            }
            (true, opt_string(job, "pages:publish", pm.get("publish"))?)
        }
        Some(_) => return Err(err(job, "`pages` must be true, false or a hash")),
    };
    if !on {
        if legacy.is_some() {
            return Err(err(job, "`publish` only applies to a Pages job"));
        }
        return Ok(None);
    }
    let publish = publish.or(legacy).unwrap_or_else(|| "public".into());
    let publish = publish.trim_matches('/').to_string();
    if publish.is_empty() || publish.split('/').any(|s| s == ".." || s == "." || s.is_empty()) || publish.contains('$') {
        return Err(err(job, "`pages:publish` must be a folder of the job's artifacts (no `..`, no variables)"));
    }
    Ok(Some(Pages { publish }))
}

fn paths_list(job: &str, key: &str, v: &Value) -> Result<Vec<String>, ConfigError> {
    match v {
        Value::Map(m) => {
            if m.contains_key("compare_to") || m.contains_key("project") {
                return Err(err(job, format!("`{key}` with compare_to or project only works in `rules`")));
            }
            strings(job, key, m.get("paths").unwrap_or(&Value::Null))
        }
        other => strings(job, key, other),
    }
}

/// `rules:changes` / `rules:exists`: paths, plus `compare_to` (changes) or
/// `project` and `ref` (exists).
fn rule_paths(owner: &str, key: &str, v: &Value, extra: &[&str]) -> Result<(Vec<String>, IndexMap<String, String>), ConfigError> {
    match v {
        Value::Map(m) => {
            let mut more = IndexMap::new();
            for (k, val) in m {
                match k.as_str() {
                    "paths" => {}
                    other if extra.contains(&other) => {
                        more.insert(other.to_string(), string(owner, &format!("{key}:{other}"), val)?);
                    }
                    other => return Err(ConfigError::Invalid(format!("{owner}: unknown `{key}:{other}`"))),
                }
            }
            Ok((strings(owner, key, m.get("paths").unwrap_or(&Value::Null))?, more))
        }
        other => Ok((strings(owner, key, other)?, IndexMap::new())),
    }
}

pub fn rules(owner: &str, v: &Value, allowed_when: &[&str]) -> Result<Vec<Rule>, ConfigError> {
    let Value::Seq(items) = v else { return Err(ConfigError::Invalid(format!("{owner}: `rules` must be an array"))) };
    let mut out = Vec::new();
    for item in items {
        let m = item.as_map().ok_or_else(|| ConfigError::Invalid(format!("{owner}: each rule must be a hash")))?;
        let mut r = Rule::default();
        for (k, val) in m {
            match k.as_str() {
                "if" => {
                    let s = string(owner, "rules:if", val)?;
                    crate::expr::Expression::parse(&s)?;
                    r.if_ = Some(s);
                }
                "changes" => {
                    let (paths, more) = rule_paths(owner, "rules:changes", val, &["compare_to"])?;
                    r.changes = Some(paths);
                    r.compare_to = more.get("compare_to").cloned();
                }
                "exists" => {
                    let (paths, more) = rule_paths(owner, "rules:exists", val, &["project", "ref"])?;
                    r.exists = Some(paths);
                    r.exists_project = more.get("project").map(|p| p.trim_matches('/').to_string());
                    r.exists_ref = more.get("ref").cloned();
                    if r.exists_ref.is_some() && r.exists_project.is_none() {
                        return Err(ConfigError::Invalid(format!("{owner}: `rules:exists:ref` needs `project`")));
                    }
                }
                "when" => {
                    let w = string(owner, "rules:when", val)?;
                    if !allowed_when.contains(&w.as_str()) {
                        return Err(ConfigError::Invalid(format!("{owner}: `rules:when: {w}` is not supported here")));
                    }
                    r.when = Some(w);
                }
                "allow_failure" => r.allow_failure = Some(matches!(val, Value::Bool(true))),
                "variables" => r.variables = variables(owner, val)?.0,
                "needs" => return Err(ConfigError::Invalid(format!("{owner}: `rules:needs` is not supported yet"))),
                "start_in" => r.start_in = Some(string(owner, "rules:start_in", val)?),
                other => return Err(ConfigError::Invalid(format!("{owner}: `rules:{other}` is not supported"))),
            }
        }
        if r.when.as_deref() == Some("delayed") && r.start_in.is_none() {
            return Err(ConfigError::Invalid(format!("{owner}: `rules:when: delayed` needs `start_in`")));
        }
        if r.start_in.is_some() && r.when.as_deref() != Some("delayed") {
            return Err(ConfigError::Invalid(format!("{owner}: `rules:start_in` goes with `when: delayed`")));
        }
        out.push(r);
    }
    Ok(out)
}

pub fn filter(job: &str, key: &str, v: &Value) -> Result<Filter, ConfigError> {
    match v {
        Value::Map(m) => {
            let mut f = Filter::default();
            for (k, val) in m {
                match k.as_str() {
                    "refs" => f.refs = strings(job, &format!("{key}:refs"), val)?,
                    "variables" => {
                        f.variables = strings(job, &format!("{key}:variables"), val)?;
                        for e in &f.variables {
                            crate::expr::Expression::parse(e)?;
                        }
                    }
                    "changes" => f.changes = paths_list(job, &format!("{key}:changes"), val)?,
                    "kubernetes" => return Err(err(job, format!("`{key}:kubernetes` is not supported"))),
                    other => return Err(err(job, format!("`{key}:{other}` is not supported"))),
                }
            }
            Ok(f)
        }
        other => Ok(Filter { refs: strings(job, key, other)?, ..Default::default() }),
    }
}

pub fn parallel(job: &str, v: &Value) -> Result<Parallel, ConfigError> {
    match v {
        Value::Int(n) if (2..=200).contains(n) => Ok(Parallel::Count(*n as u32)),
        Value::Int(_) => Err(err(job, "`parallel` must be between 2 and 200")),
        Value::Map(m) => {
            let Some(Value::Seq(rows)) = m.get("matrix") else { return Err(err(job, "`parallel` takes a number or `matrix`")) };
            let mut out = Vec::new();
            // the jobs it makes, counted before anything is expanded (GitLab's limit is 200 too)
            let mut total = 0usize;
            for row in rows {
                let rm = row.as_map().ok_or_else(|| err(job, "each `parallel:matrix` entry must be a hash"))?;
                let mut entry = IndexMap::new();
                let mut combos = 1usize;
                for (k, val) in rm {
                    let values = strings(job, "parallel:matrix", val)?;
                    combos = combos.saturating_mul(values.len());
                    entry.insert(k.clone(), values);
                }
                total = total.saturating_add(combos);
                if total > 200 {
                    return Err(err(job, "`parallel:matrix` makes more than 200 jobs"));
                }
                out.push(entry);
            }
            Ok(Parallel::Matrix(out))
        }
        _ => Err(err(job, "`parallel` takes a number or `matrix`")),
    }
}

/// `inherit:default` / `inherit:variables`: true, false or a list of names.
pub fn inherit_list(job: &str, key: &str, v: Option<&Value>) -> Result<Option<Vec<String>>, ConfigError> {
    match v {
        None | Some(Value::Bool(true)) => Ok(None),
        Some(Value::Bool(false)) => Ok(Some(Vec::new())),
        Some(other) => strings(job, key, other).map(Some),
    }
}

const ENV_ACTIONS: &[&str] = &["start", "prepare", "stop", "verify", "access"];
const ENV_TIERS: &[&str] = &["production", "staging", "testing", "development", "other"];

pub fn environment(job: &str, v: &Value) -> Result<Environment, ConfigError> {
    let e = match v {
        Value::Map(m) => {
            let mut e = Environment { action: "start".into(), ..Default::default() };
            for (k, val) in m {
                match k.as_str() {
                    "name" => e.name = string(job, "environment:name", val)?,
                    "url" => e.url = Some(string(job, "environment:url", val)?),
                    "action" => e.action = string(job, "environment:action", val)?,
                    "on_stop" => e.on_stop = Some(string(job, "environment:on_stop", val)?),
                    "auto_stop_in" => e.auto_stop_in = Some(string(job, "environment:auto_stop_in", val)?),
                    "deployment_tier" => e.deployment_tier = Some(string(job, "environment:deployment_tier", val)?),
                    "kubernetes" => return Err(err(job, "`environment:kubernetes` is not supported")),
                    other => return Err(err(job, format!("unknown `environment:{other}`"))),
                }
            }
            e
        }
        other => Environment { name: string(job, "environment", other)?, action: "start".into(), ..Default::default() },
    };
    if e.name.trim().is_empty() || e.name.len() > 255 {
        return Err(err(job, "`environment:name` must be 1 to 255 characters"));
    }
    if !ENV_ACTIONS.contains(&e.action.as_str()) {
        return Err(err(job, "`environment:action` must be start, prepare, stop, verify or access"));
    }
    if let Some(t) = &e.deployment_tier
        && !ENV_TIERS.contains(&t.as_str())
    {
        return Err(err(job, "`environment:deployment_tier` must be production, staging, testing, development or other"));
    }
    Ok(e)
}

pub fn trigger(job: &str, v: &Value) -> Result<Trigger, ConfigError> {
    let mut t = Trigger { forward_yaml_variables: true, ..Default::default() };
    match v {
        Value::Map(m) => {
            for (k, val) in m {
                match k.as_str() {
                    "project" => t.project = Some(string(job, "trigger:project", val)?.trim_matches('/').to_string()),
                    "branch" => t.branch = Some(string(job, "trigger:branch", val)?),
                    "strategy" => {
                        let st = string(job, "trigger:strategy", val)?;
                        if st != "depend" {
                            return Err(err(job, "`trigger:strategy` can only be `depend`"));
                        }
                        t.depend = true;
                    }
                    "include" => t.include = child_includes(job, val)?,
                    "forward" => {
                        let fm = val.as_map().ok_or_else(|| err(job, "`trigger:forward` must be a hash"))?;
                        for (fk, fv) in fm {
                            let b = matches!(fv, Value::Bool(true));
                            match fk.as_str() {
                                "yaml_variables" => t.forward_yaml_variables = b,
                                "pipeline_variables" => t.forward_pipeline_variables = b,
                                other => return Err(err(job, format!("unknown `trigger:forward:{other}`"))),
                            }
                        }
                    }
                    "inputs" => return Err(err(job, "`trigger:inputs` is not supported yet")),
                    other => return Err(err(job, format!("unknown `trigger:{other}`"))),
                }
            }
        }
        // `trigger: group/project` is a multi-project pipeline
        other => t.project = Some(string(job, "trigger", other)?.trim_matches('/').to_string()),
    }
    match (&t.project, t.include.is_empty()) {
        (Some(_), false) => Err(err(job, "`trigger` takes `project` or `include`, not both")),
        (None, true) => Err(err(job, "`trigger` needs `project` or `include`")),
        _ => Ok(t),
    }
}

fn child_includes(job: &str, v: &Value) -> Result<Vec<ChildInclude>, ConfigError> {
    let items: Vec<&Value> = match v {
        Value::Seq(s) => s.iter().collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for i in items {
        out.push(match i {
            Value::Map(m) => {
                if let Some(l) = m.get("local") {
                    ChildInclude::Local(string(job, "trigger:include:local", l)?)
                } else if let Some(a) = m.get("artifact") {
                    ChildInclude::Artifact {
                        artifact: string(job, "trigger:include:artifact", a)?,
                        job: opt_string(job, "trigger:include:job", m.get("job"))?
                            .ok_or_else(|| err(job, "`trigger:include:artifact` needs `job`"))?,
                    }
                } else if let Some(p) = m.get("project") {
                    ChildInclude::Project {
                        project: string(job, "trigger:include:project", p)?.trim_matches('/').to_string(),
                        file: opt_string(job, "trigger:include:file", m.get("file"))?
                            .ok_or_else(|| err(job, "`trigger:include:project` needs `file`"))?,
                        git_ref: opt_string(job, "trigger:include:ref", m.get("ref"))?,
                    }
                } else if m.contains_key("template") {
                    return Err(err(job, "`trigger:include:template`: GitLab's templates are not bundled"));
                } else {
                    return Err(err(job, "`trigger:include` entries need local, artifact or project"));
                }
            }
            other => ChildInclude::Local(string(job, "trigger:include", other)?),
        });
    }
    if out.len() > 3 {
        return Err(err(job, "`trigger:include` takes at most 3 files"));
    }
    Ok(out)
}

pub fn release(job: &str, v: &Value) -> Result<Release, ConfigError> {
    let m = v.as_map().ok_or_else(|| err(job, "`release` must be a hash"))?;
    let mut r = Release::default();
    for (k, val) in m {
        match k.as_str() {
            "tag_name" => r.tag_name = string(job, "release:tag_name", val)?,
            "tag_message" => r.tag_message = Some(string(job, "release:tag_message", val)?),
            "name" => r.name = Some(string(job, "release:name", val)?),
            "description" => r.description = Some(string(job, "release:description", val)?),
            "ref" => r.git_ref = Some(string(job, "release:ref", val)?),
            "released_at" => r.released_at = Some(string(job, "release:released_at", val)?),
            "milestones" => r.milestones = strings(job, "release:milestones", val)?,
            "assets" => {
                let links = val.as_map().and_then(|a| a.get("links")).and_then(|l| if let Value::Seq(s) = l { Some(s) } else { None });
                for l in links.ok_or_else(|| err(job, "`release:assets` takes `links`"))? {
                    let lm = l.as_map().ok_or_else(|| err(job, "each `release:assets:links` entry must be a hash"))?;
                    r.links.push(ReleaseLink {
                        name: opt_string(job, "release:assets:links:name", lm.get("name"))?
                            .ok_or_else(|| err(job, "release links need a `name`"))?,
                        url: opt_string(job, "release:assets:links:url", lm.get("url"))?
                            .ok_or_else(|| err(job, "release links need a `url`"))?,
                        filepath: opt_string(job, "release:assets:links:filepath", lm.get("filepath").or(lm.get("direct_asset_path")))?,
                        link_type: opt_string(job, "release:assets:links:link_type", lm.get("link_type"))?,
                    });
                }
            }
            other => return Err(err(job, format!("unknown `release:{other}`"))),
        }
    }
    if r.tag_name.trim().is_empty() {
        return Err(err(job, "`release:tag_name` is required"));
    }
    Ok(r)
}

pub fn id_tokens(job: &str, v: &Value) -> Result<IndexMap<String, IdToken>, ConfigError> {
    let m = v.as_map().ok_or_else(|| err(job, "`id_tokens` must be a hash"))?;
    let mut out = IndexMap::new();
    for (name, t) in m {
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || name.is_empty() {
            return Err(err(job, format!("`id_tokens:{name}`: names are letters, digits and underscores")));
        }
        let tm = t.as_map().ok_or_else(|| err(job, format!("`id_tokens:{name}` must be a hash with `aud`")))?;
        for k in tm.keys() {
            if k != "aud" {
                return Err(err(job, format!("unknown `id_tokens:{name}:{k}`")));
            }
        }
        let aud = strings(job, &format!("id_tokens:{name}:aud"), tm.get("aud").unwrap_or(&Value::Null))?;
        if aud.is_empty() {
            return Err(err(job, format!("`id_tokens:{name}` needs `aud`")));
        }
        out.insert(name.clone(), IdToken { aud });
    }
    Ok(out)
}

/// Build a job from its composed hash. `defaults` is the `default:` hash.
pub fn build(name: &str, m: &Map, defaults: &Map) -> Result<Job, ConfigError> {
    for k in m.keys() {
        if let Some((_, why)) = JOB_UNSUPPORTED.iter().find(|(u, _)| u == k) {
            return Err(err(name, format!("`{k}`: {why}")));
        }
        if !JOB_KEYWORDS.contains(&k.as_str()) && !JOB_IGNORED.contains(&k.as_str()) {
            return Err(err(name, format!("unknown keyword `{k}`")));
        }
    }
    if m.contains_key("trigger")
        && let Some(k) = m.keys().find(|k| !BRIDGE_KEYWORDS.contains(&k.as_str()))
    {
        return Err(err(name, format!("a `trigger` job runs no script: `{k}` does not apply")));
    }
    let inherit = m.get("inherit").and_then(Value::as_map);
    if let Some(i) = inherit
        && let Some(k) = i.keys().find(|k| *k != "default" && *k != "variables")
    {
        return Err(err(name, format!("`inherit:{k}` is not supported")));
    }
    let inherit_default = inherit_list(name, "inherit:default", inherit.and_then(|i| i.get("default")))?;
    let inherit_variables = inherit_list(name, "inherit:variables", inherit.and_then(|i| i.get("variables")))?;
    // job keys, else inherited default keys
    let get = |k: &str| -> Option<&Value> {
        m.get(k).or_else(|| match &inherit_default {
            None => defaults.get(k),
            Some(list) if list.iter().any(|x| x == k) => defaults.get(k),
            Some(_) => None,
        })
    };

    let mut job =
        Job { stage: m.get("stage").map(|s| string(name, "stage", s)).transpose()?.unwrap_or_else(|| "test".into()), ..Default::default() };
    job.script = m.get("script").map(|v| script(name, "script", v)).transpose()?.unwrap_or_default();
    job.before_script = get("before_script").map(|v| script(name, "before_script", v)).transpose()?.unwrap_or_default();
    job.after_script = get("after_script").map(|v| script(name, "after_script", v)).transpose()?.unwrap_or_default();
    job.image = get("image").filter(|v| !matches!(v, Value::Null)).map(|v| image(name, "image", v)).transpose()?;
    job.services = match get("services") {
        Some(Value::Seq(s)) => s.iter().map(|v| image(name, "services", v)).collect::<Result<_, _>>()?,
        Some(Value::Null) | None => Vec::new(),
        Some(_) => return Err(err(name, "`services` must be an array")),
    };
    if let Some(v) = m.get("variables") {
        let (vars, raw) = variables(&format!("job `{name}`"), v)?;
        job.variables = vars;
        job.raw_variables = raw;
    }
    job.inherit_variables = inherit_variables;
    job.tags = get("tags").map(|v| strings(name, "tags", v)).transpose()?.unwrap_or_default();
    if let Some(w) = m.get("when") {
        let w = string(name, "when", w)?;
        match w.as_str() {
            "on_success" | "on_failure" | "always" | "manual" | "delayed" => job.when = w,
            "never" => return Err(err(name, "`when: never` belongs in `rules`")),
            other => return Err(err(name, format!("`when: {other}` is not valid"))),
        }
    } else {
        job.when = "on_success".into();
    }
    match m.get("allow_failure") {
        None => {}
        Some(Value::Bool(b)) => job.allow_failure = Some(*b),
        Some(Value::Map(af)) => {
            job.allow_failure = Some(false);
            job.allow_failure_exit_codes = match af.get("exit_codes") {
                Some(Value::Int(n)) => vec![*n as i32],
                Some(Value::Seq(s)) => s.iter().filter_map(|x| if let Value::Int(n) = x { Some(*n as i32) } else { None }).collect(),
                _ => return Err(err(name, "`allow_failure:exit_codes` must be numbers")),
            };
        }
        Some(_) => return Err(err(name, "`allow_failure` must be a boolean or `{exit_codes}`")),
    }
    job.needs = m.get("needs").map(|v| needs(name, v)).transpose()?;
    job.dependencies = m.get("dependencies").map(|v| strings(name, "dependencies", v)).transpose()?;
    job.artifacts = get("artifacts").filter(|v| !matches!(v, Value::Null)).map(|v| artifacts(name, v)).transpose()?;
    job.cache = get("cache").map(|v| caches(name, v)).transpose()?.unwrap_or_default();
    job.retry = get("retry").map(|v| retry(name, v)).transpose()?;
    job.timeout = get("timeout").map(|v| string(name, "timeout", v)).transpose()?;
    job.interruptible = matches!(get("interruptible"), Some(Value::Bool(true)));
    job.coverage = m.get("coverage").map(|v| string(name, "coverage", v)).transpose()?;
    job.rules = m
        .get("rules")
        .map(|v| rules(&format!("job `{name}`"), v, &["on_success", "on_failure", "always", "manual", "never", "delayed"]))
        .transpose()?;
    job.only = m.get("only").map(|v| filter(name, "only", v)).transpose()?;
    job.except = m.get("except").map(|v| filter(name, "except", v)).transpose()?;
    if job.rules.is_some() && (job.only.is_some() || job.except.is_some()) {
        return Err(err(name, "`rules` cannot be used with `only` or `except`"));
    }
    job.parallel = m.get("parallel").map(|v| parallel(name, v)).transpose()?;
    job.environment = m.get("environment").map(|v| environment(name, v)).transpose()?;
    job.resource_group = m.get("resource_group").map(|v| string(name, "resource_group", v)).transpose()?;
    if let Some(rg) = &job.resource_group
        && (rg.is_empty() || rg.len() > 255)
    {
        return Err(err(name, "`resource_group` must be 1 to 255 characters"));
    }
    job.start_in = m.get("start_in").map(|v| string(name, "start_in", v)).transpose()?;
    if job.when == "delayed" && job.start_in.is_none() {
        return Err(err(name, "`when: delayed` needs `start_in`"));
    }
    if job.start_in.is_some() && job.when != "delayed" && !job.rules.iter().flatten().any(|r| r.when.as_deref() == Some("delayed")) {
        return Err(err(name, "`start_in` goes with `when: delayed`"));
    }
    job.release = m.get("release").map(|v| release(name, v)).transpose()?;
    job.pages = pages(name, m)?;
    job.trigger = m.get("trigger").map(|v| trigger(name, v)).transpose()?;
    job.id_tokens = get("id_tokens").map(|v| id_tokens(name, v)).transpose()?.unwrap_or_default();
    job.manual_confirmation = m.get("manual_confirmation").map(|v| string(name, "manual_confirmation", v)).transpose()?;
    if job.trigger.is_some() {
        // a bridge: GitLab gives it no image, script or runner settings
        job.before_script.clear();
        job.after_script.clear();
        job.image = None;
        job.services.clear();
        job.tags.clear();
        job.artifacts = None;
        job.cache.clear();
        job.id_tokens.clear();
    } else if job.script.is_empty() {
        if job.release.is_some() {
            // GitLab lets a release job have no script of its own
            job.script = vec!["echo \"Creating the release\"".into()];
        } else {
            return Err(err(name, "`script` is required"));
        }
    }
    Ok(job)
}

#[cfg(test)]
mod tests {
    #[test]
    fn matrix_size_is_capped() {
        let m = |rows: &str| super::parallel("j", &crate::value::parse(&format!("matrix:\n{rows}"), "x.yml").unwrap());
        let thirty = (0..30).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
        let huge: String = (0..8).map(|k| format!("    K{k}: [{thirty}]\n")).collect();
        assert!(m(&format!("  - \n{huge}")).is_err(), "30^8 jobs");
        assert!(m(&format!("  - {{A: [{thirty}], B: [{thirty}]}}\n")).is_err(), "900 jobs");
        assert!(m("  - {A: [a, b], B: [c, d]}\n  - {C: [e]}\n").is_ok());
    }

    #[test]
    fn pages_jobs() {
        let pages_of = |src: &str| crate::parse(src).map(|c| c.jobs.values().next().unwrap().pages.clone());
        let p = |dir: &str| Some(super::Pages { publish: dir.into() });
        assert_eq!(pages_of("pages:\n  script: x\n").unwrap(), p("public"), "by its name");
        assert_eq!(pages_of("site:\n  script: x\n  pages: true\n").unwrap(), p("public"));
        assert_eq!(pages_of("site:\n  script: x\n  pages:\n    publish: dist/\n").unwrap(), p("dist"));
        assert_eq!(pages_of("pages:\n  script: x\n  publish: out\n").unwrap(), p("out"), "the older keyword");
        assert_eq!(pages_of("pages:\n  script: x\n  pages: false\n").unwrap(), None);
        for bad in [
            "site:\n  script: x\n  pages:\n    publish: ../etc\n",
            "site:\n  script: x\n  pages:\n    path_prefix: mr\n",
            "site:\n  script: x\n  publish: out\n",
            "site:\n  script: x\n  pages: 3\n",
        ] {
            assert!(pages_of(bad).is_err(), "{bad}");
        }
    }
}
