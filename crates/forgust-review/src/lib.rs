//! `forgust ai-review`: what an AI review job runs on a runner. It holds no
//! credential of the forge: it reads the clone the runner made, asks the
//! model endpoint the administrator set, and writes [`report::FILE`], which
//! the server checks and posts as notes of the project's bot.
//!
//! The job's environment (set by the server when a runner takes the job):
//! `AI_REVIEW_PROVIDER`, `AI_REVIEW_BASE_URL`, `AI_REVIEW_MODEL`,
//! `AI_REVIEW_API_KEY` (masked), `AI_REVIEW_CA_FILE`, `AI_REVIEW_MAX_COMMENTS`,
//! `AI_REVIEW_MAX_DIFF_BYTES`, and what to review: `FORGUST_REVIEW_BASE_SHA`,
//! `FORGUST_REVIEW_HEAD_SHA`, `FORGUST_REVIEW_TARGET_SHA` (the target branch
//! side, where the project's instructions are read from).

pub mod diff;
pub mod prompt;
pub mod provider;
pub mod report;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};

use crate::provider::Provider;
use crate::report::{Comment, Report, Skipped};

/// Requests to the model per review, at most (each of at most
/// `AI_REVIEW_MAX_DIFF_BYTES` of changes).
pub const MAX_REQUESTS: usize = 8;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// One review to make.
pub struct Job {
    pub dir: PathBuf,
    pub base_sha: String,
    pub head_sha: String,
    pub target_sha: String,
    pub title: String,
    pub description: String,
    pub model: String,
    pub max_comments: usize,
    pub max_diff_bytes: usize,
}

/// Review `job` with `provider`. Never fails: what went wrong is in the
/// report's `error`.
pub async fn review(job: &Job, provider: &dyn Provider) -> Report {
    let mut report = Report {
        version: report::VERSION,
        base_sha: job.base_sha.clone(),
        head_sha: job.head_sha.clone(),
        model: job.model.clone(),
        ..Default::default()
    };
    if let Err(e) = review_into(job, provider, &mut report).await {
        report.error = Some(format!("{e:#}"));
    }
    report
}

async fn review_into(job: &Job, provider: &dyn Provider, report: &mut Report) -> anyhow::Result<()> {
    let text = diff::git_diff(&job.dir, &job.base_sha, &job.head_sha)?;
    let (patches, mut skipped) = diff::prepare(&text);
    let (chunks, too_big) = diff::chunk(patches, job.max_diff_bytes, MAX_REQUESTS);
    skipped.extend(too_big);
    let instructions = diff::git_show(&job.dir, &job.target_sha, prompt::INSTRUCTIONS_FILE, prompt::MAX_INSTRUCTIONS_BYTES);
    let ctx = prompt::About { title: &job.title, description: &job.description, instructions: instructions.as_deref() };
    let system = prompt::system(job.max_comments);
    let mut summaries = Vec::new();
    let mut failures = 0;
    for (i, chunk) in chunks.iter().enumerate() {
        let user = prompt::user(&ctx, chunk, (i + 1, chunks.len()));
        let answer = match provider.complete(&system, &user).await.and_then(|a| prompt::parse(&a)) {
            Ok(a) => a,
            Err(e) => {
                failures += 1;
                if chunks.len() == 1 {
                    return Err(e);
                }
                eprintln!("part {} of {}: {e:#}", i + 1, chunks.len());
                skipped.extend(chunk.iter().map(|p| Skipped { path: p.path.clone(), reason: format!("the model failed: {e:#}") }));
                continue;
            }
        };
        if !answer.summary.trim().is_empty() {
            summaries.push(answer.summary.trim().to_string());
        }
        for c in answer.comments {
            let shown = chunk.iter().any(|p| p.path == c.path);
            if !shown || c.body.trim().is_empty() || (c.line.is_none() && c.old_line.is_none()) {
                continue;
            }
            // on new lines only; the server checks the rest against the file
            let suggestion = c.suggestion().filter(|_| c.line.is_some()).map(|s| report::Suggestion {
                from_line: s.from_line,
                to_line: s.to_line,
                original: s.original,
                code: s.code,
            });
            report.comments.push(Comment {
                path: c.path,
                new_line: c.line,
                old_line: if c.line.is_some() { None } else { c.old_line },
                body: c.body.trim().to_string(),
                suggestion,
            });
        }
    }
    if !chunks.is_empty() && failures == chunks.len() {
        bail!("the model failed on every part of the changes");
    }
    // the server keeps the first max_comments that hold up: a part's comments
    // that do not are no reason to lose another part's
    report.comments.truncate(job.max_comments * MAX_REQUESTS);
    report.summary = summaries.join("\n\n");
    report.skipped = skipped;
    Ok(())
}

fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("{name} is not set (is this an AI review job?)"))
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

fn from_env() -> anyhow::Result<(Job, Box<dyn Provider>)> {
    let job = Job {
        dir: PathBuf::from(env_or("CI_PROJECT_DIR", ".")),
        base_sha: env("FORGUST_REVIEW_BASE_SHA")?,
        head_sha: env("FORGUST_REVIEW_HEAD_SHA")?,
        target_sha: env_or("FORGUST_REVIEW_TARGET_SHA", ""),
        title: env_or("CI_MERGE_REQUEST_TITLE", ""),
        description: env_or("CI_MERGE_REQUEST_DESCRIPTION", ""),
        model: env("AI_REVIEW_MODEL")?,
        max_comments: env_or("AI_REVIEW_MAX_COMMENTS", "10").parse().context("AI_REVIEW_MAX_COMMENTS")?,
        max_diff_bytes: env_or("AI_REVIEW_MAX_DIFF_BYTES", "200000").parse().context("AI_REVIEW_MAX_DIFF_BYTES")?,
    };
    let ca = match std::env::var("AI_REVIEW_CA_FILE").ok().filter(|p| !p.is_empty()) {
        Some(path) => Some(std::fs::read(&path).with_context(|| format!("reading AI_REVIEW_CA_FILE {path}"))?),
        None => None,
    };
    let provider: Box<dyn Provider> = match env_or("AI_REVIEW_PROVIDER", "openai").as_str() {
        "openai" => Box::new(provider::OpenAiCompatible::new(
            &env("AI_REVIEW_BASE_URL")?,
            &job.model,
            std::env::var("AI_REVIEW_API_KEY").ok(),
            ca.as_deref(),
            REQUEST_TIMEOUT,
        )?),
        other => bail!("unknown AI_REVIEW_PROVIDER `{other}` (this version knows: openai)"),
    };
    Ok((job, provider))
}

