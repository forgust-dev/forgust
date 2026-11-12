//! M7: the CI gaps left from M4, driven by a fake runner over HTTP.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

struct Fixture {
    srv: TestServer,
    owner: Client,
    project: i64,
    work: std::path::PathBuf,
}

type Received = Arc<Mutex<Vec<(String, String, Value)>>>;

/// A webhook receiver on loopback: (x-forgust-event, x-gitlab-event, body).
async fn listener() -> (String, Received) {
    use axum::body::Bytes;
    use axum::http::HeaderMap;
    let got: Received = Arc::new(Mutex::new(Vec::new()));
    let g = got.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: HeaderMap, body: Bytes| {
            let g = g.clone();
            async move {
                let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                g.lock().unwrap().push((h("x-forgust-event"), h("x-gitlab-event"), serde_json::from_slice(&body).unwrap_or(Value::Null)));
                "ok"
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}/hook"), got)
}

/// A project (with `before` run on it first) whose `main` gets `ci`.
async fn fixture<F, Fut>(pool: sqlx::PgPool, ci: &str, before: F) -> Fixture
where
    F: FnOnce(Client, i64) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    fixture_with(pool, ci, |_| {}, before).await
}

async fn fixture_with<F, Fut>(pool: sqlx::PgPool, ci: &str, tweak: impl FnOnce(&mut forgust_server::config::Config), before: F) -> Fixture
where
    F: FnOnce(Client, i64) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let srv = TestServer::start_with(pool, |c| {
        c.ci.long_poll_secs = 1;
        c.ci.max_trace_bytes = 1 << 20;
        c.features.webhooks_allow_local = true;
        tweak(c);
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    before(owner.clone(), project).await;
    let work = srv.work_dir("ci7");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join(".gitlab-ci.yml"), ci).unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "add ci"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;
    Fixture { srv, owner, project, work }
}

