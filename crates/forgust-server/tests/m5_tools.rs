//! M5 exit: the real Renovate and the real glab against Forgust, not just the
//! API calls read from their sources (`m5_api_v4.rs`).
//!
//! Opt in with `FORGUST_TEST_RENOVATE=renovate/renovate:<version>` (needs Docker
//! and access to the npm registry) and `FORGUST_TEST_GLAB=<path to glab>`. As in
//! `m4_runner.rs`, set `FORGUST_TEST_RUNNER_NETWORK=container:<this container>`
//! when the test runs inside a container that drives the engine's socket.

mod common;

use std::path::Path;

use common::*;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

async fn v4(c: &Client, path: &str) -> Value {
    c.ok_get(&format!("/api/v4{path}")).await
}

/// One Renovate run over the project; returns its log. Fails on a non-zero exit
/// or any ERROR line.
async fn renovate(image: &str, srv: &TestServer, token: &str, run: usize) -> String {
    let env = [
        ("RENOVATE_PLATFORM", "gitlab".to_string()),
        ("RENOVATE_ENDPOINT", format!("{}/api/v4/", srv.base)),
        ("RENOVATE_TOKEN", token.to_string()),
        ("RENOVATE_REPOSITORIES", REPO.to_string()),
        ("RENOVATE_ONBOARDING", "false".into()),
        ("RENOVATE_REQUIRE_CONFIG", "optional".into()),
        ("RENOVATE_DEPENDENCY_DASHBOARD", "true".into()),
        ("RENOVATE_PR_HOURLY_LIMIT", "0".into()),
        ("RENOVATE_GIT_AUTHOR", "Renovate Bot <renovate@example.com>".into()),
    ];
    run_renovate(image, &env.map(|(k, v)| (k.to_string(), v)), run).await
}

/// The Renovate image with `env` (plus debug logs), as a runner would run it.
async fn run_renovate(image: &str, env: &[(String, String)], run: usize) -> String {
    let network = std::env::var("FORGUST_TEST_RUNNER_NETWORK").unwrap_or_else(|_| "host".into());
    let name = format!("forgust-test-renovate-{}-{run}", std::process::id());
    let _guard = Container(name.clone());
    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(["run", "--name", &name, "--network", &network]);
    for (k, v) in env {
        cmd.args(["-e", &format!("{k}={v}")]);
    }
    let out = cmd.args(["-e", "LOG_LEVEL=debug"]).arg(image).output().await.unwrap();
    let log = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let errors: Vec<&str> = log.lines().filter(|l| l.contains(" ERROR:") || l.contains("\"level\":50")).collect();
    if !out.status.success() || !errors.is_empty() {
        let tail: Vec<&str> = log.lines().rev().take(250).collect::<Vec<_>>().into_iter().rev().collect();
        panic!("renovate run {run} failed ({}):\n{}\n--- errors ---\n{}", out.status, tail.join("\n"), errors.join("\n"));
    }
    log
}

