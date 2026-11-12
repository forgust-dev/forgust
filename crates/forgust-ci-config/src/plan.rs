//! Turn a config into the jobs of one pipeline: `workflow:rules`, job `rules`
//! (or `only`/`except`), `needs` pruning, and `parallel` expansion.

use std::collections::HashMap;

use globset::{Glob, GlobBuilder, GlobMatcher};
use indexmap::IndexMap;

use crate::expr::Expression;
use crate::job::{Filter, Job, Need, Parallel, Rule};
use crate::{Config, ConfigError};

/// What the pipeline is about. `vars` holds every variable rules may test:
/// predefined, global YAML, project/group, and pipeline variables.
pub struct Context<'a> {
    pub vars: &'a HashMap<String, String>,
    /// `push`, `web`, `api`, `schedule`, `merge_request_event`, `trigger`.
    pub source: &'a str,
    pub git_ref: &'a str,
    pub tag: bool,
    /// Changed paths for `rules:changes`; None means "unknown" (always true,
    /// as GitLab does for new branches and web pipelines).
    pub changed: Option<&'a [String]>,
    /// Every path at the commit, for `rules:exists`.
    pub files: Option<&'a [String]>,
    /// `rules:changes:compare_to`: changed paths against each ref the config
    /// names (missing: unknown, which matches, as for `changed`).
    pub changed_vs: Option<&'a HashMap<String, Vec<String>>>,
    /// `rules:exists:project`: paths of other projects, keyed `project@ref`
    /// (`ref` empty for the default branch); missing: no files.
    pub files_in: Option<&'a HashMap<String, Vec<String>>>,
}

/// The key of [`Context::files_in`].
pub fn files_key(project: &str, git_ref: Option<&str>) -> String {
    format!("{}@{}", project.trim_matches('/'), git_ref.unwrap_or(""))
}

#[derive(Debug, Clone)]
pub struct PlannedJob {
    pub name: String,
    pub stage: String,
    pub stage_idx: i32,
    /// `on_success`, `on_failure`, `always` or `manual` (after rules).
    pub when: String,
    pub allow_failure: bool,
    /// Job names this job needs (after pruning and parallel expansion); None:
    /// every job of earlier stages.
    pub needs: Option<Vec<String>>,
    /// The job with rule variables and parallel variables applied.
    pub job: Job,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub stages: Vec<String>,
    pub jobs: Vec<PlannedJob>,
    /// Variables added by the matching `workflow:rules` entry.
    pub workflow_variables: IndexMap<String, String>,
}

pub enum Outcome {
    /// `workflow:rules` (or no matching job) means no pipeline.
    Skip(String),
    Run(Plan),
}

fn glob(pattern: &str) -> Option<GlobMatcher> {
    // GitLab: `*` stays within a path segment, `**` crosses them, braces expand
    GlobBuilder::new(pattern.trim_start_matches('/')).literal_separator(true).build().ok().map(|g: Glob| g.compile_matcher())
}

fn any_match(patterns: &[String], paths: Option<&[String]>, vars: &HashMap<String, String>) -> bool {
    let Some(paths) = paths else { return true };
    patterns.iter().any(|p| {
        let p = expand_vars(p, vars);
        match glob(&p) {
            Some(m) => paths.iter().any(|x| m.is_match(x)),
            None => false,
        }
    })
}

