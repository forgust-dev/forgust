//! `.gitlab-ci.yml` (ADR-0003): the documented subset of GitLab CI/CD config,
//! with a clear error for everything outside it (see the CI compatibility guide).
//!
//! [`load`] reads a config and its includes; [`plan::plan`] picks the jobs of
//! one pipeline from it.

pub mod compose;
pub mod expr;
pub mod include;
pub mod inputs;
pub mod job;
pub mod plan;
pub mod value;

pub use indexmap::{self, IndexMap};

pub use include::{NoIncludes, Resolver};
pub use job::{Artifacts, Cache, Image, Job, Need, NeedFrom, Pages, Retry, Rule};
pub use plan::{Context, Outcome, Plan, PlannedJob};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConfigError {
    #[error("invalid YAML: {0}")]
    Yaml(String),
    #[error("{0}")]
    Invalid(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("include failed: {0}")]
    Include(String),
}

/// A composed config: includes merged, `extends` and `!reference` resolved.
#[derive(Debug, Clone)]
pub struct Config {
    /// With `.pre` first and `.post` last.
    pub stages: Vec<String>,
    /// Top-level `variables` (job variables live on each job).
    pub variables: IndexMap<String, String>,
    pub raw_variables: Vec<String>,
    pub workflow_rules: Option<Vec<Rule>>,
    pub workflow_name: Option<String>,
    /// `workflow:auto_cancel:on_new_commit`: conservative (default), interruptible or none.
    pub auto_cancel: String,
    /// `workflow:auto_cancel:on_job_failure`: none (default) or all (a failed
    /// job cancels the rest of the pipeline).
    pub auto_cancel_on_job_failure: String,
    /// Visible jobs in file order (hidden `.jobs` are templates only).
    pub jobs: IndexMap<String, Job>,
}

const DEFAULT_STAGES: &[&str] = &["build", "test", "deploy"];

/// Read a config and everything it includes.
pub async fn load(source: &str, path: &str, resolver: &dyn Resolver) -> Result<Config, ConfigError> {
    let root = value::parse(source, path)?;
    let merged = include::resolve(root, resolver).await?;
    build(merged)
}

/// A config without includes (tests, quick checks).
pub fn parse(source: &str) -> Result<Config, ConfigError> {
    let root = value::parse(source, ".gitlab-ci.yml")?;
    if root.as_map().is_some_and(|m| m.contains_key("include")) {
        return Err(ConfigError::Include("this config uses `include`; load it with a resolver".into()));
    }
    build(root)
}

