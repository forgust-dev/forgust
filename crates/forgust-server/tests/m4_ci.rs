//! M4: the GitLab Runner protocol, driven by a fake runner over HTTP (no
//! Docker needed). The real-runner test lives in `m4_runner.rs`.

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

const CI: &str = "stages: [build, test]
variables:
  GREETING: hello
build:
  stage: build
  image: alpine:3.20
  script:
    - echo $GREETING > out.txt
  artifacts:
    paths: [out.txt]
    expire_in: 1 week
test:
  stage: test
  image: alpine:3.20
  before_script: [echo before]
  script: [cat out.txt]
  after_script: [echo after]
tagged:
  stage: test
  tags: [gpu]
  script: [nvidia-smi]
";

struct Fixture {
    srv: TestServer,
    owner: Client,
    project: i64,
    sha: String,
}

async fn fixture(pool: sqlx::PgPool) -> Fixture {
    let srv = TestServer::start_with(pool, |c| {
        c.ci.long_poll_secs = 1;
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let work = srv.work_dir("ci");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join(".gitlab-ci.yml"), CI).unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "add ci"]).await);
    let sha = must(git(&work, &["rev-parse", "HEAD"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;
    Fixture { srv, owner, project, sha }
}

fn runner_req(f: &Fixture, path: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new().post(format!("{}/api/v4{path}", f.srv.base))
}

async fn new_runner(f: &Fixture, body: Value) -> String {
    let r = f.owner.ok_post(&format!("/api/v1/projects/{}/runners", f.project), body).await;
    r["token"].as_str().unwrap().to_string()
}

async fn request_job(f: &Fixture, token: &str) -> (StatusCode, Value) {
    let r = runner_req(f, "/jobs/request")
        .json(&json!({ "token": token, "system_id": "s_test", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    let s = r.status();
    (s, r.json().await.unwrap_or(Value::Null))
}

fn var<'a>(job: &'a Value, key: &str) -> Option<&'a Value> {
    job["variables"].as_array().unwrap().iter().rev().find(|v| v["key"] == key)
}

async fn patch(f: &Fixture, id: i64, token: &str, start: usize, data: &str) -> reqwest::Response {
    reqwest::Client::new()
        .patch(format!("{}/api/v4/jobs/{id}/trace", f.srv.base))
        .header("JOB-TOKEN", token)
        .header("Content-Range", format!("{start}-{}", start + data.len() - 1))
        .body(data.to_string())
        .send()
        .await
        .unwrap()
}

async fn put(f: &Fixture, id: i64, body: Value) -> reqwest::Response {
    reqwest::Client::new().put(format!("{}/api/v4/jobs/{id}", f.srv.base)).json(&body).send().await.unwrap()
}

fn header(r: &reqwest::Response, name: &str) -> String {
    r.headers().get(name).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn runner_protocol_end_to_end(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let pipelines = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines", f.project)).await;
    assert_eq!(pipelines.as_array().unwrap().len(), 1, "a push creates one pipeline");
    assert_eq!(pipelines[0]["source"], "push");
    assert_eq!(pipelines[0]["status"], "pending");
    assert_eq!(pipelines[0]["protected"], true, "main is the protected default branch");

    // legacy registration is off; verify checks the glrt- token
    let r = runner_req(&f, "/runners").json(&json!({ "token": "x" })).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::GONE);
    let r = runner_req(&f, "/runners/verify").json(&json!({ "token": "glrt-nope", "system_id": "s1" })).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let token = new_runner(&f, json!({ "description": "fake" })).await;
    assert!(token.starts_with("glrt-"));
    let r = runner_req(&f, "/runners/verify")
        .json(&json!({ "token": token, "system_id": "s1", "info": { "version": "19.5.3" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // the build job comes first (test waits for its stage)
    let (s, job) = request_job(&f, &token).await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    assert_eq!(job["job_info"]["name"], "build");
    let id = job["id"].as_i64().unwrap();
    let jt = job["token"].as_str().unwrap().to_string();
    let pipeline_id = job["job_info"]["pipeline_id"].as_i64().unwrap();
    assert!(job["git_info"]["repo_url"].as_str().unwrap().contains(&format!("gitlab-ci-token:{jt}@")));
    assert_eq!(job["git_info"]["refspecs"][0], format!("+{}:refs/pipelines/{pipeline_id}", f.sha));
    assert_eq!(job["git_info"]["sha"], f.sha);
    assert_eq!(job["git_info"]["protected"], true);
    assert_eq!(job["image"]["name"], "alpine:3.20");
    assert_eq!(job["steps"][0]["script"], json!(["echo $GREETING > out.txt"]));
    assert_eq!(job["artifacts"][0]["paths"], json!(["out.txt"]));
    assert_eq!(var(&job, "CI_JOB_TOKEN").unwrap()["masked"], true);
    assert_eq!(var(&job, "CI_PROJECT_PATH").unwrap()["value"], "acme/platform/api");
    assert_eq!(var(&job, "CI_COMMIT_BRANCH").unwrap()["value"], "main");
    assert_eq!(var(&job, "GREETING").unwrap()["value"], "hello");
    assert_eq!(var(&job, "GITLAB_USER_LOGIN").unwrap()["value"], "olivia");
    assert!(var(&job, "CI_REPOSITORY_URL").unwrap()["value"].as_str().unwrap().contains(&jt));

    // the job token clones, including the pipeline's pinned commit by id
    let clone = f.srv.work_dir("clone");
    must(git(&clone, &["init", "-q"]).await);
    let url = job["git_info"]["repo_url"].as_str().unwrap();
    must(git(&clone, &["fetch", "-q", url, &format!("+{}:refs/pipelines/{pipeline_id}", f.sha)]).await);

    // trace: append, wrong offset (416 + Range), append again
    let r = patch(&f, id, &jt, 0, "hello").await;
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    assert_eq!(header(&r, "Range"), "0-5");
    assert_eq!(header(&r, "Job-Status"), "running");
    assert_eq!(header(&r, "X-GitLab-Trace-Update-Interval"), "3");
    let r = patch(&f, id, &jt, 2, "xx").await;
    assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header(&r, "Range"), "0-5");
    assert_eq!(patch(&f, id, &jt, 5, " world\n").await.status(), StatusCode::ACCEPTED);
    let t = f.owner.ok_get(&format!("/api/v1/projects/{}/jobs/{id}/trace", f.project)).await;
    assert_eq!(t["content"], "hello world\n");
    // someone else's job token cannot write here
    assert_eq!(patch(&f, id, "fgcbt-wrong", 12, "x").await.status(), StatusCode::FORBIDDEN);

    // keep-alive
    let r = put(&f, id, json!({ "token": jt, "state": "running" })).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(header(&r, "Job-Status"), "running");

    // artifacts: multipart `file`, streamed to the blob store
    let zip = b"PK\x05\x06fake-zip-bytes".to_vec();
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(zip.clone()).file_name("artifacts.zip"));
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/{id}/artifacts?artifact_format=zip&artifact_type=archive&expire_in=1+week", f.srv.base))
        .header("JOB-TOKEN", &jt)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);

    // a final update claiming more log than arrived is refused (412) until the log catches up
    let r = put(&f, id, json!({ "token": jt, "state": "success", "output": { "bytesize": 99 } })).await;
    assert_eq!(r.status(), StatusCode::PRECONDITION_FAILED);
    let r = put(&f, id, json!({ "token": jt, "state": "success", "exit_code": 0, "output": { "bytesize": 12 } })).await;
    assert_eq!(r.status(), StatusCode::OK);
    // the token dies with the job
    assert_eq!(patch(&f, id, &jt, 12, "late").await.status(), StatusCode::FORBIDDEN);
    // the finished log moves to the blob store and still reads the same
    forgust_server::ci::trace::archive(&f.srv.state, id).await.unwrap();
    let t = f.owner.ok_get(&format!("/api/v1/projects/{}/jobs/{id}/trace", f.project)).await;
    assert_eq!(t["content"], "hello world\n");
    assert_eq!(t["complete"], true);
    let (s, raw) = f.owner.get_bytes(&format!("/api/v1/projects/{}/jobs/{id}/raw", f.project)).await;
    assert_eq!((s, raw), (200, b"hello world\n".to_vec()));
    let chunks: i64 = sqlx::query_scalar("SELECT count(*) FROM ci_job_trace_chunks WHERE build_id = $1")
        .bind(id)
        .fetch_one(&f.srv.state.db)
        .await
        .unwrap();
    assert_eq!(chunks, 0);
    let clone2 = f.srv.work_dir("clone2");
    must(git(&clone2, &["init", "-q"]).await);
    assert!(!git(&clone2, &["ls-remote", url]).await.ok, "job token still works after the job");

    // next: `test` (the gpu job needs a tagged runner), with build as dependency
    let (s, test) = request_job(&f, &token).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(test["job_info"]["name"], "test");
    assert_eq!(test["steps"][0]["script"], json!(["echo before", "cat out.txt"]));
    assert_eq!(test["steps"][1]["name"], "after_script");
    assert_eq!(test["steps"][1]["when"], "always");
    let dep = &test["dependencies"][0];
    assert_eq!(dep["name"], "build");
    assert_eq!(dep["artifacts_file"]["size"], zip.len());
    let tid = test["id"].as_i64().unwrap();
    let tt = test["token"].as_str().unwrap().to_string();
    let r = reqwest::Client::new().get(format!("{}/api/v4/jobs/{id}/artifacts", f.srv.base)).header("JOB-TOKEN", &tt).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.bytes().await.unwrap().to_vec(), zip);
    // a job is not its own dependency
    let r =
        reqwest::Client::new().get(format!("{}/api/v4/jobs/{tid}/artifacts", f.srv.base)).header("JOB-TOKEN", &tt).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    // the untagged runner never gets the gpu job
    let (s, _) = request_job(&f, &token).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // cancel from the API: the runner learns it on its next update
    f.owner.ok_post(&format!("/api/v1/projects/{}/jobs/{tid}/cancel", f.project), json!({})).await;
    let r = patch(&f, tid, &tt, 0, "running test\n").await;
    assert_eq!(header(&r, "Job-Status"), "canceling");
    assert_eq!(header(&r, "X-GitLab-Trace-Update-Interval"), "1");
    let r = put(&f, tid, json!({ "token": tt, "state": "failed", "failure_reason": "script_failure", "output": { "bytesize": 13 } })).await;
    assert_eq!(r.status(), StatusCode::OK);
    let job = f.owner.ok_get(&format!("/api/v1/projects/{}/jobs/{tid}", f.project)).await;
    assert_eq!(job["status"], "canceled");

    // a tagged runner takes the gpu job; then the pipeline finishes
    let gpu = new_runner(&f, json!({ "tags": ["gpu", "cuda"], "run_untagged": false })).await;
    let (s, g) = request_job(&f, &gpu).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(g["job_info"]["name"], "tagged");
    let r = put(
        &f,
        g["id"].as_i64().unwrap(),
        json!({ "token": g["token"], "state": "failed", "failure_reason": "script_failure", "exit_code": 127 }),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let p = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines/1", f.project)).await;
    assert_eq!(p["status"], "failed");
    let statuses: Vec<(String, String)> =
        p["jobs"].as_array().unwrap().iter().map(|j| (j["name"].as_str().unwrap().into(), j["status"].as_str().unwrap().into())).collect();
    assert!(statuses.contains(&("build".into(), "success".into())));
    assert!(statuses.contains(&("test".into(), "canceled".into())));
    assert!(statuses.contains(&("tagged".into(), "failed".into())));

    // the finished pipeline no longer pins its commit; a retry pins it again
    let pin = format!("refs/pipelines/{pipeline_id}");
    assert!(f.srv.state.storage.resolve(f.project, &pin).await.unwrap().is_none(), "pipeline ref kept after the pipeline finished");

    // retry the canceled job: a new attempt, queued again
    let retry = f.owner.ok_post(&format!("/api/v1/projects/{}/jobs/{tid}/retry", f.project), json!({})).await;
    assert_eq!(retry["attempt"], 2);
    assert_eq!(retry["status"], "pending");
    let (s, again) = request_job(&f, &token).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(again["job_info"]["name"], "test");
    assert_eq!(f.srv.state.storage.resolve(f.project, &pin).await.unwrap().as_deref(), Some(f.sha.as_str()));
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn runner_scopes(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    // a runner of another organization never sees this project's jobs
    let other = f.owner.ok_post("/api/v1/groups", json!({ "name": "Other", "path": "other" })).await;
    let r = f.owner.ok_post(&format!("/api/v1/groups/{}/runners", other["id"]), json!({})).await;
    let (s, _) = request_job(&f, r["token"].as_str().unwrap()).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // a paused runner gets nothing
    let paused = new_runner(&f, json!({ "paused": true })).await;
    assert_eq!(request_job(&f, &paused).await.0, StatusCode::NO_CONTENT);

    // instance runners need an admin to create, and can be turned off per project
    let admin = f.srv.user("root", true).await;
    assert_eq!(f.owner.post("/api/v1/admin/runners", json!({})).await.0, 403);
    let inst = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared" })).await;
    let inst = inst["token"].as_str().unwrap().to_string();
    let mut settings = f.owner.ok_get(&format!("/api/v1/projects/{}/ci/settings", f.project)).await;
    settings["instance_runners_enabled"] = json!(false);
    let (s, _) = f.owner.put(&format!("/api/v1/projects/{}/ci/settings", f.project), settings).await;
    assert_eq!(s, 200);
    // the setting applies to jobs queued from now on: retry to requeue
    f.owner.ok_post(&format!("/api/v1/projects/{}/pipelines/1/cancel", f.project), json!({})).await;
    f.owner.ok_post(&format!("/api/v1/projects/{}/pipelines/1/retry", f.project), json!({})).await;
    assert_eq!(request_job(&f, &inst).await.0, StatusCode::NO_CONTENT);

    // the parent group's runner works for the project in the subgroup; protected_only too (main is protected)
    let groups = f.owner.ok_get("/api/v1/groups").await;
    let acme = groups.as_array().unwrap().iter().find(|g| g["path"] == "acme").unwrap()["id"].clone();
    let g = f.owner.ok_post(&format!("/api/v1/groups/{acme}/runners"), json!({ "protected_only": true })).await;
    let (s, job) = request_job(&f, g["token"].as_str().unwrap()).await;
    assert_eq!(s, StatusCode::CREATED, "{job}");
    assert_eq!(job["job_info"]["name"], "build");

    // runner managers are recorded per system_id
    let list = f.owner.ok_get(&format!("/api/v1/projects/{}/runners", f.project)).await;
    let group_runner = list.as_array().unwrap().iter().find(|r| r["kind"] == "group").unwrap();
    assert_eq!(group_runner["managers"][0]["system_id"], "s_test");
    assert_eq!(group_runner["managers"][0]["version"], "19.5.3");
    assert_eq!(group_runner["status"], "online");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn variables_and_config_errors(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let base = format!("/api/v1/projects/{}/variables", f.project);
    // masked values must be maskable; masked values are write-only
    let (s, _) = f.owner.post(&base, json!({ "key": "SHORT", "value": "abc", "masked": true })).await;
    assert_eq!(s, 400);
    f.owner.ok_post(&base, json!({ "key": "SECRET", "value": "s3cr3t-value", "masked": true })).await;
    f.owner.ok_post(&base, json!({ "key": "GREETING", "value": "from-project" })).await;
    f.owner.ok_post(&base, json!({ "key": "DEPLOY_KEY", "value": "only-protected", "protected": true })).await;
    let list = f.owner.ok_get(&base).await;
    let secret = list.as_array().unwrap().iter().find(|v| v["key"] == "SECRET").unwrap();
    assert!(secret["value"].is_null());
    // stored encrypted
    let raw: String =
        sqlx::query_scalar("SELECT value_enc FROM ci_variables WHERE key = 'SECRET'").fetch_one(&f.srv.state.db).await.unwrap();
    assert!(!raw.contains("s3cr3t"));

    let token = new_runner(&f, json!({})).await;
    let (_, job) = request_job(&f, &token).await;
    // project variables override YAML ones; masked stays masked
    assert_eq!(var(&job, "GREETING").unwrap()["value"], "from-project");
    assert_eq!(var(&job, "SECRET").unwrap()["masked"], true);
    assert_eq!(var(&job, "SECRET").unwrap()["value"], "s3cr3t-value");
    // main is protected, so protected variables are there
    assert_eq!(var(&job, "DEPLOY_KEY").unwrap()["value"], "only-protected");

    // an unprotected branch gets no protected variables
    let work = f.srv.work_dir("ci");
    must(git(&work, &["checkout", "-q", "-b", "feature"]).await);
    must(git(&work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "feature"]).await);
    wait_jobs(&f.srv.state).await;
    let mut feature_job = Value::Null;
    for _ in 0..5 {
        let (s, j) = request_job(&f, &token).await;
        if s == StatusCode::CREATED && j["git_info"]["ref"] == "feature" {
            feature_job = j;
            break;
        }
    }
    assert!(var(&feature_job, "DEPLOY_KEY").is_none());
    assert_eq!(feature_job["git_info"]["protected"], false);

    // a broken config makes a failed pipeline that says why
    must(git(&work, &["checkout", "-q", "-b", "broken"]).await);
    std::fs::write(work.join(".gitlab-ci.yml"), "job:\n  script: x\n  secrets:\n    DB: {vault: db/pw}\n").unwrap();
    must(git(&work, &["commit", "-qam", "env"]).await);
    must(git(&work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "broken"]).await);
    wait_jobs(&f.srv.state).await;
    let list = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines?ref=broken", f.project)).await;
    assert_eq!(list[0]["status"], "failed");
    assert!(list[0]["yaml_errors"].as_str().unwrap().contains("secrets"), "{}", list[0]);

    // run a pipeline from the API with a variable
    let p = f
        .owner
        .ok_post(
            &format!("/api/v1/projects/{}/pipelines", f.project),
            json!({ "ref": "feature", "variables": [{ "key": "EXTRA", "value": "1" }] }),
        )
        .await;
    assert_eq!(p["source"], "web");
}

async fn push_files(srv: &TestServer, dir: &std::path::Path, user: &str, path: &str, branch: &str, files: &[(&str, &str)], msg: &str) {
    for (p, c) in files {
        let f = dir.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, c).unwrap();
    }
    must(git(dir, &["add", "-A"]).await);
    must(git(dir, &["commit", "-q", "-m", msg]).await);
    must(git(dir, &["push", "-q", &srv.http_url(user, path), &format!("HEAD:refs/heads/{branch}")]).await);
    wait_jobs(&srv.state).await;
}

async fn jobs_of(c: &Client, project: i64, iid: i64) -> Vec<(String, String)> {
    let p = c.ok_get(&format!("/api/v1/projects/{project}/pipelines/{iid}")).await;
    p["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| j["retried"] == false)
        .map(|j| (j["name"].as_str().unwrap().into(), j["status"].as_str().unwrap().into()))
        .collect()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn pipeline_engine(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    // a shared template project the owner can read; a stranger's private one they cannot
    owner.ok_post("/api/v1/projects", json!({ "name": "templates", "path": "templates", "namespace_id": _org })).await;
    let tdir = srv.work_dir("tpl");
    must(git(&tdir, &["init", "-q", "-b", "main"]).await);
    push_files(&srv, &tdir, "olivia", "acme/templates", "main", &[("ci/lint.yml", ".lint:\n  stage: test\n  script: [lint]\n")], "tpl")
        .await;
    let mallory = srv.user("mallory", false).await;
    mallory.ok_post("/api/v1/projects", json!({ "name": "secret", "path": "secret" })).await;

    let ci = r#"
include:
  - local: ci/build.yml
  - project: acme/templates
    file: ci/lint.yml
workflow:
  rules:
    - if: $CI_COMMIT_MESSAGE =~ /skip-ci/
      when: never
    - when: always
stages: [build, test, deploy]
lint:
  extends: .lint
test:
  stage: test
  needs: [build]
  parallel: 2
  script: [test]
docs:
  stage: test
  script: [docs]
  rules:
    - changes: [docs/**/*]
mr-only:
  stage: test
  script: [mr]
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
deploy:
  stage: deploy
  script: [deploy]
  when: manual
flaky:
  stage: deploy
  script: [flaky]
  retry: {max: 1, exit_codes: [137]}
  allow_failure: {exit_codes: [3]}
"#;
    let dir = srv.work_dir("app");
    must(git(&dir, &["init", "-q", "-b", "main"]).await);
    push_files(
        &srv,
        &dir,
        "olivia",
        "acme/platform/api",
        "main",
        &[(".gitlab-ci.yml", ci), ("ci/build.yml", "build:\n  stage: build\n  script: [build]\n")],
        "init",
    )
    .await;
    let mut names: Vec<String> = jobs_of(&owner, project, 1).await.into_iter().map(|(n, _)| n).collect();
    names.sort();
    // a new branch: `changes` is always true (GitLab), so docs runs too
    assert_eq!(names, vec!["build", "deploy", "docs", "flaky", "lint", "test 1/2", "test 2/2"]);

    // a later push that touches no docs: no docs job
    push_files(&srv, &dir, "olivia", "acme/platform/api", "main", &[("src/a.rs", "fn a() {}")], "code").await;
    let names: Vec<String> = jobs_of(&owner, project, 2).await.into_iter().map(|(n, _)| n).collect();
    assert!(!names.contains(&"docs".to_string()), "{names:?}");
    assert!(!names.contains(&"mr-only".to_string()));

    // workflow:rules can drop a pipeline
    push_files(&srv, &dir, "olivia", "acme/platform/api", "main", &[("src/b.rs", "fn b() {}")], "wip skip-ci").await;
    let list = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines")).await;
    assert_eq!(list.as_array().unwrap().len(), 2, "skip-ci made no pipeline");

    // an include from a project the pusher cannot read fails the pipeline, without saying more
    must(git(&dir, &["checkout", "-q", "-b", "steal"]).await);
    push_files(
        &srv,
        &dir,
        "olivia",
        "acme/platform/api",
        "steal",
        &[(
            ".gitlab-ci.yml",
            "include:
  project: mallory/secret
  file: x.yml
job:
  script: x
",
        )],
        "steal",
    )
    .await;
    must(git(&dir, &["checkout", "-q", "main"]).await);
    let list = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines?ref=steal")).await;
    assert_eq!(list[0]["status"], "failed");
    assert!(list[0]["yaml_errors"].as_str().unwrap().contains("project not found"), "{}", list[0]);

    // merge request pipelines run the MR-only job (and not jobs without rules)
    must(git(&dir, &["checkout", "-q", "-b", "feature", "main"]).await);
    push_files(&srv, &dir, "olivia", "acme/platform/api", "feature", &[("src/c.rs", "fn c() {}")], "feature").await;
    let mr = owner
        .ok_post(
            &format!("/api/v1/projects/{project}/merge_requests"),
            json!({ "source_branch": "feature", "target_branch": "main", "title": "Feature" }),
        )
        .await;
    let pipes = owner.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{}/pipelines", mr["iid"])).await;
    let mr_pipe = pipes.as_array().unwrap().iter().find(|p| p["source"] == "merge_request_event").expect("an MR pipeline");
    assert!(mr_pipe["yaml_errors"].is_null(), "{}", mr_pipe["yaml_errors"]);
    let names: Vec<String> = jobs_of(&owner, project, mr_pipe["iid"].as_i64().unwrap()).await.into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, vec!["mr-only"]);

    // run pipeline 2 with a fake runner: `needs` lets test start before lint finishes
    let token = owner.ok_post(&format!("/api/v1/projects/{project}/runners"), json!({})).await["token"].as_str().unwrap().to_string();
    let take = |name: &'static str| {
        let base = srv.base.clone();
        let token = token.clone();
        async move {
            for _ in 0..20 {
                let r = reqwest::Client::new()
                    .post(format!("{base}/api/v4/jobs/request"))
                    .json(&json!({ "token": token, "system_id": "s" }))
                    .send()
                    .await
                    .unwrap();
                if r.status() == StatusCode::CREATED {
                    let j: Value = r.json().await.unwrap();
                    assert_eq!(j["job_info"]["name"], name, "unexpected job order");
                    return j;
                }
            }
            panic!("no job {name}");
        }
    };
    let done = |j: Value, state: &'static str, exit: i32| {
        let base = srv.base.clone();
        async move {
            let r = reqwest::Client::new()
                .put(format!("{base}/api/v4/jobs/{}", j["id"]))
                .json(&json!({ "token": j["token"], "state": state, "exit_code": exit, "failure_reason": if state == "failed" { "script_failure" } else { "" } }))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
        }
    };
    // pipelines 1 and 3.. also queued jobs: cancel all but pipeline 2
    for iid in [1, 3] {
        let _ = owner.post(&format!("/api/v1/projects/{project}/pipelines/{iid}/cancel"), json!({})).await;
    }
    for p in owner.ok_get(&format!("/api/v1/projects/{project}/pipelines")).await.as_array().unwrap() {
        if p["iid"] != 2 {
            let _ = owner.post(&format!("/api/v1/projects/{project}/pipelines/{}/cancel", p["iid"]), json!({})).await;
        }
    }
    let build = take("build").await;
    done(build, "success", 0).await;
    // build done: lint (stage test) and test 1/2, 2/2 (needs build) are all pending now
    let t1 = take("lint").await;
    let t2 = take("test 1/2").await;
    let v = t2["variables"].as_array().unwrap();
    assert!(v.iter().any(|x| x["key"] == "CI_NODE_INDEX" && x["value"] == "1"));
    done(t2, "success", 0).await;
    let t3 = take("test 2/2").await;
    done(t3, "success", 0).await;
    done(t1, "success", 0).await;
    // deploy is manual (waits); flaky runs, fails with 137 → retried once, then 3 → allowed
    let f1 = take("flaky").await;
    done(f1, "failed", 137).await;
    let f2 = take("flaky").await;
    assert_eq!(f2["job_info"]["name"], "flaky");
    done(f2, "failed", 3).await;
    let p = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/2")).await;
    let flaky: Vec<&Value> = p["jobs"].as_array().unwrap().iter().filter(|j| j["name"] == "flaky").collect();
    assert_eq!(flaky.len(), 2, "one automatic retry");
    let latest = flaky.iter().find(|j| j["retried"] == false).unwrap();
    assert_eq!(latest["allow_failure"], true, "exit code 3 is allowed");
    let deploy = p["jobs"].as_array().unwrap().iter().find(|j| j["name"] == "deploy").unwrap().clone();
    assert_eq!(deploy["status"], "manual");
    assert_eq!(p["status"], "success", "allowed failure + optional manual job: success");
    // play the manual job
    owner.ok_post(&format!("/api/v1/projects/{project}/jobs/{}/play", deploy["id"]), json!({})).await;
    let d = take("deploy").await;
    done(d, "success", 0).await;
    let p = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/2")).await;
    assert_eq!(p["status"], "success");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn job_token_scope(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    // a second project the same user owns
    let lib = f
        .owner
        .ok_post(
            "/api/v1/projects",
            json!({ "name": "lib", "path": "lib", "namespace_id": f.owner.ok_get("/api/v1/groups").await[0]["id"] }),
        )
        .await;
    let ldir = f.srv.work_dir("lib");
    seed_repo(&ldir).await;
    must(git(&ldir, &["push", "-q", &f.srv.http_url("olivia", lib["full_path"].as_str().unwrap()), "main"]).await);

    let token = new_runner(&f, json!({})).await;
    let (st, job) = request_job(&f, &token).await;
    assert_eq!(st, StatusCode::CREATED, "{job}");
    let jt = job["token"].as_str().unwrap();
    let lib_url = format!("http://gitlab-ci-token:{jt}@{}/{}.git", f.srv.host(), lib["full_path"].as_str().unwrap());
    let dir = f.srv.work_dir("probe");
    must(git(&dir, &["init", "-q"]).await);
    assert!(!git(&dir, &["ls-remote", &lib_url]).await.ok, "other projects are closed to job tokens by default");
    f.owner.ok_post(&format!("/api/v1/projects/{}/job_token_allowlist", lib["id"]), json!({ "project": "acme/platform/api" })).await;
    must(git(&dir, &["ls-remote", &lib_url]).await);
    // read-only: a job token never pushes
    must(git(&dir, &["fetch", "-q", &lib_url, "main"]).await);
    assert!(!git(&dir, &["push", "-q", &lib_url, "FETCH_HEAD:refs/heads/evil"]).await.ok);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn schedules_and_merge_check(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let base = format!("/api/v1/projects/{}/pipeline_schedules", f.project);
    let (s, _) = f.owner.post(&base, json!({ "ref": "main", "cron": "0 0 3 * * *" })).await;
    assert_eq!(s, 400, "seconds field refused");
    let (s, _) = f.owner.post(&base, json!({ "ref": "nope", "cron": "0 3 * * *" })).await;
    assert_eq!(s, 400);
    let sched = f.owner.ok_post(&base, json!({ "ref": "main", "cron": "0 3 * * *", "cron_timezone": "Europe/Paris", "variables": [{ "key": "NIGHTLY", "value": "1" }] })).await;
    assert!(sched["next_run_at"].is_string());
    let sid = sched["id"].as_i64().unwrap();

    // a due schedule runs once from the sweep, then moves to its next slot
    sqlx::query("UPDATE ci_pipeline_schedules SET next_run_at = now() - interval '3 days' WHERE id = $1")
        .bind(sid)
        .execute(&f.srv.state.db)
        .await
        .unwrap();
    forgust_server::ci::sweep(&f.srv.state).await.unwrap();
    forgust_server::ci::sweep(&f.srv.state).await.unwrap();
    let list = f.owner.ok_get(&format!("/api/v1/projects/{}/pipelines", f.project)).await;
    let scheduled: Vec<&Value> = list.as_array().unwrap().iter().filter(|p| p["source"] == "schedule").collect();
    assert_eq!(scheduled.len(), 1, "missed slots run once");
    let after = f.owner.ok_get(&base).await;
    assert!(after[0]["next_run_at"].as_str().unwrap() > chrono_now().as_str());
    assert_eq!(after[0]["last_pipeline"][0], scheduled[0]["iid"]);

    // the schedule's variables reach its jobs
    let token = new_runner(&f, json!({})).await;
    let mut found = false;
    for _ in 0..4 {
        let (s, job) = request_job(&f, &token).await;
        if s == StatusCode::CREATED && var(&job, "NIGHTLY").is_some() {
            assert_eq!(var(&job, "CI_PIPELINE_SOURCE").unwrap()["value"], "schedule");
            found = true;
            break;
        }
    }
    assert!(found, "a job of the scheduled pipeline carries NIGHTLY");

    // play now
    let r = f.owner.ok_post(&format!("{base}/{sid}/play"), json!({})).await;
    assert!(r["pipeline_iid"].is_number());

    // "pipelines must succeed" blocks the merge while the pipeline is not green
    let work = f.srv.work_dir("ci");
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    std::fs::write(work.join("x.txt"), "x").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "topic"]).await);
    must(git(&work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&f.srv.state).await;
    let mut settings = f.owner.ok_get(&format!("/api/v1/projects/{}/ci/settings", f.project)).await;
    settings["only_merge_if_pipeline_succeeds"] = json!(true);
    f.owner.put(&format!("/api/v1/projects/{}/ci/settings", f.project), settings).await;
    let mr = f
        .owner
        .ok_post(
            &format!("/api/v1/projects/{}/merge_requests", f.project),
            json!({ "source_branch": "topic", "target_branch": "main", "title": "t" }),
        )
        .await;
    let st = f.owner.ok_get(&format!("/api/v1/projects/{}/merge_requests/{}/merge_status", f.project, mr["iid"])).await;
    assert_eq!(st["can_merge"], false);
    assert_eq!(st["pipeline_required"], true);
    assert!(st["blockers"].to_string().contains("pipeline must succeed"), "{st}");
}

fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// Regressions from the M4 review.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn review_regressions(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let pid = f.project;
    let dev = f.srv.user("dave", false).await;
    f.owner.ok_post(&format!("/api/v1/projects/{pid}/members"), json!({ "username": "dave", "access_level": "developer" })).await;

    // "no one" may push to main, maintainers may merge: a developer may not run pipelines there
    f.owner
        .ok_post(
            &format!("/api/v1/projects/{pid}/protected_branches"),
            json!({ "name": "main", "push_access_level": "no_one", "merge_access_level": "maintainer" }),
        )
        .await;
    let (s, _) = dev.post(&format!("/api/v1/projects/{pid}/pipelines"), json!({ "ref": "main" })).await;
    assert_eq!(s, 403, "level 0 must not let developers through");
    assert_eq!(f.owner.post(&format!("/api/v1/projects/{pid}/pipelines"), json!({ "ref": "main" })).await.0, 200);

    // a developer cannot take over a maintainer's schedule by editing it
    let sched = f.owner.ok_post(&format!("/api/v1/projects/{pid}/pipeline_schedules"), json!({ "ref": "main", "cron": "0 3 * * *" })).await;
    let work = f.srv.work_dir("ci");
    must(git(&work, &["checkout", "-q", "-b", "dev-branch"]).await);
    must(git(&work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "dev-branch"]).await);
    wait_jobs(&f.srv.state).await;
    let (s, _) = dev
        .put(&format!("/api/v1/projects/{pid}/pipeline_schedules/{}", sched["id"]), json!({ "ref": "dev-branch", "cron": "* * * * *" }))
        .await;
    assert_eq!(s, 403);

    // a manual job that was never played stays manual after cancel + retry
    std::fs::write(work.join(".gitlab-ci.yml"), "build:\n  script: x\ndeploy:\n  stage: deploy\n  script: x\n  when: manual\n").unwrap();
    must(git(&work, &["commit", "-qam", "manual"]).await);
    must(git(&work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "dev-branch"]).await);
    wait_jobs(&f.srv.state).await;
    let list = f.owner.ok_get(&format!("/api/v1/projects/{pid}/pipelines?ref=dev-branch")).await;
    let iid = list[0]["iid"].as_i64().unwrap();
    let pipeline_id = list[0]["id"].clone();
    let token = new_runner(&f, json!({})).await;
    // finish build so deploy becomes manual
    loop {
        let (s, j) = request_job(&f, &token).await;
        assert_eq!(s, StatusCode::CREATED);
        let done = j["job_info"]["pipeline_id"] == pipeline_id && j["job_info"]["name"] == "build";
        put(&f, j["id"].as_i64().unwrap(), json!({ "token": j["token"], "state": "success" })).await;
        if done {
            break;
        }
    }
    let p = f.owner.ok_get(&format!("/api/v1/projects/{pid}/pipelines/{iid}")).await;
    assert!(p["jobs"].as_array().unwrap().iter().any(|j| j["name"] == "deploy" && j["status"] == "manual"), "{p}");
    f.owner.ok_post(&format!("/api/v1/projects/{pid}/pipelines/{iid}/cancel"), json!({})).await;
    f.owner.ok_post(&format!("/api/v1/projects/{pid}/pipelines/{iid}/retry"), json!({})).await;
    let p = f.owner.ok_get(&format!("/api/v1/projects/{pid}/pipelines/{iid}")).await;
    let deploy = p["jobs"].as_array().unwrap().iter().find(|j| j["name"] == "deploy" && j["retried"] == false).unwrap();
    assert_eq!(deploy["status"], "manual", "a never-played manual job must not start by itself");

    // a broken config makes a failed pipeline without pinning its commit
    std::fs::write(work.join(".gitlab-ci.yml"), "job: [not, a, hash]\n").unwrap();
    must(git(&work, &["commit", "-qam", "broken"]).await);
    must(git(&work, &["push", "-q", &f.srv.http_url("olivia", "acme/platform/api"), "dev-branch"]).await);
    wait_jobs(&f.srv.state).await;
    let list = f.owner.ok_get(&format!("/api/v1/projects/{pid}/pipelines?ref=dev-branch")).await;
    assert_eq!(list[0]["status"], "failed");
    let pin = format!("refs/pipelines/{}", list[0]["id"]);
    assert!(f.srv.state.storage.resolve(pid, &pin).await.unwrap().is_none());

    // unknown expire_in units get the default expiry, not "forever"
    f.owner.post(&format!("/api/v1/projects/{pid}/pipelines/1/cancel"), json!({})).await;
    let r = f.owner.ok_post(&format!("/api/v1/projects/{pid}/pipelines"), json!({ "ref": "main" })).await;
    let mut job = Value::Null;
    for _ in 0..10 {
        let (s, j) = request_job(&f, &token).await;
        if s == StatusCode::CREATED && j["job_info"]["pipeline_id"] == r["id"] {
            job = j;
            break;
        }
        if s == StatusCode::CREATED {
            put(&f, j["id"].as_i64().unwrap(), json!({ "token": j["token"], "state": "success" })).await;
        }
    }
    let id = job["id"].as_i64().expect("a job of the new pipeline");
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(b"zip".to_vec()).file_name("a.zip"));
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/{id}/artifacts?expire_in=2+fortnights", f.srv.base))
        .header("JOB-TOKEN", job["token"].as_str().unwrap())
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let days: f64 =
        sqlx::query_scalar("SELECT extract(epoch FROM expire_at - now())::float8 / 86400 FROM ci_job_artifacts WHERE build_id = $1")
            .bind(id)
            .fetch_one(&f.srv.state.db)
            .await
            .unwrap();
    assert!((29.0..31.0).contains(&days), "expected the 30-day default, got {days}");
}

