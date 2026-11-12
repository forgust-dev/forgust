//! M4: a real `gitlab-runner` (Docker executor) against Forgust. Opt in with
//! `FORGUST_TEST_RUNNER=gitlab/gitlab-runner:v19.5.3` (needs Docker and the
//! `alpine:3.20` image); CI runs it for each supported runner version.
//!
//! Measures the ADR-0008 targets: pipeline created → job picked up (1 s) and
//! cancel clicked → runner stops the job (5 s, for a job that prints output).
//!
//! The runner and its job containers share a network with this test's server
//! (listening on 127.0.0.1): `host` by default. Where the test itself runs in a
//! container that drives the engine through its socket (a CI job on Podman),
//! set `FORGUST_TEST_RUNNER_NETWORK=container:<this container>` and, when the
//! engine sees its socket elsewhere, `FORGUST_TEST_RUNNER_SOCKET=<host path>`.
//! The runner's config lives in a named volume, never a bind mount: the engine
//! may not see this machine's files.

mod common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};

struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker").args(["rm", "-f", &self.0]).output();
        let _ = std::process::Command::new("docker").args(["volume", "rm", "-f", &format!("{}-config", self.0)]).output();
    }
}

async fn docker(args: &[&str]) -> (bool, String) {
    let out = tokio::process::Command::new("docker").args(args).output().await.unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

async fn job_status(c: &Client, project: i64, id: i64) -> Value {
    c.ok_get(&format!("/api/v1/projects/{project}/jobs/{id}")).await
}

async fn wait_for<F, Fut>(timeout: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn stock_runner_runs_pipelines(pool: sqlx::PgPool) {
    let Ok(image) = std::env::var("FORGUST_TEST_RUNNER") else {
        eprintln!("skipped: set FORGUST_TEST_RUNNER=gitlab/gitlab-runner:<version> to run against a real runner");
        return;
    };
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let runner = owner.ok_post(&format!("/api/v1/projects/{project}/runners"), json!({ "description": "real runner" })).await;
    let token = runner["token"].as_str().unwrap();

    // register exactly as an operator would, then run the runner
    let network = std::env::var("FORGUST_TEST_RUNNER_NETWORK").unwrap_or_else(|_| "host".into());
    let socket = std::env::var("FORGUST_TEST_RUNNER_SOCKET").unwrap_or_else(|_| "/var/run/docker.sock".into());
    let name = format!("forgust-test-runner-{}", std::process::id());
    let _guard = Container(name.clone());
    let (ok, out) = docker(&["volume", "create", &format!("{name}-config")]).await;
    assert!(ok, "config volume: {out}");
    let conf_mount = format!("{name}-config:/etc/gitlab-runner");
    let socket_mount = format!("{socket}:/var/run/docker.sock");
    let (ok, out) = docker(&[
        "run",
        "--rm",
        "--network",
        &network,
        "-v",
        &conf_mount,
        &image,
        "register",
        "--non-interactive",
        "--url",
        &srv.base,
        "--token",
        token,
        "--executor",
        "docker",
        "--docker-image",
        "alpine:3.20",
        "--docker-network-mode",
        &network,
        "--docker-pull-policy",
        "if-not-present",
        "--name",
        &name,
    ])
    .await;
    assert!(ok && out.contains("Runner registered successfully"), "register failed: {out}");
    let (ok, out) = docker(&["run", "-d", "--name", &name, "--network", &network, "-v", &socket_mount, "-v", &conf_mount, &image]).await;
    assert!(ok, "runner did not start: {out}");

    let work = srv.work_dir("app");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(
        work.join(".gitlab-ci.yml"),
        "stages: [build, test]\nbuild:\n  stage: build\n  image: alpine:3.20\n  script:\n    - mkdir -p out && echo \"built $CI_COMMIT_SHORT_SHA\" > out/r.txt\n  artifacts:\n    paths: [out/]\ntest:\n  stage: test\n  image: alpine:3.20\n  script:\n    - grep built out/r.txt\n    - wget -qO- \"$CI_REPOSITORY_URL/info/refs?service=git-upload-pack\" | grep -q refs/heads/main\n",
    )
    .unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "ci"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;

    // the whole pipeline goes green on a stock runner
    let green = wait_for(Duration::from_secs(240), || async {
        let p = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/1")).await;
        matches!(p["status"].as_str(), Some("success" | "failed" | "canceled"))
    })
    .await;
    let p = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/1")).await;
    assert!(green && p["status"] == "success", "pipeline: {p}");

    // pick-up latency: job queued → claimed by the (waiting) runner
    let row: (f64,) = sqlx::query_as("SELECT extract(epoch FROM (started_at - queued_at))::float8 FROM ci_builds WHERE name = 'test'")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    eprintln!("pick-up latency (test job): {:.3}s", row.0);
    assert!(row.0 < 1.0, "pick-up took {:.3}s (target 1 s)", row.0);

    // cancel latency for a job that prints every second
    std::fs::write(work.join(".gitlab-ci.yml"), "noisy:\n  image: alpine:3.20\n  variables:\n    FF_TIMESTAMPS: \"true\"\n  script:\n    - for i in $(seq 120); do echo tick $i; sleep 1; done\n").unwrap();
    must(git(&work, &["commit", "-qam", "noisy"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;
    let p = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/2")).await;
    let id = p["jobs"][0]["id"].as_i64().unwrap();
    let ticking = wait_for(Duration::from_secs(120), || async {
        let t = owner.ok_get(&format!("/api/v1/projects/{project}/jobs/{id}/trace")).await;
        t["content"].as_str().unwrap_or("").contains("tick 3")
    })
    .await;
    assert!(ticking, "the noisy job never started");
    let clicked = chrono::Utc::now();
    owner.ok_post(&format!("/api/v1/projects/{project}/jobs/{id}/cancel"), json!({})).await;
    let mut log = String::new();
    let start = Instant::now();
    let mut stopped = false;
    while start.elapsed() < Duration::from_secs(30) {
        let t = owner.ok_get(&format!("/api/v1/projects/{project}/jobs/{id}/trace")).await;
        log = t["content"].as_str().unwrap_or("").to_string();
        if log.contains("canceled externally") {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(stopped, "the runner never reported the cancel");
    // when the runner stopped the script, by its own (timestamped) log line
    let line = log.lines().find(|l| l.contains("canceled externally")).unwrap();
    // runner timestamps when present (FF_TIMESTAMPS), else when the line arrived
    let at = chrono::DateTime::parse_from_rfc3339(line.split_whitespace().next().unwrap_or(""))
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    let cancel = (at - clicked).num_milliseconds() as f64 / 1000.0;
    eprintln!("cancel latency (clicked → runner stops the script): {cancel:.1}s");
    assert!(cancel < 5.0, "cancel took {cancel:.1}s (target 5 s)");
    let canceled = wait_for(Duration::from_secs(30), || async { job_status(&owner, project, id).await["status"] == "canceled" }).await;
    assert!(canceled, "job not recorded as canceled");

    // reports as this runner uploads them: dotenv variables reach the next stage,
    // JUnit files (two, concatenated) are read
    std::fs::write(
        work.join(".gitlab-ci.yml"),
        r#"stages: [build, check]
report:
  stage: build
  image: alpine:3.20
  script:
    - echo "BUILD_VERSION=1.2.$CI_PIPELINE_IID" > build.env
    - printf '<testsuite name="s"><testcase classname="c" name="ok"/><testcase classname="c" name="bad"><failure message="m">x</failure></testcase></testsuite>' > a.xml
    - printf '<testsuite name="t"><testcase classname="d" name="ok2"/></testsuite>' > b.xml
  artifacts:
    reports:
      dotenv: build.env
      junit: [a.xml, b.xml]
check:
  stage: check
  image: alpine:3.20
  script:
    - test "$BUILD_VERSION" = "1.2.3"
"#,
    )
    .unwrap();
    must(git(&work, &["commit", "-qam", "reports"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;
    let done = wait_for(Duration::from_secs(240), || async {
        let p = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/3")).await;
        matches!(p["status"].as_str(), Some("success" | "failed" | "canceled"))
    })
    .await;
    let p = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/3")).await;
    assert!(done && p["status"] == "success", "the dotenv variable did not reach `check`: {p}");
    wait_jobs(&srv.state).await;
    let t = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/3/test_report")).await;
    assert_eq!((t["total"].as_i64(), t["failed"].as_i64()), (Some(3), Some(1)), "{t}");
}
