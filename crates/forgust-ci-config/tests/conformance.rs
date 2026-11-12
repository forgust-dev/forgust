//! The conformance corpus (`ci-conformance/cases`): every case through our
//! engine. A nightly job compares the same cases with a real GitLab CE.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use forgust_ci_config::{Context, Outcome, Resolver};
use serde_json::Value;

struct Dir(PathBuf);

#[async_trait::async_trait]
impl Resolver for Dir {
    async fn local(&self, path: &str) -> Result<String, String> {
        std::fs::read_to_string(self.0.join(path)).map_err(|e| e.to_string())
    }
    async fn local_glob(&self, pattern: &str) -> Result<Vec<String>, String> {
        let m = globset::GlobBuilder::new(pattern).literal_separator(true).build().unwrap().compile_matcher();
        let mut out = Vec::new();
        walk(&self.0, &self.0, &mut out);
        Ok(out.into_iter().filter(|p| m.is_match(p)).collect())
    }
    async fn project(&self, _: &str, _: Option<&str>, _: &str) -> Result<String, String> {
        Err("project includes are not used in the corpus".into())
    }
    async fn remote(&self, _: &str) -> Result<String, String> {
        Err("remote includes are not used in the corpus".into())
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(root, &p, out);
        } else {
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().into());
        }
    }
}

/// What a case's pipeline looks like, in the corpus's JSON shape.
async fn run_case(dir: &Path) -> Value {
    let source = std::fs::read_to_string(dir.join(".gitlab-ci.yml")).unwrap();
    let ctx: Value = std::fs::read_to_string(dir.join("context.json")).ok().map(|s| serde_json::from_str(&s).unwrap()).unwrap_or_default();
    let git_ref = ctx["ref"].as_str().unwrap_or("main");
    let tag = ctx["tag"].as_bool().unwrap_or(false);
    let pipeline_source = ctx["source"].as_str().unwrap_or("push");
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("CI_DEFAULT_BRANCH".into(), "main".into());
    vars.insert("CI_PIPELINE_SOURCE".into(), pipeline_source.into());
    vars.insert("CI_COMMIT_REF_NAME".into(), git_ref.into());
    if tag {
        vars.insert("CI_COMMIT_TAG".into(), git_ref.into());
    } else if pipeline_source != "merge_request_event" {
        vars.insert("CI_COMMIT_BRANCH".into(), git_ref.into());
    }
    if let Some(m) = ctx["variables"].as_object() {
        for (k, v) in m {
            vars.insert(k.clone(), v.as_str().unwrap_or_default().into());
        }
    }
    let cfg = match forgust_ci_config::load(&source, ".gitlab-ci.yml", &Dir(dir.to_path_buf())).await {
        Ok(c) => c,
        Err(e) => return serde_json::json!({ "error": e.to_string() }),
    };
    let c =
        Context { vars: &vars, source: pipeline_source, git_ref, tag, changed: None, files: Some(&[]), changed_vs: None, files_in: None };
    match forgust_ci_config::plan::plan(&cfg, &c) {
        Err(e) => serde_json::json!({ "error": e.to_string() }),
        Ok(Outcome::Skip(_)) => serde_json::json!({ "skipped": true }),
        Ok(Outcome::Run(p)) => serde_json::json!({
            "jobs": p.jobs.iter().map(|j| serde_json::json!({
                "name": j.name, "stage": j.stage, "when": j.when, "allow_failure": j.allow_failure,
                "needs": j.needs, "script": j.job.script,
            })).collect::<Vec<_>>()
        }),
    }
}

fn check(case: &str, expected: &Value, got: &Value) -> Result<(), String> {
    if let Some(err) = expected["error"].as_str() {
        let msg = got["error"].as_str().ok_or_else(|| format!("{case}: expected an error mentioning `{err}`, got {got}"))?;
        return if msg.contains(err) { Ok(()) } else { Err(format!("{case}: error `{msg}` does not mention `{err}`")) };
    }
    if expected["skipped"] == true {
        return if got["skipped"] == true { Ok(()) } else { Err(format!("{case}: expected no pipeline, got {got}")) };
    }
    let want = expected["jobs"].as_array().unwrap();
    let have = got["jobs"].as_array().ok_or_else(|| format!("{case}: expected jobs, got {got}"))?;
    let names = |v: &Vec<Value>| {
        let mut n: Vec<String> = v.iter().map(|j| j["name"].as_str().unwrap().to_string()).collect();
        n.sort();
        n
    };
    if names(want) != names(have) {
        return Err(format!("{case}: jobs {:?}, expected {:?}", names(have), names(want)));
    }
    for w in want {
        let h = have.iter().find(|h| h["name"] == w["name"]).unwrap();
        for key in ["stage", "when", "allow_failure", "needs", "script"] {
            if !w[key].is_null() && w[key] != h[key] {
                return Err(format!("{case}: job `{}` {key} = {}, expected {}", w["name"], h[key], w[key]));
            }
        }
    }
    Ok(())
}

#[test]
fn corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci-conformance/cases");
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    cases.sort();
    assert!(cases.len() >= 10, "corpus missing at {}", root.display());
    let mut failures = Vec::new();
    for dir in &cases {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let expected: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap()).unwrap();
        let got = rt.block_on(run_case(dir));
        if let Err(e) = check(&name, &expected, &got) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}