/// Caches never cross between protected and unprotected refs, and a blocked
/// user's running jobs lose the user's access.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn cache_scopes_and_blocked_job_tokens(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let work = f.srv.work_dir("ci");
    let ci = "cached:\n  tags: [cache]\n  script: [\"true\"]\n  cache:\n    key: deps\n    fallback_keys: [base]\n    paths: [vendor]\n";
    push_files(&f.srv, &work, "olivia", "acme/platform/api", "main", &[(".gitlab-ci.yml", ci)], "cache on main").await;
    push_files(&f.srv, &work, "olivia", "acme/platform/api", "feature", &[("x.txt", "x")], "feature").await;
    let token = new_runner(&f, json!({ "tags": ["cache"], "run_untagged": false })).await;
    let mut seen = std::collections::BTreeMap::new();
    let mut job_token = String::new();
    for _ in 0..2 {
        let (st, job) = request_job(&f, &token).await;
        assert_eq!(st, StatusCode::CREATED, "{job}");
        let r = job["git_info"]["ref"].as_str().unwrap().to_string();
        seen.insert(r, (job["cache"][0]["key"].clone(), job["cache"][0]["fallback_keys"].clone()));
        job_token = job["token"].as_str().unwrap().to_string();
    }
    assert_eq!(seen["main"], (json!("deps-protected"), json!(["base-protected"])));
    assert_eq!(seen["feature"], (json!("deps-non_protected"), json!(["base-non_protected"])));

    let url = format!("http://gitlab-ci-token:{job_token}@{}/acme/platform/api.git", f.srv.host());
    let dir = f.srv.work_dir("probe");
    must(git(&dir, &["init", "-q"]).await);
    must(git(&dir, &["ls-remote", &url]).await);
    let me = f.owner.ok_get("/api/v1/user").await;
    forgust_db::users::set_state(&f.srv.state.db, me["id"].as_i64().unwrap(), "blocked").await.unwrap();
    assert!(!git(&dir, &["ls-remote", &url]).await.ok, "a blocked user's job token must stop working");
}