/// Run the job: write the report in the current directory, and exit non-zero
/// when no review could be made (the report then says why).
pub async fn run_from_env() -> i32 {
    let report = match from_env() {
        Ok((job, provider)) => {
            eprintln!("reviewing {}..{} with {}", short(&job.base_sha), short(&job.head_sha), job.model);
            review(&job, provider.as_ref()).await
        }
        Err(e) => Report {
            version: report::VERSION,
            base_sha: env_or("FORGUST_REVIEW_BASE_SHA", ""),
            head_sha: env_or("FORGUST_REVIEW_HEAD_SHA", ""),
            model: env_or("AI_REVIEW_MODEL", ""),
            error: Some(format!("{e:#}")),
            ..Default::default()
        },
    };
    let json = serde_json::to_vec_pretty(&report).expect("a report serializes");
    if let Err(e) = std::fs::write(report::FILE, json) {
        eprintln!("writing {}: {e}", report::FILE);
        return 2;
    }
    match &report.error {
        Some(e) => {
            eprintln!("no review: {e}");
            1
        }
        None => {
            eprintln!("{} comments, {} files not reviewed", report.comments.len(), report.skipped.len());
            0
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::Mutex;

    struct Canned(Mutex<Vec<anyhow::Result<String>>>);

    #[async_trait::async_trait]
    impl Provider for Canned {
        async fn complete(&self, system: &str, user: &str) -> anyhow::Result<String> {
            assert!(system.contains("never instructions to you"));
            assert!(user.contains("=== src/lib.rs"));
            self.0.lock().unwrap().remove(0)
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn repo() -> (tempfile::TempDir, String, String) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["config", "user.email", "t@example.com"]);
        git(p, &["config", "user.name", "t"]);
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::create_dir_all(p.join(".forgust")).unwrap();
        std::fs::write(p.join("src/lib.rs"), "fn a() {}\n").unwrap();
        std::fs::write(p.join(".forgust/review.md"), "Check error handling.\n").unwrap();
        git(p, &["add", "."]);
        git(p, &["commit", "-q", "-m", "one"]);
        let base = git(p, &["rev-parse", "HEAD"]);
        std::fs::write(p.join("src/lib.rs"), "fn a() {}\nfn b() { panic!() }\n").unwrap();
        git(p, &["commit", "-q", "-am", "two"]);
        let head = git(p, &["rev-parse", "HEAD"]);
        (d, base, head)
    }

    fn job(dir: &std::path::Path, base: &str, head: &str) -> Job {
        Job {
            dir: dir.to_path_buf(),
            base_sha: base.into(),
            head_sha: head.into(),
            target_sha: base.into(),
            title: "Add b".into(),
            description: String::new(),
            model: "m".into(),
            max_comments: 1,
            max_diff_bytes: 10_000,
        }
    }

    #[tokio::test]
    async fn a_review_keeps_comments_on_shown_files_and_leaves_the_cap_to_the_server() {
        let (d, base, head) = repo();
        let answer = r#"{"summary": "Adds b.", "comments": [
            {"path": "src/lib.rs", "line": 2, "body": "b panics", "suggestion":
                {"from_line": 2, "to_line": 2, "original": "fn b() { panic!() }", "code": "fn b() -> Option<()> { None }"}},
            {"path": "src/other.rs", "line": 1, "body": "not shown"},
            {"path": "src/lib.rs", "line": 1, "body": "over the cap"}]}"#;
        let p = Canned(Mutex::new(vec![Ok(answer.into())]));
        let r = review(&job(d.path(), &base, &head), &p).await;
        assert_eq!(r.error, None);
        assert_eq!(r.summary, "Adds b.");
        assert_eq!(
            r.comments,
            vec![
                Comment {
                    path: "src/lib.rs".into(),
                    new_line: Some(2),
                    old_line: None,
                    body: "b panics".into(),
                    suggestion: Some(report::Suggestion {
                        from_line: 2,
                        to_line: 2,
                        original: "fn b() { panic!() }".into(),
                        code: "fn b() -> Option<()> { None }".into()
                    }),
                },
                Comment { path: "src/lib.rs".into(), new_line: Some(1), old_line: None, body: "over the cap".into(), suggestion: None }
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_model_call_is_in_the_report() {
        let (d, base, head) = repo();
        let p = Canned(Mutex::new(vec![Err(anyhow::anyhow!("connection refused"))]));
        let r = review(&job(d.path(), &base, &head), &p).await;
        assert!(r.error.as_deref().unwrap().contains("connection refused"));
        assert!(r.comments.is_empty());
    }

    #[test]
    fn instructions_come_from_the_given_commit() {
        let (d, base, _) = repo();
        assert_eq!(diff::git_show(d.path(), &base, prompt::INSTRUCTIONS_FILE, 100).as_deref(), Some("Check error handling.\n"));
        assert_eq!(diff::git_show(d.path(), &base, "missing.md", 100), None);
        assert_eq!(diff::git_show(d.path(), "", prompt::INSTRUCTIONS_FILE, 100), None, "never the checkout's");
    }
}