fn build(root: value::Value) -> Result<Config, ConfigError> {
    let root = compose::compose(root)?;
    let mut stages: Vec<String> = match root.get("stages") {
        None | Some(value::Value::Null) => DEFAULT_STAGES.iter().map(|s| s.to_string()).collect(),
        Some(value::Value::Seq(s)) => s
            .iter()
            .map(|x| x.scalar_string().ok_or_else(|| ConfigError::Invalid("`stages` must be a list of names".into())))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(ConfigError::Invalid("`stages` must be a list".into())),
    };
    stages.retain(|s| s != ".pre" && s != ".post");
    stages.insert(0, ".pre".into());
    stages.push(".post".into());

    let (variables, raw_variables) = match root.get("variables") {
        Some(v) => job::variables("top-level", v)?,
        None => (IndexMap::new(), Vec::new()),
    };
    let mut auto_cancel = "conservative".to_string();
    let mut auto_cancel_on_job_failure = "none".to_string();
    let (workflow_rules, workflow_name) = match root.get("workflow") {
        None | Some(value::Value::Null) => (None, None),
        Some(value::Value::Map(w)) => {
            for k in w.keys() {
                if !["rules", "name", "auto_cancel"].contains(&k.as_str()) {
                    return Err(ConfigError::Invalid(format!("unknown `workflow:{k}`")));
                }
            }
            let rules = w.get("rules").map(|r| job::rules("workflow", r, &["always", "never"])).transpose()?;
            if let Some(ac) = w.get("auto_cancel") {
                let m = ac.as_map().ok_or_else(|| ConfigError::Invalid("`workflow:auto_cancel` must be a hash".into()))?;
                for (k, v) in m {
                    match k.as_str() {
                        "on_new_commit" => {
                            let mode = v.scalar_string().unwrap_or_default();
                            if !["conservative", "interruptible", "none"].contains(&mode.as_str()) {
                                return Err(ConfigError::Invalid(
                                    "`workflow:auto_cancel:on_new_commit` must be conservative, interruptible or none".into(),
                                ));
                            }
                            auto_cancel = mode;
                        }
                        "on_job_failure" => {
                            let mode = v.scalar_string().unwrap_or_default();
                            if !["none", "all"].contains(&mode.as_str()) {
                                return Err(ConfigError::Invalid("`workflow:auto_cancel:on_job_failure` must be none or all".into()));
                            }
                            auto_cancel_on_job_failure = mode;
                        }
                        other => return Err(ConfigError::Invalid(format!("unknown `workflow:auto_cancel:{other}`"))),
                    }
                }
            }
            (rules, w.get("name").and_then(value::Value::scalar_string))
        }
        Some(_) => return Err(ConfigError::Invalid("`workflow` must be a hash".into())),
    };
    let defaults = root.get("default").and_then(value::Value::as_map).cloned().unwrap_or_default();
    for k in defaults.keys() {
        if !job::DEFAULT_KEYWORDS.contains(&k.as_str()) {
            if ["hooks", "id_tokens"].contains(&k.as_str()) {
                return Err(ConfigError::Unsupported(format!("`default:{k}` is not supported yet")));
            }
            return Err(ConfigError::Invalid(format!("unknown `default:{k}`")));
        }
    }

    let mut jobs = IndexMap::new();
    for (name, v) in &root {
        if compose::GLOBAL_KEYWORDS.contains(&name.as_str()) || name.starts_with('.') {
            continue;
        }
        let m = v.as_map().ok_or_else(|| ConfigError::Invalid(format!("job `{name}` must be a hash")))?;
        let j = job::build(name, m, &defaults)?;
        if !stages.contains(&j.stage) {
            return Err(ConfigError::Invalid(format!("job `{name}`: stage `{}` is not in `stages`", j.stage)));
        }
        jobs.insert(name.clone(), j);
    }
    if jobs.is_empty() {
        return Err(ConfigError::Invalid("the config defines no jobs".into()));
    }
    for (name, j) in &jobs {
        for d in j.dependencies.iter().flatten() {
            let dj = jobs.get(d).ok_or_else(|| ConfigError::Invalid(format!("job `{name}` depends on unknown job `{d}`")))?;
            let (a, b) = (stages.iter().position(|s| *s == dj.stage), stages.iter().position(|s| *s == j.stage));
            if a >= b && j.needs.is_none() {
                return Err(ConfigError::Invalid(format!("job `{name}` depends on `{d}`, which is not in an earlier stage")));
            }
        }
        for n in j.needs.iter().flatten().filter(|n| n.from.is_none()) {
            if !jobs.contains_key(&n.job) && !n.optional {
                return Err(ConfigError::Invalid(format!("job `{name}` needs unknown job `{}`", n.job)));
            }
        }
    }
    for (name, j) in &jobs {
        if let Some(stop) = j.environment.as_ref().and_then(|e| e.on_stop.as_ref())
            && !jobs.contains_key(stop)
        {
            return Err(ConfigError::Invalid(format!("job `{name}`: `environment:on_stop` names unknown job `{stop}`")));
        }
    }
    Ok(Config { stages, variables, raw_variables, workflow_rules, workflow_name, auto_cancel, auto_cancel_on_job_failure, jobs })
}

impl Config {
    fn all_rules(&self) -> impl Iterator<Item = &Rule> {
        self.workflow_rules.iter().flatten().chain(self.jobs.values().flat_map(|j| j.rules.iter().flatten()))
    }