/// A merge request's pipeline is protected only when its source and target
/// branches both are (a merge request from protected `main` into a branch of
/// one's own must not run that branch's config with protected secrets).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn merge_request_pipelines_are_protected_only_between_protected_branches(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let (_org, _sub, pid) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{pid}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    let work = srv.work_dir("prot");
    seed_repo(&work).await;
    let ci = "t:\n  script: [echo]\n  rules:\n    - if: $CI_PIPELINE_SOURCE == \"merge_request_event\"\n    - if: $CI_COMMIT_BRANCH\n";
    commit_file(&work, ".gitlab-ci.yml", ci, "ci").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&format!("/api/v1/projects/{pid}/repository/branches"), json!({ "branch": "mine", "ref": "main" })).await;
    // a developer's merge request from main gets no pipeline (they may not run main's);
    // a maintainer's gets one, unprotected: the target is not protected
    let mr = owner
        .ok_post(
            &format!("/api/v1/projects/{pid}/merge_requests"),
            json!({ "source_branch": "main", "target_branch": "mine", "title": "x" }),
        )
        .await;
    wait_jobs(&srv.state).await;
    let list = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/{}/pipelines", mr["iid"])).await;
    let pls: Vec<&serde_json::Value> = list.as_array().unwrap().iter().filter(|p| p["source"] == "merge_request_event").collect();
    assert!(!pls.is_empty(), "{list}");
    assert!(pls.iter().all(|p| p["protected"] == false), "{list}");
    let main = owner.ok_get(&format!("/api/v1/projects/{pid}/pipelines?ref=main")).await;
    assert!(
        main.as_array().unwrap().iter().any(|p| p["protected"] == true && p["source"] == "push"),
        "main's own pipeline stays protected: {main}"
    );
}