async fn runner(f: &Fixture) -> String {
    f.owner.ok_post(&format!("/api/v1/projects/{}/runners", f.project), json!({ "description": "fake" })).await["token"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn request_job(f: &Fixture, token: &str) -> (StatusCode, Value) {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", f.srv.base))
        .json(&json!({ "token": token, "system_id": "s_test", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    let s = r.status();
    (s, r.json().await.unwrap_or(Value::Null))
}

async fn finish(f: &Fixture, job: &Value, state: &str) {
    let r = reqwest::Client::new()
        .put(format!("{}/api/v4/jobs/{}", f.srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": state, "exit_code": if state == "success" { 0 } else { 1 }, "output": { "bytesize": 0 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn pipeline_and_job_webhooks(pool: sqlx::PgPool) {
    let (url, got) = listener().await;
    let f = fixture(pool, "build:\n  script: [make]\n", |owner, project| async move {
        owner.ok_post(&format!("/api/v1/projects/{project}/hooks"), json!({ "url": url, "events": ["pipeline", "build"] })).await;
    })
    .await;
    let token = runner(&f).await;
    let (s, job) = request_job(&f, &token).await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    finish(&f, &job, "success").await;
    wait_jobs(&f.srv.state).await;

    let got = got.lock().unwrap().clone();
    let builds: Vec<&Value> = got.iter().filter(|(e, _, _)| e == "build").map(|(_, _, b)| b).collect();
    let statuses: Vec<&str> = builds.iter().map(|b| b["build_status"].as_str().unwrap()).collect();
    for s in ["pending", "running", "success"] {
        assert!(statuses.contains(&s), "job hook for {s}: {statuses:?}");
    }
    let done = builds.iter().find(|b| b["build_status"] == "success").unwrap();
    assert_eq!(done["object_kind"], "build");
    assert_eq!(done["build_name"], "build");
    assert_eq!(done["ref"], "main");
    assert_eq!(done["project_name"], "acme/platform/api");
    let pipelines: Vec<&Value> = got.iter().filter(|(e, _, _)| e == "pipeline").map(|(_, _, b)| b).collect();
    let last = pipelines.iter().find(|p| p["object_attributes"]["status"] == "success").expect("a pipeline hook on success");
    assert_eq!(last["object_kind"], "pipeline");
    assert_eq!(last["builds"][0]["name"], "build");
    assert_eq!(last["commit"]["title"], "add ci");
    assert!(got.iter().all(|(e, g, _)| (e == "pipeline" && g == "Pipeline Hook") || (e == "build" && g == "Job Hook")));
    // each transition once: retried runner requests do not repeat them
    assert_eq!(statuses.iter().filter(|s| **s == "success").count(), 1);
}

async fn push_change(f: &Fixture, file: &str, ci: Option<&str>) {
    std::fs::write(f.work.join(file), format!("{file}\n")).unwrap();
    if let Some(ci) = ci {
        std::fs::write(f.work.join(".gitlab-ci.yml"), ci).unwrap();
    }
    must(git(&f.work, &["add", "."]).await);
    must(git(&f.work, &["commit", "-q", "-m", file]).await);
    must(git(&f.work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&f.srv.state).await;
}

/// Pipeline statuses, oldest first.
async fn pipeline_statuses(f: &Fixture) -> Vec<String> {
    let list = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines", f.project)).await;
    let mut out: Vec<(i64, String)> =
        list.as_array().unwrap().iter().map(|p| (p["iid"].as_i64().unwrap(), p["status"].as_str().unwrap().to_string())).collect();
    out.sort();
    out.into_iter().map(|(_, s)| s).collect()
}

const STAGED: &str = "stages: [build, deploy]
build:
  stage: build
  interruptible: true
  script: [make]
deploy:
  stage: deploy
  script: [ship]
";

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn redundant_pipelines_are_canceled(pool: sqlx::PgPool) {
    let f = fixture(pool, STAGED, |_, _| async {}).await;
    let token = runner(&f).await;

    // nothing started: the older pipeline goes
    push_change(&f, "a", None).await;
    assert_eq!(pipeline_statuses(&f).await, vec!["canceled", "pending"]);

    // only interruptible jobs started: still redundant
    let (_, job) = request_job(&f, &token).await;
    assert_eq!(job["job_info"]["name"], "build");
    push_change(&f, "b", None).await;
    // the running job is asked to stop (canceling) until its runner reports
    let jobs = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines/2", f.project)).await["jobs"].clone();
    let st = |name: &str| jobs.as_array().unwrap().iter().find(|j| j["name"] == name).unwrap()["status"].as_str().unwrap().to_string();
    assert_eq!((st("build"), st("deploy")), ("canceling".to_string(), "canceled".to_string()), "{jobs}");
    finish(&f, &job, "failed").await;
    assert_eq!(pipeline_statuses(&f).await, vec!["canceled", "canceled", "pending"]);

    // a job that is not interruptible started: the pipeline is kept
    let (_, build) = request_job(&f, &token).await;
    finish(&f, &build, "success").await;
    let (_, deploy) = request_job(&f, &token).await;
    assert_eq!(deploy["job_info"]["name"], "deploy");
    push_change(&f, "c", None).await;
    assert_eq!(pipeline_statuses(&f).await, vec!["canceled", "canceled", "running", "pending"]);
    finish(&f, &deploy, "success").await;

    // `interruptible`: only interruptible jobs go (the pipeline carries on without them)
    let ci = "workflow:\n  auto_cancel:\n    on_new_commit: interruptible\nstages: [build, deploy]\nbuild:\n  stage: build\n  interruptible: true\n  script: [make]\nlint:\n  stage: build\n  script: [lint]\ndeploy:\n  stage: deploy\n  script: [ship]\n";
    push_change(&f, "d", Some(ci)).await;
    let before = pipeline_statuses(&f).await.len();
    push_change(&f, "e", None).await;
    let list = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines", f.project)).await;
    let d = list.as_array().unwrap().iter().find(|p| p["iid"].as_i64() == Some(before as i64)).unwrap();
    let jobs = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines/{}", f.project, d["iid"])).await["jobs"].clone();
    let st = |name: &str| jobs.as_array().unwrap().iter().find(|j| j["name"] == name).unwrap()["status"].as_str().unwrap().to_string();
    assert_eq!(st("build"), "canceled", "{jobs}");
    assert_eq!(st("lint"), "pending");

    // the project setting turns it off
    let s = f.owner.ok_get(&format!("/api/v1/projects/{}/ci/settings", f.project)).await;
    assert_eq!(s["auto_cancel_redundant"], true);
    let mut off = s.clone();
    off["auto_cancel_redundant"] = json!(false);
    assert_eq!(f.owner.put(&format!("/api/v1/projects/{}/ci/settings", f.project), off).await.0, 200);
    let n = pipeline_statuses(&f).await;
    push_change(&f, "f", None).await;
    let after = pipeline_statuses(&f).await;
    assert_eq!(after[n.len() - 1], n[n.len() - 1], "the previous pipeline is left alone");
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let rt = tokio::runtime::Handle::current();
    let data = data.to_vec();
    std::thread::spawn(move || {
        rt.block_on(async move {
            use async_compression::tokio::write::GzipEncoder;
            use tokio::io::AsyncWriteExt;
            let mut e = GzipEncoder::new(Vec::new());
            e.write_all(&data).await.unwrap();
            e.shutdown().await.unwrap();
            e.into_inner()
        })
    })
    .join()
    .unwrap()
}

/// Upload a report as the runner does (gzip, `artifact_type`).
async fn upload_report(f: &Fixture, job: &Value, kind: &str, name: &str, body: Vec<u8>) -> (StatusCode, String) {
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(body).file_name(name.to_string()));
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=gzip&artifact_type={kind}", f.srv.base, job["id"]))
        .header("JOB-TOKEN", job["token"].as_str().unwrap())
        .multipart(form)
        .send()
        .await
        .unwrap();
    (r.status(), r.text().await.unwrap_or_default())
}

fn var<'a>(job: &'a Value, key: &str) -> Option<&'a str> {
    job["variables"].as_array().unwrap().iter().rev().find(|v| v["key"] == key).and_then(|v| v["value"].as_str())
}

const DOTENV_CI: &str = "stages: [build, use]
build:
  stage: build
  script: [make]
  artifacts:
    reports:
      dotenv: build.env
use:
  stage: use
  variables:
    VERSION: from-yaml
  script: [echo $VERSION]
blind:
  stage: use
  needs: [{ job: build, artifacts: false }]
  script: [echo $VERSION]
";

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn dotenv_reports_pass_variables_on(pool: sqlx::PgPool) {
    let f = fixture(pool, DOTENV_CI, |owner, project| async move {
        owner.ok_post(&format!("/api/v1/projects/{project}/variables"), json!({ "key": "REGION", "value": "from-project" })).await;
    })
    .await;
    let token = runner(&f).await;
    let (_, build) = request_job(&f, &token).await;
    assert_eq!(build["job_info"]["name"], "build");
    // refused as GitLab refuses them
    for bad in [&b"A=1\n\nB=2\n"[..], b"# note\nA=1\n", b"BAD-NAME=1\n", &vec![b'x'; 6000]] {
        let (s, body) = upload_report(&f, &build, "dotenv", "build.env", gzip(bad)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    }
    let (s, body) =
        upload_report(&f, &build, "dotenv", "build.env", gzip(b"VERSION=1.2.3\nREGION=from-dotenv\nURL=https://x/?a=b\n")).await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    finish(&f, &build, "success").await;

    let (_, a) = request_job(&f, &token).await;
    let (_, b) = request_job(&f, &token).await;
    let (used, blind) = if a["job_info"]["name"] == "use" { (a, b) } else { (b, a) };
    // over YAML variables, under project ones
    assert_eq!(var(&used, "VERSION"), Some("1.2.3"));
    assert_eq!(var(&used, "URL"), Some("https://x/?a=b"));
    assert_eq!(var(&used, "REGION"), Some("from-project"));
    // `needs: artifacts: false` gets none of it
    assert_eq!(var(&blind, "VERSION"), None, "{blind}");
}

fn junit(cases: &[(&str, bool)]) -> String {
    let body: String = cases
        .iter()
        .map(|(n, ok)| {
            if *ok {
                format!(r#"<testcase classname="cart" name="{n}" time="0.1"/>"#)
            } else {
                format!(r#"<testcase classname="cart" name="{n}"><failure message="{n} broke">trace of {n}</failure></testcase>"#)
            }
        })
        .collect();
    format!(r#"<?xml version="1.0"?><testsuite name="unit">{body}</testsuite>"#)
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn junit_reports_in_pipelines_and_merge_requests(pool: sqlx::PgPool) {
    let ci = "test:\n  script: [make test]\n  artifacts:\n    reports:\n      junit: [a.xml, b.xml]\n";
    let f = fixture(pool, ci, |_, _| async {}).await;
    let token = runner(&f).await;

    // main: `adds` passes, `removes` fails; two files, as the runner concatenates them
    let (st, job) = request_job(&f, &token).await;
    assert_eq!(st, StatusCode::CREATED, "{job} {:?}", pipeline_statuses(&f).await);
    let mut both = gzip(junit(&[("adds", true), ("removes", false)]).as_bytes());
    both.extend(gzip(junit(&[("totals", true)]).as_bytes()));
    assert_eq!(upload_report(&f, &job, "junit", "junit.xml.gz", both).await.0, StatusCode::CREATED);
    finish(&f, &job, "failed").await;
    wait_jobs(&f.srv.state).await;
    let t = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines/1/test_report", f.project)).await;
    assert_eq!((t["total"].as_i64(), t["failed"].as_i64(), t["success"].as_i64()), (Some(3), Some(1), Some(2)), "{t}");
    assert_eq!(t["jobs"][0]["failures"][0]["name"], "removes");
    assert_eq!(t["jobs"][0]["failures"][0]["message"], "removes broke");
    // every case, for the list of tests
    let all = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines/1/test_cases", f.project)).await;
    assert_eq!(all[0]["job_name"], "test");
    assert_eq!(all[0]["cases"][0], json!({ "suite": "unit", "classname": "cart", "name": "adds", "status": "success", "time": 0.1 }));
    assert_eq!((all[0]["cases"][1]["name"].as_str(), all[0]["cases"][1]["status"].as_str()), (Some("removes"), Some("failed")));
    assert_eq!(all[0]["cases"].as_array().unwrap().len(), 3);

    // a merge request breaks `adds` and fixes `removes`
    must(git(&f.work, &["checkout", "-q", "-b", "topic"]).await);
    std::fs::write(f.work.join("t.txt"), "t\n").unwrap();
    must(git(&f.work, &["add", "t.txt"]).await);
    must(git(&f.work, &["commit", "-q", "-m", "topic"]).await);
    must(git(&f.work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&f.srv.state).await;
    f.owner.ok_post(&format!("/api/v1/projects/{}/merge_requests", f.project), json!({ "source_branch": "topic", "title": "Topic" })).await;
    let (st, job) = request_job(&f, &token).await;
    assert_eq!(st, StatusCode::CREATED, "{job} {:?}", pipeline_statuses(&f).await);
    let report = gzip(junit(&[("adds", false), ("removes", true), ("totals", true)]).as_bytes());
    assert_eq!(upload_report(&f, &job, "junit", "junit.xml.gz", report).await.0, StatusCode::CREATED);
    finish(&f, &job, "failed").await;
    wait_jobs(&f.srv.state).await;
    let s = f.owner.ok_get(&format!("/api/v1/projects/{}/merge_requests/1/test_summary", f.project)).await;
    assert_eq!(s["head"]["failed"], 1, "{s}");
    assert_eq!(s["base"]["failed"], 1);
    assert_eq!(s["base_pipeline_iid"], 1);
    assert_eq!(s["new_failures"].as_array().unwrap().len(), 1);
    assert_eq!(s["new_failures"][0]["case"]["name"], "adds");
    assert_eq!(s["existing_failures"], json!([]));
    assert_eq!(s["resolved"], json!(["cart::removes"]));

    // a broken report is recorded as such, not fatal
    must(git(&f.work, &["checkout", "-q", "main"]).await);
    push_change(&f, "c", None).await;
    let (_, job) = request_job(&f, &token).await;
    assert_eq!(
        upload_report(&f, &job, "junit", "junit.xml.gz", gzip(b"<testsuite><testcase name=\"x\"><fail")).await.0,
        StatusCode::CREATED
    );
    finish(&f, &job, "success").await;
    wait_jobs(&f.srv.state).await;
    let list = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines", f.project)).await;
    let last = list.as_array().unwrap().iter().map(|p| p["iid"].as_i64().unwrap()).max().unwrap();
    let t = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines/{last}/test_report", f.project)).await;
    assert!(t["jobs"][0]["error"].is_string() || t["total"] == 0, "{t}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn browse_job_artifacts(pool: sqlx::PgPool) {
    let f = fixture(pool, "build:\n  script: [make]\n  artifacts:\n    paths: [out/]\n", |_, _| async {}).await;
    let token = runner(&f).await;
    let (_, job) = request_job(&f, &token).await;
    // a real zip (deflate), as `git archive` writes it
    let dir = f.srv.work_dir("zip");
    must(git(&dir, &["init", "-q", "-b", "main"]).await);
    std::fs::create_dir_all(dir.join("out/sub")).unwrap();
    let text = "built\n".repeat(200);
    std::fs::write(dir.join("out/r.txt"), &text).unwrap();
    std::fs::write(dir.join("out/page.html"), "<script>alert(1)</script>").unwrap();
    std::fs::write(dir.join("out/sub/data.bin"), [0u8, 1, 2, 3]).unwrap();
    must(git(&dir, &["add", "."]).await);
    must(git(&dir, &["commit", "-q", "-m", "z"]).await);
    let zip = dir.join("a.zip");
    must(git(&dir, &["archive", "--format=zip", "-o", zip.to_str().unwrap(), "HEAD"]).await);
    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(std::fs::read(&zip).unwrap()).file_name("artifacts.zip"));
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=zip&artifact_type=archive", f.srv.base, job["id"]))
        .header("JOB-TOKEN", job["token"].as_str().unwrap())
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    finish(&f, &job, "success").await;
    let base = format!("/api/v1/projects/{}/jobs/{}/artifacts", f.project, job["id"]);

    let root = f.owner.ok_get(&format!("{base}/tree")).await;
    assert_eq!(root["entries"][0]["name"], "out", "{root}");
    assert_eq!(root["entries"][0]["dir"], true);
    let out = f.owner.ok_get(&format!("{base}/tree?path=out/")).await;
    let names: Vec<&str> = out["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["sub", "page.html", "r.txt"]);
    assert_eq!(f.owner.get(&format!("{base}/tree?path=nope/")).await.0, 404);

    // text is shown (inflated), never as HTML; other files download
    let r = f.owner.req(reqwest::Method::GET, &format!("{base}/file?path=out/r.txt")).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "text/plain; charset=utf-8");
    assert_eq!(r.text().await.unwrap(), text);
    let r = f.owner.req(reqwest::Method::GET, &format!("{base}/file?path=out/page.html")).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "text/plain; charset=utf-8");
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    let r = f.owner.req(reqwest::Method::GET, &format!("{base}/file?path=out/sub/data.bin")).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "application/octet-stream");
    assert!(r.headers()["content-disposition"].to_str().unwrap().starts_with("attachment"));
    assert_eq!(r.bytes().await.unwrap().as_ref(), &[0u8, 1, 2, 3]);
    // names are matched exactly
    for p in ["out/../out/r.txt", "out/", "/etc/passwd", "out/R.txt"] {
        assert_eq!(f.owner.get(&format!("{base}/file?path={p}")).await.0, 404, "{p}");
    }
    // strangers do not see it (private project)
    assert!(matches!(Client::new(&f.srv.base).get(&format!("{base}/tree")).await.0, 401 | 404));
    // /api/v4, as GitLab
    let tok =
        f.owner.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let r = reqwest::Client::new()
        .get(format!("{}/api/v4/projects/{}/jobs/{}/artifacts/out/r.txt", f.srv.base, f.project, job["id"]))
        .header("PRIVATE-TOKEN", &tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.text().await.unwrap(), text);
}

/// A job log stops at `ci.max_trace_bytes`, whoever writes it, and the job fails.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn job_logs_are_capped(pool: sqlx::PgPool) {
    let f = fixture(pool, "build:\n  script: [make]\n", |_, _| async {}).await;
    let token = runner(&f).await;
    let (s, job) = request_job(&f, &token).await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    let patch = |start: usize, len: usize| {
        reqwest::Client::new()
            .patch(format!("{}/api/v4/jobs/{}/trace", f.srv.base, job["id"]))
            .header("JOB-TOKEN", job["token"].as_str().unwrap())
            .header("Content-Range", format!("{start}-{}", start + len - 1))
            .body(vec![b'x'; len])
            .send()
    };
    assert_eq!(patch(0, 600_000).await.unwrap().status(), StatusCode::ACCEPTED);
    assert_eq!(patch(600_000, 600_000).await.unwrap().status(), StatusCode::FORBIDDEN);
    let shown = f.owner.ok_get(&format!("/api/v1/projects/{}/jobs/{}", f.project, job["id"])).await;
    assert_eq!((shown["status"].as_str(), shown["failure_reason"].as_str()), (Some("failed"), Some("trace_size_exceeded")));
}

/// Someone else's schedule: a Developer sees its variable names, not their
/// values, and cannot run it (it would run as its owner).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn others_schedules_are_the_owners(pool: sqlx::PgPool) {
    let f = fixture(pool, "build:\n  script: [make]\n", |_, _| async {}).await;
    let dev = f.srv.user("dave", false).await;
    let base = format!("/api/v1/projects/{}/pipeline_schedules", f.project);
    f.owner.ok_post(&format!("/api/v1/projects/{}/members", f.project), json!({ "username": "dave", "access_level": "developer" })).await;
    let sched = f
        .owner
        .ok_post(&base, json!({ "ref": "main", "cron": "0 3 * * *", "variables": [{ "key": "DEPLOY_KEY", "value": "s3cret" }] }))
        .await;
    let theirs = dev.ok_get(&base).await;
    assert_eq!(theirs[0]["variables"][0]["key"], "DEPLOY_KEY");
    assert_eq!(theirs[0]["variables"][0]["value"], "");
    assert_eq!(f.owner.ok_get(&base).await[0]["variables"][0]["value"], "s3cret");
    let (s, body) = dev.post(&format!("{base}/{}/play", sched["id"]), json!({})).await;
    assert_eq!(s, 403, "{body}");
    // their own schedule they run (on a branch they may push)
    must(git(&f.work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "main:dev-branch"]).await);
    wait_jobs(&f.srv.state).await;
    let mine = dev.ok_post(&base, json!({ "ref": "dev-branch", "cron": "0 3 * * *" })).await;
    dev.ok_post(&format!("{base}/{}/play", mine["id"]), json!({})).await;
    // they leave the project: their schedule stops (it would run as them)
    let dave = dev.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let (s, body) = f.owner.delete(&format!("/api/v1/projects/{}/members/{dave}", f.project)).await;
    assert_eq!(s, 200, "{body}");
    let (s, body) = f.owner.post(&format!("{base}/{}/play", mine["id"]), json!({})).await;
    assert_eq!(s, 400, "{body}");
    let after = f.owner.ok_get(&base).await;
    let theirs = after.as_array().unwrap().iter().find(|x| x["id"] == mine["id"]).unwrap();
    assert_eq!(theirs["active"], false, "{theirs}");
}

/// A project's artifacts have a quota: an upload past it is refused (413),
/// and one that would cross it is cut at what is left.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn artifact_storage_has_a_quota(pool: sqlx::PgPool) {
    let ci = "a:\n  script: [make]\nb:\n  script: [make]\nc:\n  script: [make]\n";
    let f = fixture_with(pool, ci, |c| c.ci.max_artifact_bytes_per_project = 1000, |_, _| async {}).await;
    let token = runner(&f).await;
    let upload = |job: Value, len: usize| {
        let base = f.srv.base.clone();
        async move {
            let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(vec![b'z'; len]).file_name("a.zip"));
            reqwest::Client::new()
                .post(format!("{base}/api/v4/jobs/{}/artifacts?artifact_format=zip&artifact_type=archive", job["id"]))
                .header("JOB-TOKEN", job["token"].as_str().unwrap())
                .multipart(form)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    let (_, a) = request_job(&f, &token).await;
    assert_eq!(upload(a, 600).await, StatusCode::CREATED);
    let (_, b) = request_job(&f, &token).await;
    assert_eq!(upload(b, 600).await, StatusCode::PAYLOAD_TOO_LARGE, "600 + 600 > 1000");
    let (_, c) = request_job(&f, &token).await;
    assert_eq!(upload(c.clone(), 300).await, StatusCode::CREATED, "600 + 300 fits");
    assert_eq!(forgust_db::ci::artifact_bytes(&f.srv.state.db, f.project).await.unwrap(), 900);
}

/// The project's storage quota caps artifacts too; the job itself
/// still runs and finishes.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn artifacts_stop_at_the_storage_quota(pool: sqlx::PgPool) {
    let ci = "a:\n  script: [make]\nb:\n  script: [make]\n";
    let f = fixture_with(pool, ci, |_| {}, |_, _| async {}).await;
    let token = runner(&f).await;
    let db = &f.srv.state.db;
    let used = forgust_db::quotas::project_used(db, f.project).await.unwrap();
    sqlx::query("UPDATE projects SET quota_bytes = $2 WHERE id = $1").bind(f.project).bind(used + 500).execute(db).await.unwrap();
    let upload = |job: &Value, len: usize| {
        let (base, id, tok) = (f.srv.base.clone(), job["id"].clone(), job["token"].as_str().unwrap().to_string());
        async move {
            let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(vec![b'z'; len]).file_name("a.zip"));
            let r = reqwest::Client::new()
                .post(format!("{base}/api/v4/jobs/{id}/artifacts?artifact_format=zip&artifact_type=archive"))
                .header("JOB-TOKEN", tok)
                .multipart(form)
                .send()
                .await
                .unwrap();
            (r.status(), r.text().await.unwrap())
        }
    };
    let (_, a) = request_job(&f, &token).await;
    assert_eq!(upload(&a, 600).await.0, StatusCode::PAYLOAD_TOO_LARGE, "more than the 500 left");
    assert_eq!(upload(&a, 500).await.0, StatusCode::CREATED, "exactly what is left");
    let (_, b) = request_job(&f, &token).await;
    let (s, body) = upload(&b, 10).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(body.contains("Storage quota reached: this project"), "{body}");
}

/// A long log is read in pieces of at most 1 MB, live and once archived.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn long_logs_are_read_in_pieces(pool: sqlx::PgPool) {
    let f = fixture_with(pool, "build:\n  script: [make]\n", |c| c.ci.max_trace_bytes = 4 << 20, |_, _| async {}).await;
    let token = runner(&f).await;
    let (_, job) = request_job(&f, &token).await;
    let len = (1 << 20) + 300_000;
    let log: Vec<u8> = (0..len).map(|i| b'a' + (i % 26) as u8).collect();
    let r = reqwest::Client::new()
        .patch(format!("{}/api/v4/jobs/{}/trace", f.srv.base, job["id"]))
        .header("JOB-TOKEN", job["token"].as_str().unwrap())
        .header("Content-Range", format!("0-{}", len - 1))
        .body(log.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let read_all = |done: bool| {
        let (owner, project, id, log) = (f.owner.clone(), f.project, job["id"].clone(), log.clone());
        async move {
            let first = owner.ok_get(&format!("/api/v1/projects/{project}/jobs/{id}/trace?offset=0")).await;
            assert_eq!(first["size"], 1 << 20, "a first piece of 1 MB");
            assert_eq!((first["more"].as_bool(), first["complete"].as_bool()), (Some(true), Some(false)));
            let rest = owner.ok_get(&format!("/api/v1/projects/{project}/jobs/{id}/trace?offset={}", first["size"])).await;
            assert_eq!(rest["more"], false);
            assert_eq!(rest["complete"], done);
            let text = format!("{}{}", first["content"].as_str().unwrap(), rest["content"].as_str().unwrap());
            assert_eq!(text.as_bytes(), &log[..]);
        }
    };
    read_all(false).await;
    finish(&f, &job, "success").await;
    let b = forgust_db::ci::build(&f.srv.state.db, job["id"].as_i64().unwrap()).await.unwrap().unwrap();
    forgust_server::ci::trace::archive(&f.srv.state, b.id).await.unwrap();
    read_all(true).await;
}

/// A merge request from a protected branch the author cannot push gets no
/// pipeline (it would carry the protected variables).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn merge_request_pipelines_need_the_source(pool: sqlx::PgPool) {
    let ci = "mr:\n  script: [make]\n  rules:\n    - if: $CI_PIPELINE_SOURCE == \"merge_request_event\"\n";
    let f = fixture(pool, ci, |_, _| async {}).await;
    let dev = f.srv.user("dave", false).await;
    f.owner.ok_post(&format!("/api/v1/projects/{}/members", f.project), json!({ "username": "dave", "access_level": "developer" })).await;
    let url = f.srv.http_url("dave", "acme/platform/api");
    must(git(&f.work, &["push", "-q", &url, "main:other"]).await);
    must(git(&f.work, &["push", "-q", &url, "main:topic"]).await);
    wait_jobs(&f.srv.state).await;
    let mrs = format!("/api/v1/projects/{}/merge_requests", f.project);
    dev.ok_post(&mrs, json!({ "source_branch": "main", "target_branch": "other", "title": "From main" })).await;
    dev.ok_post(&mrs, json!({ "source_branch": "topic", "target_branch": "other", "title": "From topic" })).await;
    wait_jobs(&f.srv.state).await;
    let list = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines", f.project)).await;
    let refs: Vec<&str> =
        list.as_array().unwrap().iter().filter(|p| p["source"] == "merge_request_event").map(|p| p["ref"].as_str().unwrap()).collect();
    assert_eq!(refs, ["topic"]);
}