async fn open_renovate_mrs(c: &Client, project: i64) -> Vec<Value> {
    v4(c, &format!("/projects/{project}/merge_requests?state=opened&per_page=100"))
        .await
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source_branch"].as_str().unwrap_or("").starts_with("renovate/"))
        .cloned()
        .collect()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn real_renovate_opens_updates_and_closes_merge_requests(pool: sqlx::PgPool) {
    let Ok(image) = std::env::var("FORGUST_TEST_RENOVATE") else {
        eprintln!("skipped: set FORGUST_TEST_RENOVATE=renovate/renovate:<version> to run the real Renovate");
        return;
    };
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let token = owner.ok_post("/api/v1/user/tokens", json!({ "name": "renovate", "scopes": ["api", "write_repository"] })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let api = Client::with_token(&srv.base, &token);

    let work = srv.work_dir("renovate");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join("package.json"), r#"{ "name": "app", "version": "1.0.0", "dependencies": { "left-pad": "1.0.0" } }"#).unwrap();
    std::fs::write(work.join("README.md"), "app\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "init"]).await);
    let url = srv.http_url("olivia", REPO);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;

    // 1. a first run opens the update and the Dependency Dashboard
    renovate(&image, &srv, &token, 1).await;
    wait_jobs(&srv.state).await;
    let mrs = open_renovate_mrs(&api, project).await;
    assert_eq!(mrs.len(), 1, "one update MR: {mrs:?}");
    let mr = &mrs[0];
    assert!(mr["title"].as_str().unwrap().contains("left-pad"), "{mr}");
    let iid = mr["iid"].as_i64().unwrap();
    let branch = mr["source_branch"].as_str().unwrap().to_string();
    let issues = v4(&api, &format!("/projects/{project}/issues?state=opened")).await;
    assert!(issues.as_array().unwrap().iter().any(|i| i["title"] == "Dependency Dashboard"), "dashboard issue: {issues}");

    // 2. main moves on and the rebase box is ticked: the branch is rebased onto
    //    main, and no second MR appears
    commit_file(&work, "README.md", "app, moved on\n", "docs").await;
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    let main_head = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    let desc = v4(&api, &format!("/projects/{project}/merge_requests/{iid}")).await["description"].as_str().unwrap().to_string();
    assert!(desc.contains("<!-- rebase-check -->"), "Renovate's rebase checkbox: {desc}");
    let ticked = desc.replace("- [ ] <!-- rebase-check -->", "- [x] <!-- rebase-check -->");
    let (s, v) = api.put(&format!("/api/v4/projects/{project}/merge_requests/{iid}"), json!({ "description": ticked })).await;
    assert_eq!(s, 200, "{v}");
    renovate(&image, &srv, &token, 2).await;
    wait_jobs(&srv.state).await;
    let mrs = open_renovate_mrs(&api, project).await;
    assert_eq!(mrs.len(), 1, "still one update MR: {mrs:?}");
    must(git(&work, &["fetch", "-q", &url, &branch]).await);
    let parent = must(git(&work, &["rev-parse", "FETCH_HEAD^"]).await).trim().to_string();
    assert_eq!(parent, main_head, "the update branch was rebased onto main");

    // 3. the dependency is updated on main by hand: Renovate closes its MR
    std::fs::write(work.join("package.json"), r#"{ "name": "app", "version": "1.0.0", "dependencies": { "left-pad": "1.3.0" } }"#).unwrap();
    must(git(&work, &["commit", "-qam", "left-pad 1.3.0"]).await);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    renovate(&image, &srv, &token, 3).await;
    wait_jobs(&srv.state).await;
    let mr = v4(&api, &format!("/projects/{project}/merge_requests/{iid}")).await;
    assert_eq!(mr["state"], "closed", "the obsolete MR is closed: {mr}");
}

/// Built-in Renovate: the job Forgust hands a runner carries all
/// Renovate needs. Its variables, given to the real image, onboard the project
/// as the project's bot; the run's token goes with the run.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn real_built_in_renovate_onboards_a_project(pool: sqlx::PgPool) {
    let Ok(image) = std::env::var("FORGUST_TEST_RENOVATE") else {
        eprintln!("skipped: set FORGUST_TEST_RENOVATE=renovate/renovate:<version> to run the real Renovate");
        return;
    };
    let img = image.clone();
    let srv = TestServer::start_with(pool, move |c| {
        c.ci.long_poll_secs = 1;
        c.renovate.image = img;
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let work = srv.work_dir("built-in");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join("package.json"), r#"{ "name": "app", "version": "1.0.0", "dependencies": { "left-pad": "1.0.0" } }"#).unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "init"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    // Renovate jobs go to instance runners only
    let admin = srv.user("root", true).await;
    let runner = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared" })).await["token"].as_str().unwrap().to_string();

    let (s, st) = owner.put(&format!("/api/v1/projects/{project}/renovate"), json!({ "enabled": true })).await;
    assert_eq!(s, 200, "{st}");
    owner.ok_post(&format!("/api/v1/projects/{project}/renovate/run"), json!({})).await;
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", srv.base))
        .json(&json!({ "token": runner, "system_id": "s_tools", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201, "the Renovate job is handed out");
    let job = r.json::<Value>().await.unwrap();
    assert_eq!(job["image"]["name"], image.as_str());
    let env: Vec<(String, String)> = job["variables"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["key"].as_str().unwrap().starts_with("RENOVATE_"))
        // a runner expands variables that are not raw (`$CI_PROJECT_DIR`: the
        // build directory, writable by the job); here, a directory in the container
        .map(|v| {
            let value = v["value"].as_str().unwrap().to_string();
            let value = if v["raw"] == true { value } else { value.replace("$CI_PROJECT_DIR", "/tmp/build") };
            (v["key"].as_str().unwrap().to_string(), value)
        })
        .collect();
    assert!(env.iter().any(|(k, v)| k == "RENOVATE_CACHE_DIR" && v == "/tmp/build/.renovate-cache"), "{env:?}");
    run_renovate(&image, &env, 10).await;
    wait_jobs(&srv.state).await;

    let bot = format!("renovate-bot-{project}");
    let mrs = owner.ok_get(&format!("/api/v1/projects/{project}/merge_requests?state=opened")).await;
    let onboarding = mrs
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source_branch"] == "renovate/configure")
        .unwrap_or_else(|| panic!("onboarding MR: {mrs}"));
    assert_eq!(onboarding["author_username"], bot.as_str(), "{onboarding}");

    // the run ends: its token goes
    let token = env.iter().find(|(k, _)| k == "RENOVATE_TOKEN").unwrap().1.clone();
    let r = reqwest::Client::new()
        .put(format!("{}/api/v4/jobs/{}", srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": "success", "exit_code": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    wait_jobs(&srv.state).await;
    assert_eq!(Client::with_token(&srv.base, &token).get("/api/v4/user").await.0, 401);
}

/// Run glab in `dir`; panics with its output when it fails.
async fn glab(bin: &str, config: &Path, dir: &Path, args: &[&str]) -> String {
    let out = tokio::process::Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GLAB_CONFIG_DIR", config)
        .env("NO_COLOR", "1")
        .env("GLAB_SEND_TELEMETRY", "false")
        .env("GLAB_CHECK_UPDATE", "false")
        .env_remove("GITLAB_TOKEN")
        .env_remove("GITLAB_HOST")
        .output()
        .await
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "glab {} failed:\n{text}", args.join(" "));
    text
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn real_glab_drives_merge_requests_issues_pipelines_and_releases(pool: sqlx::PgPool) {
    let Ok(bin) = std::env::var("FORGUST_TEST_GLAB") else {
        eprintln!("skipped: set FORGUST_TEST_GLAB=<path to glab> to run the real glab");
        return;
    };
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let token = owner.ok_post("/api/v1/user/tokens", json!({ "name": "glab", "scopes": ["api", "write_repository"] })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let api = Client::with_token(&srv.base, &token);
    let host = srv.host();
    let config = srv.work_dir("glab-config");
    std::fs::write(
        config.join("config.yml"),
        format!(
            "git_protocol: http\ncheck_update: false\nno_prompt: true\ntelemetry: false\nhosts:\n  {host}:\n    token: {token}\n    api_host: {host}\n    git_protocol: http\n    api_protocol: http\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config.join("config.yml"), std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let work = srv.work_dir("glab");
    seed_repo(&work).await;
    std::fs::write(work.join(".gitlab-ci.yml"), "test:\n  script: [echo hi]\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "ci"]).await);
    let url = srv.http_url("olivia", REPO);
    must(git(&work, &["remote", "add", "origin", &format!("http://{host}/{REPO}.git")]).await);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;

    let view = glab(&bin, &config, &work, &["repo", "view", REPO]).await;
    assert!(view.contains("API") || view.contains("api"), "{view}");

    // merge requests
    must(git(&work, &["checkout", "-q", "-b", "feature"]).await);
    commit_file(&work, "feature.txt", "new\n", "Add feature").await;
    must(git(&work, &["push", "-q", &url, "feature"]).await);
    wait_jobs(&srv.state).await;
    let created = glab(
        &bin,
        &config,
        &work,
        &[
            "mr",
            "create",
            "--source-branch",
            "feature",
            "--target-branch",
            "main",
            "--title",
            "Add feature",
            "--description",
            "From glab",
            "--yes",
        ],
    )
    .await;
    assert!(created.contains("/-/merge_requests/1"), "{created}");
    let list = glab(&bin, &config, &work, &["mr", "list"]).await;
    assert!(list.contains("Add feature"), "{list}");
    let shown = glab(&bin, &config, &work, &["mr", "view", "1"]).await;
    assert!(shown.contains("From glab"), "{shown}");
    glab(&bin, &config, &work, &["mr", "note", "1", "-m", "Looks good"]).await;
    let notes = v4(&api, &format!("/projects/{project}/merge_requests/1/notes")).await;
    assert!(notes.as_array().unwrap().iter().any(|n| n["body"] == "Looks good"), "{notes}");
    // a pipeline is pending (no runner): glab asks for auto-merge, as on GitLab
    glab(&bin, &config, &work, &["mr", "merge", "1", "--yes"]).await;
    let mr = v4(&api, &format!("/projects/{project}/merge_requests/1")).await;
    assert_eq!((mr["state"].as_str(), mr["merge_when_pipeline_succeeds"].as_bool()), (Some("opened"), Some(true)), "{mr}");
    // armed, glab refuses to merge until the pipeline passes (as with GitLab): cancel it
    let (s, body) = owner.delete(&format!("/api/v1/projects/{project}/merge_requests/1/auto_merge")).await;
    assert_eq!(s, 200, "{body}");
    glab(&bin, &config, &work, &["mr", "merge", "1", "--yes", "--auto-merge=false"]).await;
    let mr = v4(&api, &format!("/projects/{project}/merge_requests/1")).await;
    assert_eq!(mr["state"], "merged", "{mr}");

    // issues
    let issue = glab(&bin, &config, &work, &["issue", "create", "--title", "Broken thing", "--description", "Steps", "--yes"]).await;
    assert!(issue.contains("/-/issues/1") || issue.contains("#1"), "{issue}");
    let issues = glab(&bin, &config, &work, &["issue", "list"]).await;
    assert!(issues.contains("Broken thing"), "{issues}");
    glab(&bin, &config, &work, &["issue", "close", "1"]).await;
    let i = v4(&api, &format!("/projects/{project}/issues/1")).await;
    assert_eq!(i["state"], "closed", "{i}");

    // pipelines (no runner: they stay pending, but glab lists them)
    let pipelines = glab(&bin, &config, &work, &["ci", "list"]).await;
    assert!(pipelines.contains("pending") || pipelines.contains("created"), "{pipelines}");

    // releases
    must(git(&work, &["checkout", "-q", "main"]).await);
    glab(&bin, &config, &work, &["release", "create", "v1.0.0", "--ref", "main", "--notes", "First release"]).await;
    let releases = glab(&bin, &config, &work, &["release", "list"]).await;
    assert!(releases.contains("v1.0.0"), "{releases}");

    // the generic API command
    let api = glab(&bin, &config, &work, &["api", &format!("projects/{project}")]).await;
    assert!(api.contains(REPO), "{api}");
}
