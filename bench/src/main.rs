//! Forgust benchmark harness (ADR-0008).
//!
//! `seed` pushes a fixture repo into a project, `refs` adds N refs to a project's
//! repo on disk (the 1M-ref fixture), `run` measures the targets under the single
//! and concurrent profiles and compares p95 against a stored baseline.
//!
//! HTTP timings are measured by this client over loopback, a close proxy for the
//! server-side span timings the ADR names. Numbers from a non-dedicated machine are
//! informative only.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "forgust-bench")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a fixture repo with `files` files of `lines` lines and `commits` commits.
    Fixture {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 2000)]
        files: usize,
        #[arg(long, default_value_t = 200)]
        commits: usize,
        #[arg(long, default_value_t = 2000)]
        lines: usize,
    },
    /// Write `count` branch refs into a project's bare repo (packed-refs), all pointing at HEAD.
    Refs {
        /// Path of the bare repo on disk (see `forgust` storage layout).
        #[arg(long)]
        repo: PathBuf,
        #[arg(long, default_value_t = 1_000_000)]
        count: usize,
    },
    /// Measure the ADR-0008 targets.
    Run(Box<RunArgs>),
}

#[derive(Parser)]
struct RunArgs {
    /// Base URL, e.g. http://127.0.0.1:8080
    #[arg(long)]
    url: String,
    /// Personal access token with `api` + `read_repository` scope.
    #[arg(long, env = "FORGUST_BENCH_TOKEN")]
    token: String,
    /// Project path with a typical repo (tree, blob, commit, blame targets).
    #[arg(long)]
    project: String,
    /// File (≈2,000 lines) for the blob and blame targets.
    #[arg(long)]
    file: String,
    /// Project path holding the 1M-ref repo (optional).
    #[arg(long)]
    refs_project: Option<String>,
    /// Merge request `<project path>!<iid>` with ~1,000 changed files (optional).
    #[arg(long)]
    mr: Option<String>,
    #[arg(long, default_value_t = 200)]
    iterations: usize,
    #[arg(long, default_value_t = 20)]
    warmup: usize,
    #[arg(long, default_value_t = 32)]
    clients: usize,
    #[arg(long, default_value_t = 20)]
    concurrent_secs: u64,
    /// Results file to write.
    #[arg(long, default_value = "bench/results/latest.json")]
    out: PathBuf,
    /// Baseline to compare against (regression gate).
    #[arg(long)]
    baseline: Option<PathBuf>,
    /// Write the results as the new baseline.
    #[arg(long)]
    update_baseline: bool,
    /// Allowed p95 regression over the baseline.
    #[arg(long, default_value_t = 0.15)]
    threshold: f64,
    /// Ignore p95 differences smaller than this (timer and scheduler noise).
    #[arg(long, default_value_t = 5.0)]
    min_delta_ms: f64,
    /// Free text describing the machine (see bench/HOST.md).
    #[arg(long, default_value = "unspecified (informative only)")]
    host: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Measurement {
    name: String,
    profile: String,
    n: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
    /// First request after the server started serving this target (cache cold).
    first_ms: Option<f64>,
    target_p95_ms: Option<f64>,
}

#[derive(Serialize, Deserialize)]
struct Report {
    host: String,
    created_at: String,
    measurements: Vec<Measurement>,
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn summarize(name: &str, profile: &str, mut ms: Vec<f64>, first: Option<f64>, target: Option<f64>) -> Measurement {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Measurement {
        name: name.into(),
        profile: profile.into(),
        n: ms.len(),
        p50_ms: pct(&ms, 0.5),
        p95_ms: pct(&ms, 0.95),
        max_ms: ms.last().copied().unwrap_or(0.0),
        first_ms: first,
        target_p95_ms: target,
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Bench")
        .env("GIT_AUTHOR_EMAIL", "bench@example.com")
        .env("GIT_COMMITTER_NAME", "Bench")
        .env("GIT_COMMITTER_EMAIL", "bench@example.com")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !out.status.success() {
        bail!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn fixture(out: &Path, files: usize, commits: usize, lines: usize) -> Result<()> {
    std::fs::create_dir_all(out)?;
    git(out, &["init", "-q", "-b", "main"])?;
    let dirs = ["src", "src/core", "src/net", "docs", "tests"];
    for d in dirs {
        std::fs::create_dir_all(out.join(d))?;
    }
    let body = |seed: usize, n: usize| -> String {
        (0..n).map(|i| format!("fn f_{seed}_{i}(x: u64) -> u64 {{ x.wrapping_mul({}) ^ {} }}\n", i + 1, seed)).collect()
    };
    for f in 0..files {
        let d = dirs[f % dirs.len()];
        let n = if f == 0 { lines } else { 40 };
        std::fs::write(out.join(format!("{d}/file_{f:05}.rs")), body(f, n))?;
    }
    std::fs::write(out.join("README.md"), "# Bench fixture\n\nGenerated by forgust-bench.\n")?;
    git(out, &["add", "-A"])?;
    git(out, &["commit", "-q", "-m", "fixture: initial"])?;
    for c in 1..commits {
        // touch a handful of files per commit so history, diffs and blame have depth
        for k in 0..5 {
            let f = (c * 7 + k * 13) % files;
            let d = dirs[f % dirs.len()];
            let p = out.join(format!("{d}/file_{f:05}.rs"));
            let mut s = std::fs::read_to_string(&p)?;
            s.push_str(&format!("// change {c}.{k}\n"));
            std::fs::write(&p, s)?;
        }
        git(out, &["commit", "-q", "-am", &format!("fixture: change {c}")])?;
    }
    println!("fixture written to {} (main file: src/file_00000.rs, {lines} lines)", out.display());
    Ok(())
}

/// Append `count` refs to packed-refs (writing loose refs one by one is far too slow).
fn many_refs(repo: &Path, count: usize) -> Result<()> {
    let head = git(repo, &["rev-parse", "HEAD"]).context("the repo needs at least one commit")?;
    let packed = repo.join("packed-refs");
    let existing = std::fs::read_to_string(&packed).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().filter(|l| !l.starts_with('#')).map(str::to_string).collect();
    for i in 0..count {
        lines.push(format!("{head} refs/heads/bench/{i:07}"));
    }
    lines.sort_by(|a, b| a.split(' ').nth(1).cmp(&b.split(' ').nth(1)));
    lines.dedup_by(|a, b| a.split(' ').nth(1) == b.split(' ').nth(1));
    let mut out = String::from("# pack-refs with: peeled fully-peeled sorted \n");
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    std::fs::write(&packed, out)?;
    println!("wrote {count} refs to {}", packed.display());
    Ok(())
}

struct Target {
    name: &'static str,
    path: String,
    target_single: f64,
    target_concurrent: f64,
}

async fn time_get(client: &reqwest::Client, url: &str, token: &str) -> Result<f64> {
    let start = Instant::now();
    let r = client.get(url).header("private-token", token).send().await?;
    let status = r.status();
    let _ = r.bytes().await?;
    if !status.is_success() {
        bail!("GET {url} -> {status}");
    }
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

fn time_cmd(dir: &Path, args: &[&str]) -> Result<f64> {
    let start = Instant::now();
    git(dir, args)?;
    Ok(start.elapsed().as_secs_f64() * 1000.0)
}

async fn run(a: RunArgs) -> Result<()> {
    let client = reqwest::Client::builder().pool_max_idle_per_host(a.clients).build()?;
    let enc = |s: &str| s.replace('/', "%2F");
    let api = format!("{}/api/v1/projects/{}", a.url, enc(&a.project));
    let detail: serde_json::Value = client.get(&api).header("private-token", &a.token).send().await?.error_for_status()?.json().await?;
    let branch = detail["default_branch"].as_str().unwrap_or("main").to_string();
    let log: serde_json::Value = client
        .get(format!("{api}/repository/commits?ref={branch}&per_page=5"))
        .header("private-token", &a.token)
        .send()
        .await?
        .json()
        .await?;
    let sha = log[1]["id"].as_str().or(log[0]["id"].as_str()).context("project has no commits")?.to_string();

    // (name, path, ADR-0008 single-warm target, concurrent-warm target)
    let targets = vec![
        Target {
            name: "repo_home_tree",
            path: format!("{api}/repository/tree?ref={branch}"),
            target_single: 50.0,
            target_concurrent: 150.0,
        },
        Target {
            name: "repo_home_readme",
            path: format!("{api}/repository/readme?ref={branch}"),
            target_single: 50.0,
            target_concurrent: 150.0,
        },
        Target {
            name: "blob_highlighted",
            path: format!("{api}/repository/blob?ref={branch}&path={}", a.file),
            target_single: 50.0,
            target_concurrent: 150.0,
        },
        Target {
            name: "commit_diff",
            path: format!("{api}/repository/commits/{sha}/diff"),
            target_single: 100.0,
            target_concurrent: 250.0,
        },
        Target {
            name: "blame",
            path: format!("{api}/repository/blame?ref={branch}&path={}", a.file),
            target_single: 100.0,
            target_concurrent: 300.0,
        },
        Target {
            name: "commit_list",
            path: format!("{api}/repository/commits?ref={branch}"),
            target_single: 100.0,
            target_concurrent: 250.0,
        },
    ];

    let mut targets = targets;
    if let Some(spec) = &a.mr {
        let (mp, iid) = spec.split_once('!').context("--mr is <project>!<iid>")?;
        let mr_api = format!("{}/api/v1/projects/{}/merge_requests/{iid}", a.url, enc(mp));
        targets.push(Target { name: "mr_page", path: mr_api.clone(), target_single: 100.0, target_concurrent: 250.0 });
        targets.push(Target { name: "mr_diff_first_page", path: format!("{mr_api}/diff"), target_single: 150.0, target_concurrent: 400.0 });
    }
    let mut results = Vec::new();
    // single client: first request (cold-ish), warm-up, then measured iterations
    for t in &targets {
        let first = time_get(&client, &t.path, &a.token).await?;
        for _ in 0..a.warmup {
            time_get(&client, &t.path, &a.token).await?;
        }
        let mut ms = Vec::with_capacity(a.iterations);
        for _ in 0..a.iterations {
            ms.push(time_get(&client, &t.path, &a.token).await?);
        }
        let m = summarize(t.name, "single", ms, Some(first), Some(t.target_single));
        println!(
            "{:<22} single      p50 {:>7.2} ms  p95 {:>7.2} ms  (first {:.1} ms, target {} ms)",
            m.name, m.p50_ms, m.p95_ms, first, t.target_single
        );
        results.push(m);
    }
    // concurrent: N clients hammering each target for a fixed time
    for t in &targets {
        let deadline = Instant::now() + Duration::from_secs(a.concurrent_secs);
        let mut tasks = Vec::new();
        for _ in 0..a.clients {
            let (client, url, token) = (client.clone(), t.path.clone(), a.token.clone());
            tasks.push(tokio::spawn(async move {
                let mut v = Vec::new();
                while Instant::now() < deadline {
                    if let Ok(ms) = time_get(&client, &url, &token).await {
                        v.push(ms);
                    }
                }
                v
            }));
        }
        let mut all = Vec::new();
        for task in tasks {
            all.extend(task.await?);
        }
        let m = summarize(t.name, "concurrent", all, None, Some(t.target_concurrent));
        println!(
            "{:<22} concurrent  p50 {:>7.2} ms  p95 {:>7.2} ms  (n={}, target {} ms)",
            m.name, m.p50_ms, m.p95_ms, m.n, t.target_concurrent
        );
        results.push(m);
    }

    // the 1M-ref repo: ls-refs with a prefix, no-op fetch, one-branch push
    if let Some(rp) = &a.refs_project {
        let work = std::env::temp_dir().join(format!("forgust-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let host = a.url.split("://").nth(1).unwrap_or(&a.url);
        let scheme = a.url.split("://").next().unwrap_or("http");
        let remote = format!("{scheme}://bench:{}@{host}/{rp}.git", a.token);
        git(&work, &["init", "-q"])?;
        git(&work, &["-c", "protocol.version=2", "fetch", "-q", &remote, "main:refs/remotes/o/main"])?;
        let mut ls = Vec::new();
        let mut fetch = Vec::new();
        let mut push = Vec::new();
        let ls_url = format!("{}/{rp}.git/git-upload-pack", a.url);
        // protocol v2 ls-refs with a ref-prefix that matches 10 refs (bench/0000010-19);
        // `git ls-remote <pattern>` would list every ref and filter client-side
        let body =
            [pkt("command=ls-refs\n"), b"0001".to_vec(), pkt("peel\n"), pkt("ref-prefix refs/heads/bench/000001\n"), b"0000".to_vec()]
                .concat();
        for i in 0..20 {
            let start = Instant::now();
            let r = client
                .post(&ls_url)
                .basic_auth("bench", Some(&a.token))
                .header("git-protocol", "version=2")
                .header("content-type", "application/x-git-upload-pack-request")
                .body(body.clone())
                .send()
                .await?
                .bytes()
                .await?;
            ls.push(start.elapsed().as_secs_f64() * 1000.0);
            if i == 0 {
                let n = String::from_utf8_lossy(&r).matches("refs/heads/bench/").count();
                anyhow::ensure!(n == 10, "ls-refs returned {n} refs, expected 10");
            }
            fetch.push(time_cmd(&work, &["-c", "protocol.version=2", "fetch", "-q", &remote, "main:refs/remotes/o/main"])?);
            git(&work, &["checkout", "-q", "-B", "work", "refs/remotes/o/main"])?;
            git(&work, &["commit", "-q", "--allow-empty", "-m", &format!("bench push {i}")])?;
            push.push(time_cmd(&work, &["push", "-q", &remote, &format!("HEAD:refs/heads/bench-push/{}-{i}", std::process::id())])?);
        }
        for (name, v, target) in
            [("refs_1m_ls_refs_prefix", ls, 100.0), ("refs_1m_fetch_noop", fetch, 300.0), ("refs_1m_push_one", push, 300.0)]
        {
            let m = summarize(name, "single", v, None, Some(target));
            println!(
                "{:<22} single      p50 {:>7.2} ms  p95 {:>7.2} ms  (target {} ms; git commands include client startup)",
                m.name, m.p50_ms, m.p95_ms, target
            );
            results.push(m);
        }
        let _ = std::fs::remove_dir_all(&work);
    }

    let report = Report { host: a.host.clone(), created_at: chrono_now(), measurements: results };
    if let Some(dir) = a.out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&a.out, serde_json::to_vec_pretty(&report)?)?;
    println!("results written to {}", a.out.display());

    let mut failed = false;
    if let Some(base) = &a.baseline {
        if base.exists() && !a.update_baseline {
            let baseline: Report = serde_json::from_slice(&std::fs::read(base)?)?;
            for m in &report.measurements {
                if let Some(b) = baseline.measurements.iter().find(|b| b.name == m.name && b.profile == m.profile) {
                    let limit = (b.p95_ms * (1.0 + a.threshold)).max(b.p95_ms + a.min_delta_ms);
                    let verdict = if m.p95_ms > limit {
                        failed = true;
                        "REGRESSION"
                    } else {
                        "ok"
                    };
                    println!("{:<22} {:<10} p95 {:>7.2} ms vs baseline {:>7.2} ms  {verdict}", m.name, m.profile, m.p95_ms, b.p95_ms);
                }
            }
        }
        if a.update_baseline || !base.exists() {
            if let Some(dir) = base.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(base, serde_json::to_vec_pretty(&report)?)?;
            println!("baseline written to {}", base.display());
        }
    }
    if failed {
        bail!("p95 regressed more than {:.0}% over the baseline", a.threshold * 100.0);
    }
    Ok(())
}

fn pkt(s: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", s.len() + 4).into_bytes();
    v.extend_from_slice(s.as_bytes());
    v
}

fn chrono_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{secs}")
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Fixture { out, files, commits, lines } => fixture(&out, files, commits, lines),
        Cmd::Refs { repo, count } => many_refs(&repo, count),
        Cmd::Run(a) => run(*a).await,
    }
}