    /// Refs `rules:changes:compare_to` names (unexpanded): the server diffs
    /// against each before planning ([`Context::changed_vs`]).
    pub fn compare_to_refs(&self) -> Vec<String> {
        let mut v: Vec<String> = self.all_rules().filter_map(|r| r.compare_to.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// (project, ref) pairs `rules:exists` looks in ([`Context::files_in`]).
    pub fn exists_projects(&self) -> Vec<(String, Option<String>)> {
        let mut v: Vec<(String, Option<String>)> =
            self.all_rules().filter_map(|r| r.exists_project.clone().map(|p| (p, r.exists_ref.clone()))).collect();
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_pipeline() {
        let p = parse(
            "stages: [build, test]\nbuild:\n  stage: build\n  image: rust:1\n  script: cargo build\ntest:\n  script:\n    - cargo test\n",
        )
        .unwrap();
        assert_eq!(p.stages, vec![".pre", "build", "test", ".post"]);
        assert_eq!(p.jobs["build"].script, vec!["cargo build"]);
        assert_eq!(p.jobs["build"].image.as_ref().unwrap().name, "rust:1");
        assert_eq!(p.jobs["test"].stage, "test");
    }

    #[test]
    fn rejects_unsupported_keywords_clearly() {
        let err = parse("deploy:\n  script: x\n  secrets:\n    A: {vault: a/b}\n").unwrap_err();
        assert!(err.to_string().contains("secrets"), "{err}");
        let err = parse("t:\n  trigger: other/project\n  script: x\n").unwrap_err();
        assert!(err.to_string().contains("runs no script"), "{err}");
        let err = parse("job:\n  script: x\n  scriptt: y\n").unwrap_err();
        assert!(err.to_string().contains("unknown keyword `scriptt`"), "{err}");
    }

    #[test]
    fn ci_keywords_of_for5() {
        let c = parse(
            "deploy:\n  script: x\n  environment: {name: review/$CI_COMMIT_REF_SLUG, url: https://x, on_stop: stop, auto_stop_in: 1 day}\n  resource_group: prod\n  id_tokens:\n    T: {aud: https://sts.example.com}\n  manual_confirmation: Sure?\n  when: manual\nstop:\n  script: y\n  environment: {name: review/$CI_COMMIT_REF_SLUG, action: stop}\n  when: manual\nchild:\n  trigger:\n    include: [ci/a.yml, {artifact: gen.yml, job: deploy}]\n    strategy: depend\nother:\n  trigger: group/project\nrel:\n  release: {tag_name: v1, description: notes}\nlater:\n  script: z\n  when: delayed\n  start_in: 10 minutes\nworkflow:\n  auto_cancel: {on_job_failure: all}\n",
        )
        .unwrap();
        let d = &c.jobs["deploy"];
        assert_eq!(d.environment.as_ref().unwrap().on_stop.as_deref(), Some("stop"));
        assert_eq!(d.resource_group.as_deref(), Some("prod"));
        assert_eq!(d.id_tokens["T"].aud, vec!["https://sts.example.com"]);
        assert_eq!(d.manual_confirmation.as_deref(), Some("Sure?"));
        assert_eq!(c.jobs["stop"].environment.as_ref().unwrap().action, "stop");
        let t = c.jobs["child"].trigger.as_ref().unwrap();
        assert!(t.depend && t.project.is_none() && t.include.len() == 2);
        assert_eq!(c.jobs["other"].trigger.as_ref().unwrap().project.as_deref(), Some("group/project"));
        assert!(!c.jobs["rel"].script.is_empty(), "a release job without a script gets a no-op one");
        assert_eq!(c.jobs["later"].start_in.as_deref(), Some("10 minutes"));
        assert_eq!(c.auto_cancel_on_job_failure, "all");
        for (bad, why) in [
            ("a:\n  script: x\n  when: delayed\n", "start_in"),
            ("a:\n  script: x\n  environment: {name: x, action: launch}\n", "action"),
            ("a:\n  script: x\n  environment: {name: x, on_stop: nope}\n", "unknown job `nope`"),
            ("a:\n  trigger: {project: p, include: x.yml}\n", "not both"),
            ("a:\n  script: x\n  id_tokens: {T: {}}\n", "aud"),
            ("a:\n  release: {name: x}\n", "tag_name"),
            ("a:\n  script: x\n  rules:\n    - exists: {paths: [x], ref: main}\n", "needs `project`"),
        ] {
            let e = parse(bad).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
        let c = parse("a:\n  script: x\n  rules:\n    - changes: {paths: [src/**], compare_to: refs/heads/main}\n    - exists: {paths: [Dockerfile], project: g/p, ref: v1}\n").unwrap();
        assert_eq!(c.compare_to_refs(), vec!["refs/heads/main"]);
        assert_eq!(c.exists_projects(), vec![("g/p".to_string(), Some("v1".to_string()))]);
    }

    #[test]
    fn requires_script_and_known_stage() {
        assert!(parse("a:\n  image: x\n").unwrap_err().to_string().contains("script"));
        assert!(parse("a:\n  stage: nope\n  script: x\n").unwrap_err().to_string().contains("nope"));
    }

    #[test]
    fn defaults_and_inherit() {
        let c = parse(
            "default:\n  image: node:20\n  before_script: [npm ci]\n  tags: [docker]\nvariables:\n  A: 1\n  B: '2'\na:\n  script: x\nb:\n  image: alpine\n  inherit:\n    default: [tags]\n    variables: [B]\n  script: x\n",
        )
        .unwrap();
        let a = &c.jobs["a"];
        assert_eq!(a.image.as_ref().unwrap().name, "node:20");
        assert_eq!(a.before_script, vec!["npm ci"]);
        let b = &c.jobs["b"];
        assert_eq!(b.image.as_ref().unwrap().name, "alpine");
        assert!(b.before_script.is_empty());
        assert_eq!(b.tags, vec!["docker"]);
        assert_eq!(b.inherit_variables.as_deref(), Some(&["B".to_string()][..]));
        assert_eq!(c.variables["A"], "1");
    }

    #[test]
    fn rich_keywords() {
        let c = parse(
            "job:\n  image: {name: 'golang:1.22', entrypoint: ['']}\n  services: [postgres:16, {name: redis:7, alias: cache}]\n  script: [go test ./...]\n  variables:\n    GOFLAGS: {value: -mod=mod, description: flags}\n    RAW: {value: $NOT_EXPANDED, expand: false}\n  artifacts:\n    when: always\n    reports: {junit: report.xml}\n  cache:\n    key: {files: [go.sum]}\n    paths: [.cache]\n  retry: {max: 2, when: [runner_system_failure]}\n  allow_failure: {exit_codes: [42]}\n  timeout: 30m\n  interruptible: true\n",
        )
        .unwrap();
        let j = &c.jobs["job"];
        assert_eq!(j.image.as_ref().unwrap().entrypoint.as_deref(), Some(&["".to_string()][..]));
        assert_eq!(j.services[1].alias.as_deref(), Some("cache"));
        assert_eq!(j.variables["GOFLAGS"], "-mod=mod");
        assert_eq!(j.raw_variables, vec!["RAW"]);
        assert_eq!(j.artifacts.as_ref().unwrap().reports["junit"], vec!["report.xml"]);
        assert_eq!(j.cache[0].key_files, vec!["go.sum"]);
        assert_eq!(j.retry.as_ref().unwrap().when, vec!["runner_system_failure"]);
        assert_eq!(j.allow_failure_exit_codes, vec![42]);
    }

    struct Files(Vec<(&'static str, &'static str)>);

    #[async_trait::async_trait]
    impl Resolver for Files {
        async fn local(&self, path: &str) -> Result<String, String> {
            self.0.iter().find(|(p, _)| *p == path).map(|(_, c)| c.to_string()).ok_or_else(|| "not found".into())
        }
        async fn local_glob(&self, pattern: &str) -> Result<Vec<String>, String> {
            let m = globset::GlobBuilder::new(pattern).literal_separator(true).build().unwrap().compile_matcher();
            Ok(self.0.iter().filter(|(p, _)| m.is_match(p)).map(|(p, _)| p.to_string()).collect())
        }
        async fn project(&self, project: &str, _: Option<&str>, file: &str) -> Result<String, String> {
            self.local(&format!("{project}/{file}")).await
        }
        async fn remote(&self, url: &str) -> Result<String, String> {
            self.local(url).await
        }
    }

    #[test]
    fn includes_merge_in_order_and_main_wins() {
        let files = Files(vec![
            ("ci/base.yml", ".tpl:\n  image: base\nlint:\n  script: [lint-base]\n  variables: {LEVEL: base}\n"),
            ("ci/jobs/a.yml", "a:\n  extends: .tpl\n  script: [a]\n"),
            ("ci/jobs/b.yml", "b:\n  script: [b]\n"),
            ("shared/ci/tpl.yml", "include: ci/nested.yml\nshared:\n  script: [shared]\n"),
            ("shared/ci/nested.yml", "nested:\n  script: [nested]\n"),
            ("https://example.com/r.yml", "remote:\n  script: [r]\n"),
        ]);
        let main = "include:\n  - local: /ci/base.yml\n  - ci/jobs/*.yml\n  - project: shared\n    file: ci/tpl.yml\n  - remote: https://example.com/r.yml\nlint:\n  variables: {LEVEL: main}\n";
        let rt = tokio_rt();
        let c = rt.block_on(load(main, ".gitlab-ci.yml", &files)).unwrap();
        let names: Vec<&str> = c.jobs.keys().map(String::as_str).collect();
        for n in ["lint", "a", "b", "shared", "nested", "remote"] {
            assert!(names.contains(&n), "{names:?} misses {n}");
        }
        assert_eq!(c.jobs["a"].image.as_ref().unwrap().name, "base", "extends across files");
        assert_eq!(c.jobs["lint"].variables["LEVEL"], "main", "main file wins");
        assert_eq!(c.jobs["lint"].script, vec!["lint-base"], "deep merge keeps included keys");
        let bad = rt.block_on(load("include: missing.yml\n", ".gitlab-ci.yml", &files)).unwrap_err();
        assert!(matches!(bad, ConfigError::Include(_)), "{bad}");
        let tpl = rt.block_on(load("include:\n  template: Auto-DevOps.gitlab-ci.yml\n", ".gitlab-ci.yml", &files)).unwrap_err();
        assert!(matches!(tpl, ConfigError::Unsupported(_)));
    }

    fn tokio_rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().build().unwrap()
    }
}