/// `$VAR` / `${VAR}` in paths (GitLab expands variables in `changes`/`exists`).
fn expand_vars(s: &str, vars: &HashMap<String, String>) -> String {
    if !s.contains('$') {
        return s.to_string();
    }
    let mut out = String::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '$' {
            let braced = b.get(i + 1) == Some(&'{');
            let start = if braced { i + 2 } else { i + 1 };
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == '_') {
                j += 1;
            }
            if j > start {
                let name: String = b[start..j].iter().collect();
                out.push_str(vars.get(&name).map(String::as_str).unwrap_or(""));
                i = if braced && b.get(j) == Some(&'}') { j + 1 } else { j };
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn rule_matches(r: &Rule, ctx: &Context<'_>, vars: &HashMap<String, String>) -> Result<bool, ConfigError> {
    if let Some(cond) = &r.if_
        && !Expression::parse(cond)?.eval(vars)
    {
        return Ok(false);
    }
    if let Some(ch) = &r.changes {
        let changed = match &r.compare_to {
            Some(base) => ctx.changed_vs.and_then(|m| m.get(&expand_vars(base, vars))).map(Vec::as_slice),
            None => ctx.changed,
        };
        if !any_match(ch, changed, vars) {
            return Ok(false);
        }
    }
    if let Some(ex) = &r.exists {
        let files: &[String] = match &r.exists_project {
            Some(p) => {
                let key = files_key(&expand_vars(p, vars), r.exists_ref.as_ref().map(|x| expand_vars(x, vars)).as_deref());
                ctx.files_in.and_then(|m| m.get(&key)).map(Vec::as_slice).unwrap_or(&[])
            }
            None => ctx.files.unwrap_or(&[]),
        };
        if !any_match(ex, Some(files), vars) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn ref_matches(pattern: &str, ctx: &Context<'_>) -> bool {
    let branch_pipeline = !ctx.tag && ctx.source != "merge_request_event";
    match pattern {
        "branches" => branch_pipeline,
        "tags" => ctx.tag,
        "merge_requests" => ctx.source == "merge_request_event",
        "pushes" => ctx.source == "push",
        "web" => ctx.source == "web",
        "api" => ctx.source == "api",
        "schedules" => ctx.source == "schedule",
        "triggers" => ctx.source == "trigger",
        "pipelines" | "external" | "chat" | "external_pull_requests" => false,
        p if p.starts_with('/') && p.len() > 1 => {
            let end = p.rfind('/').unwrap_or(0);
            if end == 0 {
                return false;
            }
            let (pat, flags) = (&p[1..end], &p[end + 1..]);
            let pat = if flags.contains('i') { format!("(?i){pat}") } else { pat.to_string() };
            regex::RegexBuilder::new(&pat).size_limit(1 << 20).build().map(|re| re.is_match(ctx.git_ref)).unwrap_or(false)
        }
        // a literal branch or tag name
        p => ctx.source != "merge_request_event" && p == ctx.git_ref,
    }
}

/// `only`/`except` keys are ANDed: each present key must have a matching entry.
fn filter_matches(f: &Filter, ctx: &Context<'_>, vars: &HashMap<String, String>) -> Result<bool, ConfigError> {
    if !f.refs.is_empty() && !f.refs.iter().any(|r| ref_matches(r, ctx)) {
        return Ok(false);
    }
    if !f.variables.is_empty() {
        let mut any = false;
        for e in &f.variables {
            if Expression::parse(e)?.eval(vars) {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(false);
        }
    }
    if !f.changes.is_empty() && !any_match(&f.changes, ctx.changed, vars) {
        return Ok(false);
    }
    Ok(true)
}

/// A job that is part of the pipeline: its `when`, `allow_failure`, and rule variables.
/// (when, allow_failure, rule variables, start_in for `when: delayed`)
type Selected = (String, bool, IndexMap<String, String>, Option<String>);

/// Evaluate one job: None if it is not part of this pipeline.
/// `yaml`: top-level (and workflow) YAML variables; job variables override
/// them, and `ctx.vars` (predefined, project, group, pipeline) override both.
fn evaluate(job: &Job, ctx: &Context<'_>, yaml: &HashMap<String, String>) -> Result<Option<Selected>, ConfigError> {
    let mut vars = yaml.clone();
    for (k, v) in &job.variables {
        vars.insert(k.clone(), v.clone());
    }
    for (k, v) in ctx.vars {
        vars.insert(k.clone(), v.clone());
    }
    if let Some(rules) = &job.rules {
        for r in rules {
            if !rule_matches(r, ctx, &vars)? {
                continue;
            }
            let when = r.when.clone().unwrap_or_else(|| job.when.clone());
            if when == "never" {
                return Ok(None);
            }
            // manual jobs from rules default to blocking (allow_failure: false)
            let allow = r.allow_failure.or(job.allow_failure).unwrap_or(false);
            let start_in = if when == "delayed" { r.start_in.clone().or_else(|| job.start_in.clone()) } else { None };
            return Ok(Some((when, allow, r.variables.clone(), start_in)));
        }
        return Ok(None);
    }
    let default_only = Filter { refs: vec!["branches".into(), "tags".into()], ..Default::default() };
    let only = job.only.as_ref().unwrap_or(&default_only);
    if !filter_matches(only, ctx, &vars)? {
        return Ok(None);
    }
    if let Some(except) = &job.except
        && filter_matches(except, ctx, &vars)?
    {
        return Ok(None);
    }
    let allow = job.allow_failure.unwrap_or(job.when == "manual");
    Ok(Some((job.when.clone(), allow, IndexMap::new(), job.start_in.clone())))
}

/// Jobs in one pipeline, counting every `parallel` instance.
pub const MAX_JOBS: usize = 2000;

/// Instances of a job: itself, `name 1/N`… or `name: [a, b]` for a matrix.
fn instances(name: &str, job: &Job) -> Vec<(String, IndexMap<String, String>)> {
    match &job.parallel {
        None => vec![(name.to_string(), IndexMap::new())],
        Some(Parallel::Count(n)) => (1..=*n)
            .map(|i| {
                let mut v = IndexMap::new();
                v.insert("CI_NODE_INDEX".into(), i.to_string());
                v.insert("CI_NODE_TOTAL".into(), n.to_string());
                (format!("{name} {i}/{n}"), v)
            })
            .collect(),
        Some(Parallel::Matrix(rows)) => {
            let mut combos: Vec<IndexMap<String, String>> = Vec::new();
            for row in rows {
                let mut acc: Vec<IndexMap<String, String>> = vec![IndexMap::new()];
                for (k, values) in row {
                    let mut next = Vec::new();
                    for a in &acc {
                        for v in values {
                            let mut c = a.clone();
                            c.insert(k.clone(), v.clone());
                            next.push(c);
                        }
                    }
                    acc = next;
                }
                combos.extend(acc);
            }
            let total = combos.len();
            combos
                .into_iter()
                .enumerate()
                .map(|(i, mut vars)| {
                    let label = vars.values().cloned().collect::<Vec<_>>().join(", ");
                    vars.insert("CI_NODE_INDEX".into(), (i + 1).to_string());
                    vars.insert("CI_NODE_TOTAL".into(), total.to_string());
                    (format!("{name}: [{label}]"), vars)
                })
                .collect()
        }
    }
}

pub fn plan(cfg: &Config, ctx: &Context<'_>) -> Result<Outcome, ConfigError> {
    // YAML variables (top level, then workflow rules) sit under ctx.vars
    let mut yaml: HashMap<String, String> = cfg.variables.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut vars = yaml.clone();
    for (k, v) in ctx.vars {
        vars.insert(k.clone(), v.clone());
    }

    let mut workflow_variables = IndexMap::new();
    if let Some(rules) = &cfg.workflow_rules {
        let mut matched = false;
        for r in rules {
            if rule_matches(r, ctx, &vars)? {
                if r.when.as_deref() == Some("never") {
                    return Ok(Outcome::Skip("workflow:rules excluded this pipeline".into()));
                }
                workflow_variables = r.variables.clone();
                matched = true;
                break;
            }
        }
        if !matched {
            return Ok(Outcome::Skip("no workflow:rules entry matched".into()));
        }
        for (k, v) in &workflow_variables {
            yaml.insert(k.clone(), v.clone());
        }
    }

    // evaluate jobs
    let mut chosen: Vec<(String, Job, String, bool)> = Vec::new();
    for (name, job) in &cfg.jobs {
        if let Some((when, allow, rule_vars, start_in)) = evaluate(job, ctx, &yaml)? {
            let mut j = job.clone();
            for (k, v) in rule_vars {
                j.variables.insert(k, v);
            }
            j.start_in = start_in;
            chosen.push((name.clone(), j, when, allow));
        }
    }
    if chosen.is_empty() {
        return Ok(Outcome::Skip("no job matched the rules for this pipeline".into()));
    }

    // parallel names: a need/dependency on `build` means every `build` instance
    let mut instance_names: HashMap<String, Vec<String>> = HashMap::new();
    for (name, job, _, _) in &chosen {
        instance_names.insert(name.clone(), instances(name, job).into_iter().map(|(n, _)| n).collect());
    }
    let total: usize = instance_names.values().map(Vec::len).sum();
    if total > MAX_JOBS {
        return Err(ConfigError::Invalid(format!("this pipeline would have {total} jobs; at most {MAX_JOBS} are allowed")));
    }
    let present = |n: &str| instance_names.contains_key(n);

    let mut jobs = Vec::new();
    for (name, job, when, allow) in &chosen {
        let needs = match &job.needs {
            None => None,
            Some(list) => {
                let mut out = Vec::new();
                let mut kept: Vec<Need> = Vec::new();
                for n in list {
                    // from another pipeline or project: artifacts only, no edge here
                    if n.from.is_some() {
                        kept.push(n.clone());
                        continue;
                    }
                    if !present(&n.job) {
                        if n.optional {
                            continue;
                        }
                        if cfg.jobs.contains_key(&n.job) {
                            return Err(ConfigError::Invalid(format!("job `{name}` needs `{}`, which is not in this pipeline", n.job)));
                        }
                        return Err(ConfigError::Invalid(format!("job `{name}` needs `{}`, which does not exist", n.job)));
                    }
                    match &n.matrix {
                        None => out.extend(instance_names[&n.job].iter().cloned()),
                        // `needs:parallel:matrix`: only the named instances
                        Some(rows) => {
                            let target = chosen.iter().find(|(cn, ..)| cn == &n.job).map(|(_, j, ..)| j);
                            let mut picked = Vec::new();
                            for (iname, ivars) in target.map(|t| instances(&n.job, t)).unwrap_or_default() {
                                let hit = rows.iter().any(|row| row.iter().all(|(k, vals)| ivars.get(k).is_some_and(|v| vals.contains(v))));
                                if hit {
                                    picked.push(iname);
                                }
                            }
                            if picked.is_empty() {
                                return Err(ConfigError::Invalid(format!(
                                    "job `{name}` needs a `{}` matrix instance that does not exist",
                                    n.job
                                )));
                            }
                            out.extend(picked);
                        }
                    }
                    kept.push(n.clone());
                }
                Some((out, kept))
            }
        };
        let dependencies = job
            .dependencies
            .as_ref()
            .map(|d| d.iter().flat_map(|x| instance_names.get(x).cloned().unwrap_or_default()).collect::<Vec<_>>());
        let stage_idx = cfg.stages.iter().position(|s| *s == job.stage).unwrap_or(0) as i32;
        for (iname, pvars) in instances(name, job) {
            let mut j = job.clone();
            for (k, v) in &pvars {
                j.variables.insert(k.clone(), v.clone());
            }
            j.when = when.clone();
            j.allow_failure = Some(*allow);
            j.dependencies = dependencies.clone();
            if let Some((_, kept)) = &needs {
                j.needs = Some(kept.clone());
            }
            jobs.push(PlannedJob {
                name: iname,
                stage: job.stage.clone(),
                stage_idx,
                when: when.clone(),
                allow_failure: *allow,
                needs: needs.as_ref().map(|(n, _)| n.clone()),
                job: j,
            });
        }
    }
    // stage-ordered `needs` must not point to a later stage
    for j in &jobs {
        if let Some(needs) = &j.needs {
            for n in needs {
                if let Some(o) = jobs.iter().find(|o| &o.name == n)
                    && o.stage_idx > j.stage_idx
                {
                    return Err(ConfigError::Invalid(format!("job `{}` needs `{n}`, which is in a later stage", j.name)));
                }
            }
        }
    }
    let stages = cfg.stages.iter().filter(|s| jobs.iter().any(|j| &j.stage == *s)).cloned().collect();
    Ok(Outcome::Run(Plan { stages, jobs, workflow_variables }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(src: &str) -> Config {
        crate::parse(src).unwrap()
    }

    fn ctx<'a>(vars: &'a HashMap<String, String>, source: &'a str, git_ref: &'a str, changed: Option<&'a [String]>) -> Context<'a> {
        Context { vars, source, git_ref, tag: false, changed, files: Some(&[]), changed_vs: None, files_in: None }
    }

    fn names(o: Outcome) -> Vec<String> {
        match o {
            Outcome::Run(p) => p.jobs.into_iter().map(|j| j.name).collect(),
            Outcome::Skip(why) => vec![format!("SKIP: {why}")],
        }
    }

    fn v(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn rules_pick_when_and_variables() {
        let c = cfg(
            "deploy:\n  script: x\n  rules:\n    - if: $CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH\n      when: manual\n      variables: {TARGET: prod}\n    - when: never\n",
        );
        let vars = v(&[("CI_COMMIT_BRANCH", "main"), ("CI_DEFAULT_BRANCH", "main")]);
        let Outcome::Run(p) = plan(&c, &ctx(&vars, "push", "main", None)).unwrap() else { panic!() };
        assert_eq!(p.jobs[0].when, "manual");
        assert!(!p.jobs[0].allow_failure, "manual from rules blocks");
        assert_eq!(p.jobs[0].job.variables["TARGET"], "prod");
        let vars = v(&[("CI_COMMIT_BRANCH", "dev"), ("CI_DEFAULT_BRANCH", "main")]);
        assert!(names(plan(&c, &ctx(&vars, "push", "dev", None)).unwrap())[0].starts_with("SKIP"));
    }

    #[test]
    fn jobs_without_rules_skip_merge_request_pipelines() {
        let c = cfg("a:\n  script: x\nb:\n  script: x\n  rules:\n    - if: $CI_PIPELINE_SOURCE == 'merge_request_event'\n");
        let vars = v(&[("CI_PIPELINE_SOURCE", "merge_request_event")]);
        assert_eq!(names(plan(&c, &ctx(&vars, "merge_request_event", "feat", None)).unwrap()), vec!["b"]);
        let vars = v(&[("CI_PIPELINE_SOURCE", "push")]);
        assert_eq!(names(plan(&c, &ctx(&vars, "push", "feat", None)).unwrap()), vec!["a"]);
    }

    #[test]
    fn workflow_rules() {
        let c = cfg(
            "workflow:\n  rules:\n    - if: $CI_COMMIT_TAG\n      when: never\n    - if: $CI_PIPELINE_SOURCE == 'push'\n      variables: {MODE: fast}\njob:\n  script: echo $MODE\n",
        );
        let vars = v(&[("CI_PIPELINE_SOURCE", "push")]);
        let Outcome::Run(p) = plan(&c, &ctx(&vars, "push", "main", None)).unwrap() else { panic!() };
        assert_eq!(p.workflow_variables["MODE"], "fast");
        let vars = v(&[("CI_PIPELINE_SOURCE", "web")]);
        assert!(matches!(plan(&c, &ctx(&vars, "web", "main", None)).unwrap(), Outcome::Skip(_)));
    }

    #[test]
    fn changes_and_exists() {
        let c = cfg(
            "docs:\n  script: x\n  rules:\n    - changes: ['docs/**/*.md']\nrust:\n  script: x\n  rules:\n    - exists: [Cargo.toml]\n",
        );
        let vars = HashMap::new();
        let changed = vec!["docs/guide/a.md".to_string()];
        let files = vec!["Cargo.toml".to_string(), "src/main.rs".to_string()];
        let c1 = Context {
            vars: &vars,
            source: "push",
            git_ref: "main",
            tag: false,
            changed: Some(&changed),
            files: Some(&files),
            changed_vs: None,
            files_in: None,
        };
        assert_eq!(names(plan(&c, &c1).unwrap()), vec!["docs", "rust"]);
        let other = vec!["src/main.rs".to_string()];
        let c2 = Context { changed: Some(&other), files: Some(&[]), ..c1 };
        assert!(names(plan(&c, &c2).unwrap())[0].starts_with("SKIP"));
    }

    #[test]
    fn only_except() {
        let c = cfg(
            "a:\n  script: x\n  only: [main, /^release-.*$/]\nb:\n  script: x\n  except: [tags]\nc:\n  script: x\n  only:\n    refs: [branches]\n    variables: [$DEPLOY]\n",
        );
        let vars = HashMap::new();
        assert_eq!(names(plan(&c, &ctx(&vars, "push", "release-1", None)).unwrap()), vec!["a", "b"]);
        let vars = v(&[("DEPLOY", "1")]);
        assert_eq!(names(plan(&c, &ctx(&vars, "push", "dev", None)).unwrap()), vec!["b", "c"]);
    }

    #[test]
    fn needs_pruning_and_parallel() {
        let c = cfg(
            "build:\n  stage: build\n  parallel: 2\n  script: x\nlint:\n  stage: build\n  script: x\n  rules: [{when: never}]\ntest:\n  stage: test\n  needs: [build, {job: lint, optional: true}]\n  script: x\n",
        );
        let vars = HashMap::new();
        let Outcome::Run(p) = plan(&c, &ctx(&vars, "push", "main", None)).unwrap() else { panic!() };
        let names: Vec<&str> = p.jobs.iter().map(|j| j.name.as_str()).collect();
        assert_eq!(names, vec!["build 1/2", "build 2/2", "test"]);
        assert_eq!(p.jobs[2].needs.as_deref().unwrap(), ["build 1/2", "build 2/2"]);
        assert_eq!(p.jobs[1].job.variables["CI_NODE_INDEX"], "2");
        // a required need that rules removed is an error
        let c = cfg("a:\n  script: x\n  rules: [{when: never}]\nb:\n  needs: [a]\n  script: x\n");
        assert!(plan(&c, &ctx(&vars, "push", "main", None)).is_err());
    }

    #[test]
    fn needs_from_other_projects_and_pipelines_are_not_edges() {
        use crate::NeedFrom;
        let c = cfg(
            "build:\n  script: x\ntest:\n  needs:\n    - build\n    - {project: acme/lib, job: dist, ref: main}\n    - {pipeline: $PARENT_PIPELINE_ID, job: gen, artifacts: true}\n  script: x\n",
        );
        let vars = HashMap::new();
        let Outcome::Run(p) = plan(&c, &ctx(&vars, "push", "main", None)).unwrap() else { panic!() };
        assert_eq!(p.jobs[1].needs.as_deref().unwrap(), ["build"], "only this pipeline's jobs order it");
        let kept = p.jobs[1].job.needs.as_ref().unwrap();
        assert_eq!(kept[1].from, Some(NeedFrom::Project { project: "acme/lib".into(), git_ref: "main".into() }));
        assert_eq!(kept[2].from, Some(NeedFrom::Pipeline { pipeline: "$PARENT_PIPELINE_ID".into() }));
        for bad in [
            "a:\n  needs: [{project: acme/lib, job: dist}]\n  script: x\n",
            "a:\n  needs: [{project: acme/lib, pipeline: '1', job: dist, ref: main}]\n  script: x\n",
            "a:\n  needs: [{job: dist, ref: main}]\n  script: x\n",
        ] {
            assert!(crate::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn job_variables_override_top_level_ones_in_rules() {
        let c = cfg(
            "variables:\n  DEPLOY: 'false'\ndeploy:\n  script: x\n  variables:\n    DEPLOY: 'true'\n  rules:\n    - if: $DEPLOY == 'true'\n",
        );
        let vars = HashMap::new();
        assert_eq!(names(plan(&c, &ctx(&vars, "push", "main", None)).unwrap()), vec!["deploy"]);
        // a pipeline variable still wins over the job's
        let vars = v(&[("DEPLOY", "no")]);
        assert!(names(plan(&c, &ctx(&vars, "push", "main", None)).unwrap())[0].starts_with("SKIP"));
    }

    #[test]
    fn matrix() {
        let c = cfg("test:\n  script: x\n  parallel:\n    matrix:\n      - OS: [linux, mac]\n        ARCH: amd64\n      - OS: win\n");
        let vars = HashMap::new();
        assert_eq!(
            names(plan(&c, &ctx(&vars, "push", "main", None)).unwrap()),
            vec!["test: [linux, amd64]", "test: [mac, amd64]", "test: [win]"]
        );
    }
}
