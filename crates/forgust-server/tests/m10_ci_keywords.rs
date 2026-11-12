//! CI keywords GitLab pipelines use, driven by a fake runner over HTTP:
//! environments and deployments, protected environments, resource groups,
//! delayed jobs, coverage, `release:`, child and multi-project pipelines,
//! `on_job_failure`, `id_tokens`, components and inputs, `include:rules`,
//! `rules:changes:compare_to` and `rules:exists:project`.

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

struct Fx {
    srv: TestServer,
    owner: Client,
    project: i64,
    work: std::path::PathBuf,
    runner: String,
}

async fn fixture(pool: sqlx::PgPool, files: &[(&str, &str)]) -> Fx {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let runner = owner.ok_post(&format!("/api/v1/projects/{project}/runners"), json!({ "description": "fake" })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let work = srv.work_dir("ci10");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    for (path, content) in files {
        let p = work.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "ci"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    Fx { srv, owner, project, work, runner }
}

async fn request(fx: &Fx) -> Option<Value> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", fx.srv.base))
        .json(&json!({ "token": fx.runner, "system_id": "s_k", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    if r.status() == StatusCode::CREATED { Some(r.json().await.unwrap()) } else { None }
}

async fn finish(fx: &Fx, job: &Value, state: &str) {
    let r = reqwest::Client::new()
        .put(format!("{}/api/v4/jobs/{}", fx.srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": state, "exit_code": if state == "success" { 0 } else { 1 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    wait_jobs(&fx.srv.state).await;
}

async fn log(fx: &Fx, job: &Value, text: &str) {
    let r = reqwest::Client::new()
        .patch(format!("{}/api/v4/jobs/{}/trace", fx.srv.base, job["id"]))
        .header("JOB-TOKEN", job["token"].as_str().unwrap())
        .header("Content-Range", format!("0-{}", text.len() - 1))
        .body(text.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
}

fn var(job: &Value, k: &str) -> Option<String> {
    job["variables"].as_array().unwrap().iter().find(|v| v["key"] == k).map(|v| v["value"].as_str().unwrap().to_string())
}

async fn pipeline(fx: &Fx, iid: i64) -> Value {
    fx.owner.ok_get(&format!("/api/v1/projects/{}/pipelines/{iid}", fx.project)).await
}

fn job_named<'a>(p: &'a Value, name: &str) -> &'a Value {
    p["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|j| j["name"] == name && j["retried"] == false)
        .unwrap_or_else(|| panic!("no job {name} in {p}"))
}

async fn push_ci(fx: &Fx, ci: &str) {
    std::fs::write(fx.work.join(".gitlab-ci.yml"), ci).unwrap();
    must(git(&fx.work, &["add", "."]).await);
    must(git(&fx.work, &["commit", "-q", "-m", "ci change"]).await);
    must(git(&fx.work, &["push", "-q", &fx.srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&fx.srv.state).await;
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn environments_deployments_and_stopping(pool: sqlx::PgPool) {
    let ci = "deploy:\n  script: [deploy]\n  environment:\n    name: review/$CI_COMMIT_REF_SLUG\n    url: https://$CI_COMMIT_REF_SLUG.example.com\n    on_stop: stop\n    auto_stop_in: 1 week\nstop:\n  script: [teardown]\n  when: manual\n  environment:\n    name: review/$CI_COMMIT_REF_SLUG\n    action: stop\n";
    let fx = fixture(pool, &[(".gitlab-ci.yml", ci)]).await;
    let job = request(&fx).await.expect("the deploy job");
    assert_eq!(var(&job, "CI_ENVIRONMENT_NAME").as_deref(), Some("review/main"));
    assert_eq!(var(&job, "CI_ENVIRONMENT_URL").as_deref(), Some("https://main.example.com"));
    assert_eq!(var(&job, "CI_ENVIRONMENT_TIER").as_deref(), Some("development"));
    assert_eq!(var(&job, "CI_ENVIRONMENT_ACTION").as_deref(), Some("start"));
    let envs = fx.owner.ok_get(&format!("/api/v1/projects/{}/environments", fx.project)).await;
    assert_eq!(envs[0]["name"], "review/main");
    assert_eq!(envs[0]["last_deployment"]["status"], "running");
    finish(&fx, &job, "success").await;
    let envs = fx.owner.ok_get(&format!("/api/v1/projects/{}/environments", fx.project)).await;
    let env = &envs[0];
    assert_eq!((env["state"].as_str(), env["external_url"].as_str()), (Some("available"), Some("https://main.example.com")));
    assert_eq!(env["last_deployment"]["status"], "success");
    assert!(env["auto_stop_at"].is_string() && env["stop_build_id"].is_i64(), "{env}");
    // stopping it runs the stop job
    let eid = env["id"].as_i64().unwrap();
    fx.owner.ok_post(&format!("/api/v1/projects/{}/environments/{eid}/stop", fx.project), json!({})).await;
    let detail = fx.owner.ok_get(&format!("/api/v1/projects/{}/environments/{eid}", fx.project)).await;
    assert_eq!(detail["state"], "stopping");
    let stop = request(&fx).await.expect("the stop job");
    assert_eq!(stop["job_info"]["name"], "stop");
    finish(&fx, &stop, "success").await;
    let detail = fx.owner.ok_get(&format!("/api/v1/projects/{}/environments/{eid}", fx.project)).await;
    assert_eq!(detail["state"], "stopped");
    assert_eq!(detail["deployments"].as_array().unwrap().len(), 1);
    // a stopped environment can be deleted
    assert_eq!(fx.owner.delete(&format!("/api/v1/projects/{}/environments/{eid}", fx.project)).await.0, 200);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn protected_environments_and_resource_groups(pool: sqlx::PgPool) {
    let ci = "deploy:\n  script: [deploy]\n  environment: production\n  resource_group: production\n";
    let fx = fixture(pool, &[("README.md", "x\n")]).await;
    // only Maintainers deploy to production
    fx.owner
        .ok_post(
            &format!("/api/v1/projects/{}/protected_environments", fx.project),
            json!({ "name": "production", "deploy_access_level": "maintainer" }),
        )
        .await;
    srv_member(&fx, "dave", "developer").await;
    fx.owner
        .ok_post(
            &format!("/api/v1/projects/{}/protected_branches", fx.project),
            json!({ "name": "main", "push_access_level": "developer" }),
        )
        .await;
    std::fs::write(fx.work.join(".gitlab-ci.yml"), ci).unwrap();
    must(git(&fx.work, &["add", "."]).await);
    must(git(&fx.work, &["commit", "-q", "-m", "ci"]).await);
    must(git(&fx.work, &["push", "-q", &fx.srv.http_url("dave", REPO), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    let p = pipeline(&fx, 1).await;
    let d = job_named(&p, "deploy");
    assert_eq!((d["status"].as_str(), d["failure_reason"].as_str()), (Some("failed"), Some("protected_environment_failure")), "{d}");

    // Maintainers do; one deploy at a time
    commit_file(&fx.work, "a.txt", "1\n", "one").await;
    must(git(&fx.work, &["push", "-q", &fx.srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    commit_file(&fx.work, "a.txt", "2\n", "two").await;
    must(git(&fx.work, &["push", "-q", &fx.srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    // pipeline 3 makes pipeline 2 redundant (canceled): its deploy lets 3's go
    let (p2, p3) = (pipeline(&fx, 2).await, pipeline(&fx, 3).await);
    assert_eq!(job_named(&p2, "deploy")["status"], "canceled");
    assert_eq!(job_named(&p3, "deploy")["status"], "pending", "the canceled job freed the resource group");
}

async fn srv_member(fx: &Fx, user: &str, role: &str) {
    fx.srv.user(user, false).await;
    fx.owner.ok_post(&format!("/api/v1/projects/{}/members", fx.project), json!({ "username": user, "access_level": role })).await;
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_resource_group_runs_one_job_at_a_time(pool: sqlx::PgPool) {
    let ci = "a:\n  script: [x]\n  resource_group: db\nb:\n  script: [y]\n  resource_group: db\nc:\n  script: [z]\n";
    let fx = fixture(pool, &[(".gitlab-ci.yml", ci)]).await;
    let p = pipeline(&fx, 1).await;
    assert_eq!(job_named(&p, "a")["status"], "pending");
    assert_eq!(job_named(&p, "b")["status"], "waiting_for_resource");
    assert_eq!(job_named(&p, "c")["status"], "pending");
    let first = request(&fx).await.unwrap();
    let second = request(&fx).await.unwrap();
    let names = [first["job_info"]["name"].clone(), second["job_info"]["name"].clone()];
    assert!(names.contains(&json!("a")) && names.contains(&json!("c")), "{names:?}");
    assert!(request(&fx).await.is_none(), "b waits");
    let a = if first["job_info"]["name"] == "a" { &first } else { &second };
    finish(&fx, a, "success").await;
    assert_eq!(job_named(&pipeline(&fx, 1).await, "b")["status"], "pending");
    let b = request(&fx).await.expect("b runs now");
    assert_eq!(b["job_info"]["name"], "b");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn delayed_jobs_wait_then_run(pool: sqlx::PgPool) {
    let ci = "later:\n  script: [x]\n  when: delayed\n  start_in: 1 hour\nlater2:\n  script: [y]\n  when: delayed\n  start_in: 10 minutes\nlater3:\n  script: [z]\n  when: delayed\n  start_in: 5 minutes\n";
    let fx = fixture(pool, &[(".gitlab-ci.yml", ci)]).await;
    let p = pipeline(&fx, 1).await;
    assert_eq!(p["status"], "scheduled");
    let later = job_named(&p, "later");
    assert_eq!(later["status"], "scheduled");
    let at = chrono::DateTime::parse_from_rfc3339(later["scheduled_at"].as_str().unwrap()).unwrap();
    let wait = at.signed_duration_since(chrono::Utc::now()).num_minutes();
    assert!((58..=60).contains(&wait), "{wait} minutes");
    assert!(request(&fx).await.is_none());
    // its time comes
    sqlx::query("UPDATE ci_builds SET scheduled_at = now() - interval '1 second' WHERE name = 'later'")
        .execute(&fx.srv.state.db)
        .await
        .unwrap();
    forgust_server::ci::sweep(&fx.srv.state).await.unwrap();
    assert_eq!(request(&fx).await.unwrap()["job_info"]["name"], "later");
    // someone runs one now, and holds the other
    let base = format!("/api/v1/projects/{}/jobs", fx.project);
    fx.owner.ok_post(&format!("{base}/{}/play", job_named(&p, "later2")["id"]), json!({})).await;
    assert_eq!(request(&fx).await.unwrap()["job_info"]["name"], "later2");
    let held = fx.owner.ok_post(&format!("{base}/{}/unschedule", job_named(&p, "later3")["id"]), json!({})).await;
    assert_eq!(held["status"], "manual");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn coverage_release_and_failure_cancels(pool: sqlx::PgPool) {
    // no tag pipelines: the release makes a tag
    let ci = "workflow:\n  rules:\n    - if: $CI_COMMIT_TAG\n      when: never\n    - when: always\n  auto_cancel:\n    on_job_failure: all\nstages: [test, release]\ntest:\n  script: [make test]\n  coverage: '/TOTAL.*\\s(\\d+(?:\\.\\d+)?)%/'\nrelease:\n  stage: release\n  script: [echo]\n  release:\n    tag_name: v$CI_PIPELINE_IID.0\n    name: Release $CI_PIPELINE_IID\n    description: Built by $CI_JOB_NAME\n";
    let fx = fixture(pool, &[(".gitlab-ci.yml", ci)]).await;
    let test = request(&fx).await.unwrap();
    log(&fx, &test, "running tests\nTOTAL    120    18    85.50%\n").await;
    finish(&fx, &test, "success").await;
    let p = pipeline(&fx, 1).await;
    assert_eq!(job_named(&p, "test")["coverage"], 85.5);
    assert_eq!(p["coverage"], 85.5);
    let rel = request(&fx).await.unwrap();
    finish(&fx, &rel, "success").await;
    let r = fx.owner.ok_get(&format!("/api/v1/projects/{}/releases/v1.0", fx.project)).await;
    assert_eq!((r["name"].as_str(), r["description"].as_str()), (Some("Release 1"), Some("Built by release")));
    assert!(fx.srv.state.storage.resolve(fx.project, "refs/tags/v1.0").await.unwrap().is_some(), "the tag was created");

    // on_job_failure: all: a failed job cancels the rest
    push_ci(&fx, "workflow:\n  auto_cancel:\n    on_job_failure: all\na:\n  script: [x]\nb:\n  script: [y]\n").await;
    let first = request(&fx).await.unwrap();
    finish(&fx, &first, "failed").await;
    let p = pipeline(&fx, 2).await;
    let other = if first["job_info"]["name"] == "a" { "b" } else { "a" };
    assert_eq!(job_named(&p, other)["status"], "canceled", "{p}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn child_pipelines_and_strategy_depend(pool: sqlx::PgPool) {
    let ci = "child:\n  trigger:\n    include: ci/child.yml\n    strategy: depend\n  variables:\n    FROM_PARENT: hello\n";
    let child = "unit:\n  script: [make unit]\n";
    let fx = fixture(pool, &[(".gitlab-ci.yml", ci), ("ci/child.yml", child)]).await;
    let p = pipeline(&fx, 1).await;
    let bridge = job_named(&p, "child");
    assert_eq!((bridge["bridge"].as_bool(), bridge["status"].as_str()), (Some(true), Some("running")), "{bridge}");
    let down = &bridge["downstream"];
    assert_eq!((down["iid"].as_i64(), down["source"].as_str()), (Some(2), Some("parent_pipeline")), "{bridge}");
    let job = request(&fx).await.expect("the child's job");
    assert_eq!(job["job_info"]["name"], "unit");
    assert_eq!(var(&job, "FROM_PARENT").as_deref(), Some("hello"));
    assert_eq!(var(&job, "CI_PIPELINE_SOURCE").as_deref(), Some("parent_pipeline"));
    assert_eq!(var(&job, "CI_PARENT_PIPELINE_ID").as_deref(), Some(p["id"].to_string().as_str()));
    let childp = pipeline(&fx, 2).await;
    assert_eq!(childp["upstream"]["iid"], 1);
    finish(&fx, &job, "success").await;
    let p = pipeline(&fx, 1).await;
    assert_eq!(job_named(&p, "child")["status"], "success");
    assert_eq!(p["status"], "success");

    // canceling the parent cancels the child, and the bridge ends canceled once
    // the child's running job stops (a bridge has no runner to report it)
    push_ci(&fx, &format!("{ci}# again\n")).await;
    let job = request(&fx).await.expect("the second child's job");
    let p = pipeline(&fx, 3).await;
    fx.owner.ok_post(&format!("/api/v1/projects/{}/pipelines/3/cancel", fx.project), json!({})).await;
    let childp = pipeline(&fx, 4).await;
    assert_eq!((childp["upstream"]["iid"].as_i64(), childp["status"].as_str()), (Some(3), Some("running")), "{childp}");
    assert_eq!(job_named(&childp, "unit")["status"], "canceling");
    finish(&fx, &job, "failed").await;
    let childp = pipeline(&fx, 4).await;
    assert_eq!(childp["status"], "canceled", "{childp}");
    let p2 = pipeline(&fx, 3).await;
    assert_eq!((job_named(&p2, "child")["status"].as_str(), p2["status"].as_str()), (Some("canceled"), Some("canceled")), "{p} {p2}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn multi_project_pipelines(pool: sqlx::PgPool) {
    let fx = fixture(pool, &[("README.md", "x\n")]).await;
    // a second project with its own config
    let g = fx.owner.ok_post("/api/v1/groups", json!({ "name": "Other", "path": "other" })).await;
    let two = fx.owner.ok_post("/api/v1/projects", json!({ "name": "Two", "path": "two", "namespace_id": g["id"] })).await;
    let w2 = fx.srv.work_dir("two");
    must(git(&w2, &["init", "-q", "-b", "main"]).await);
    std::fs::write(w2.join(".gitlab-ci.yml"), "downstream:\n  script: [echo $UPSTREAM_VAR]\n").unwrap();
    must(git(&w2, &["add", "."]).await);
    must(git(&w2, &["commit", "-q", "-m", "ci"]).await);
    // no pipeline for this push: the config arrives with the push but is checked later
    must(git(&w2, &["push", "-q", &fx.srv.http_url("olivia", "other/two"), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    push_ci(&fx, "bridge:\n  trigger: other/two\n  variables:\n    UPSTREAM_VAR: hi\n").await;
    let p = pipeline(&fx, 1).await;
    let b = job_named(&p, "bridge");
    assert_eq!(b["status"], "success", "without strategy: depend, the bridge succeeds once the pipeline exists");
    assert_eq!(b["downstream"]["project_path"], "other/two");
    let dp = fx.owner.ok_get(&format!("/api/v1/projects/{}/pipelines/{}", two["id"], b["downstream"]["iid"])).await;
    assert_eq!(dp["source"], "pipeline");
    assert_eq!(dp["upstream"]["project_path"], REPO);

    // someone who cannot run pipelines there: the bridge fails
    srv_member(&fx, "dave", "developer").await;
    fx.owner
        .ok_post(
            &format!("/api/v1/projects/{}/protected_branches", fx.project),
            json!({ "name": "main", "push_access_level": "developer" }),
        )
        .await;
    commit_file(&fx.work, "x.txt", "x\n", "dave's").await;
    must(git(&fx.work, &["push", "-q", &fx.srv.http_url("dave", REPO), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    let p = pipeline(&fx, 2).await;
    let b = job_named(&p, "bridge");
    assert_eq!((b["status"].as_str(), b["failure_reason"].as_str()), (Some("failed"), Some("downstream_bridge_project_not_found")), "{b}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn id_tokens_are_signed_jwts(pool: sqlx::PgPool) {
    use base64::Engine;
    let ci = "deploy:\n  script: [x]\n  environment: production\n  id_tokens:\n    AWS_TOKEN:\n      aud: https://sts.amazonaws.com\n";
    let fx = fixture(pool, &[(".gitlab-ci.yml", ci)]).await;
    let job = request(&fx).await.unwrap();
    let jwt = var(&job, "AWS_TOKEN").expect("the token variable");
    let masked = job["variables"].as_array().unwrap().iter().find(|v| v["key"] == "AWS_TOKEN").unwrap()["masked"].clone();
    assert_eq!(masked, true);
    let parts: Vec<&str> = jwt.split('.').collect();
    let dec = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap();
    let head: Value = serde_json::from_slice(&dec(parts[0])).unwrap();
    let claims: Value = serde_json::from_slice(&dec(parts[1])).unwrap();
    assert_eq!(head["alg"], "ES256");
    assert_eq!(claims["aud"], "https://sts.amazonaws.com");
    assert_eq!(claims["iss"], fx.srv.base);
    assert_eq!(claims["sub"], format!("project_path:{REPO}:ref_type:branch:ref:main"));
    assert_eq!((claims["project_path"].as_str(), claims["user_login"].as_str()), (Some(REPO), Some("olivia")));
    assert_eq!((claims["environment"].as_str(), claims["deployment_tier"].as_str()), (Some("production"), Some("production")));
    // the signature checks out against the published key
    let disco: Value = reqwest::get(format!("{}/.well-known/openid-configuration", fx.srv.base)).await.unwrap().json().await.unwrap();
    assert_eq!(disco["issuer"], fx.srv.base);
    let jwks: Value = reqwest::get(disco["jwks_uri"].as_str().unwrap()).await.unwrap().json().await.unwrap();
    let k = &jwks["keys"][0];
    assert_eq!(k["kid"], head["kid"]);
    let mut point = vec![4u8];
    point.extend(dec(k["x"].as_str().unwrap()));
    point.extend(dec(k["y"].as_str().unwrap()));
    let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, point);
    key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &dec(parts[2])).expect("a valid signature");
    assert!(key.verify(format!("{}.{}x", parts[0], parts[1]).as_bytes(), &dec(parts[2])).is_err());
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn components_inputs_and_include_rules(pool: sqlx::PgPool) {
    let fx = fixture(pool, &[("README.md", "x\n")]).await;
    // a component project with a release
    let g = fx.owner.ok_post("/api/v1/groups", json!({ "name": "Components", "path": "components" })).await;
    let comp = fx.owner.ok_post("/api/v1/projects", json!({ "name": "Build", "path": "build", "namespace_id": g["id"] })).await;
    let wc = fx.srv.work_dir("comp");
    must(git(&wc, &["init", "-q", "-b", "main"]).await);
    std::fs::create_dir_all(wc.join("templates")).unwrap();
    std::fs::write(
        wc.join("templates/compile.yml"),
        "spec:\n  inputs:\n    lang:\n      options: [rust, go]\n    stage:\n      default: test\n---\n\"compile-$[[ inputs.lang ]]\":\n  stage: $[[ inputs.stage ]]\n  script:\n    - echo $[[ inputs.lang ]]\n",
    )
    .unwrap();
    must(git(&wc, &["add", "."]).await);
    must(git(&wc, &["commit", "-q", "-m", "component"]).await);
    must(git(&wc, &["push", "-q", &fx.srv.http_url("olivia", "components/build"), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    fx.owner
        .ok_post(&format!("/api/v1/projects/{}/releases", comp["id"]), json!({ "tag_name": "v1.0.0", "ref": "main", "name": "v1" }))
        .await;
    let host = fx.srv.host();
    let ci = format!(
        "include:\n  - component: {host}/components/build/compile@v1.0.0\n    inputs:\n      lang: rust\n  - component: $CI_SERVER_FQDN/components/build/compile@~latest\n    inputs:\n      lang: go\n      stage: build\n  - local: extra.yml\n    rules:\n      - if: $CI_COMMIT_BRANCH == \"nope\"\n  - local: kept.yml\n    rules:\n      - exists: [README.md]\nstages: [build, test]\n"
    );
    std::fs::write(fx.work.join("extra.yml"), "extra:\n  script: [x]\n").unwrap();
    std::fs::write(fx.work.join("kept.yml"), "kept:\n  script: [x]\n").unwrap();
    std::fs::write(fx.work.join(".gitlab-ci.yml"), ci).unwrap();
    must(git(&fx.work, &["add", "."]).await);
    must(git(&fx.work, &["commit", "-q", "-m", "components"]).await);
    must(git(&fx.work, &["push", "-q", &fx.srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    let p = pipeline(&fx, 1).await;
    assert!(p["yaml_errors"].is_null(), "{p}");
    let mut names: Vec<(String, String)> = p["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| (j["name"].as_str().unwrap().to_string(), j["stage"].as_str().unwrap().to_string()))
        .collect();
    names.sort();
    assert_eq!(names, vec![("compile-go".into(), "build".into()), ("compile-rust".into(), "test".into()), ("kept".into(), "test".into())]);
    // a bad input is an error on the pipeline
    push_ci(&fx, &format!("include:\n  - component: {host}/components/build/compile@v1.0.0\n    inputs:\n      lang: java\n")).await;
    let p = pipeline(&fx, 2).await;
    assert!(p["yaml_errors"].as_str().unwrap_or("").contains("options"), "{p}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn compare_to_and_exists_in_another_project(pool: sqlx::PgPool) {
    let fx = fixture(pool, &[("README.md", "x\n"), ("src/a.rs", "a\n")]).await;
    let g = fx.owner.ok_post("/api/v1/groups", json!({ "name": "Shared", "path": "shared" })).await;
    fx.owner.ok_post("/api/v1/projects", json!({ "name": "Conf", "path": "conf", "namespace_id": g["id"] })).await;
    let ws = fx.srv.work_dir("conf");
    must(git(&ws, &["init", "-q", "-b", "main"]).await);
    std::fs::write(ws.join("deploy.toml"), "x\n").unwrap();
    must(git(&ws, &["add", "."]).await);
    must(git(&ws, &["commit", "-q", "-m", "conf"]).await);
    must(git(&ws, &["push", "-q", &fx.srv.http_url("olivia", "shared/conf"), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    let ci = "src:\n  script: [x]\n  rules:\n    - changes:\n        paths: [src/**]\n        compare_to: refs/heads/main\ndocs:\n  script: [x]\n  rules:\n    - changes:\n        paths: [docs/**]\n        compare_to: refs/heads/main\nshared:\n  script: [x]\n  rules:\n    - exists:\n        paths: [deploy.toml]\n        project: shared/conf\nmissing:\n  script: [x]\n  rules:\n    - exists:\n        paths: [nope.toml]\n        project: shared/conf\n        ref: main\n";
    push_ci(&fx, ci).await;
    must(git(&fx.work, &["checkout", "-q", "-b", "feature"]).await);
    commit_file(&fx.work, "src/b.rs", "b\n", "feature work").await;
    must(git(&fx.work, &["push", "-q", &fx.srv.http_url("olivia", REPO), "feature"]).await);
    wait_jobs(&fx.srv.state).await;
    let list = fx.owner.ok_get(&format!("/api/v1/projects/{}/pipelines?ref=feature", fx.project)).await;
    let iid = list.as_array().unwrap()[0]["iid"].as_i64().unwrap();
    let p = pipeline(&fx, iid).await;
    let mut names: Vec<&str> = p["jobs"].as_array().unwrap().iter().map(|j| j["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, vec!["shared", "src"], "{p}");
}

async fn upload_archive(fx: &Fx, job: &Value) {
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(b"PK zip".to_vec()).file_name("artifacts.zip"));
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=zip&artifact_type=archive", fx.srv.base, job["id"]))
        .header("JOB-TOKEN", job["token"].as_str().unwrap())
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
}

async fn download(fx: &Fx, dep_id: &Value, token: &Value) -> StatusCode {
    reqwest::Client::new()
        .get(format!("{}/api/v4/jobs/{dep_id}/artifacts", fx.srv.base))
        .header("JOB-TOKEN", token.as_str().unwrap())
        .send()
        .await
        .unwrap()
        .status()
}

async fn request_with(fx: &Fx, runner: &str) -> Option<Value> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", fx.srv.base))
        .json(&json!({ "token": runner, "system_id": "s_k2", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    if r.status() == StatusCode::CREATED { Some(r.json().await.unwrap()) } else { None }
}

/// `needs:pipeline` (a child takes its parent's artifacts) and `needs:project`
/// (another project's latest on a ref, for who may read it).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn needs_from_other_pipelines_and_projects(pool: sqlx::PgPool) {
    let ci = "stages: [build, child]\ngen:\n  stage: build\n  script: [make]\n  artifacts:\n    paths: [out/]\nchild:\n  stage: child\n  trigger:\n    include: ci/child.yml\n  variables:\n    PARENT_PIPELINE_ID: $CI_PIPELINE_ID\n";
    let child = "use:\n  needs:\n    - pipeline: $PARENT_PIPELINE_ID\n      job: gen\n  script: [cat out/x]\n";
    let fx = fixture(pool, &[(".gitlab-ci.yml", ci), ("ci/child.yml", child)]).await;
    let gen_job = request(&fx).await.expect("gen");
    upload_archive(&fx, &gen_job).await;
    finish(&fx, &gen_job, "success").await;
    wait_jobs(&fx.srv.state).await;
    let used = request(&fx).await.expect("the child's job");
    assert_eq!(used["job_info"]["name"], "use");
    let deps = used["dependencies"].as_array().unwrap();
    assert_eq!((deps.len(), deps[0]["id"].clone(), deps[0]["name"].as_str()), (1, gen_job["id"].clone(), Some("gen")), "{deps:?}");
    assert!(deps[0]["artifacts_file"]["filename"].is_string(), "{deps:?}");
    assert_eq!(download(&fx, &gen_job["id"], &used["token"]).await, StatusCode::OK, "from the parent pipeline");
    finish(&fx, &used, "success").await;

    // another project of the owner's: the latest `gen` on main
    let sub = fx.owner.ok_get("/api/v1/groups/acme%2Fplatform").await["id"].as_i64().unwrap();
    let web =
        fx.owner.ok_post("/api/v1/projects", json!({ "namespace_id": sub, "name": "Web", "path": "web" })).await["id"].as_i64().unwrap();
    let web_runner = fx.owner.ok_post(&format!("/api/v1/projects/{web}/runners"), json!({ "description": "web" })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let wci = "site:\n  needs:\n    - project: acme/platform/api\n      job: gen\n      ref: main\n    - project: acme/platform/api\n      job: nope\n      ref: main\n  script: [x]\n";
    // api lets web's jobs in (its job token allowlist: on by default)
    let api_id = fx.owner.ok_get("/api/v1/projects/acme%2Fplatform%2Fapi").await["id"].as_i64().unwrap();
    fx.owner.ok_post(&format!("/api/v1/projects/{api_id}/job_token_allowlist"), json!({ "project": "acme/platform/web" })).await;
    let wwork = fx.srv.work_dir("web");
    must(git(&wwork, &["init", "-q", "-b", "main"]).await);
    std::fs::write(wwork.join(".gitlab-ci.yml"), wci).unwrap();
    must(git(&wwork, &["add", "."]).await);
    must(git(&wwork, &["commit", "-q", "-m", "ci"]).await);
    must(git(&wwork, &["push", "-q", &fx.srv.http_url("olivia", "acme/platform/web"), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    let site = request_with(&fx, &web_runner).await.expect("web's job");
    let deps = site["dependencies"].as_array().unwrap();
    assert_eq!(
        deps.iter().map(|d| d["id"].clone()).collect::<Vec<_>>(),
        vec![gen_job["id"].clone()],
        "a missing job is left out: {deps:?}"
    );
    assert_eq!(download(&fx, &gen_job["id"], &site["token"]).await, StatusCode::OK);

    // someone who cannot read it gets nothing from it
    let eve = fx.srv.user("eve", false).await;
    let eve_p = eve.ok_post("/api/v1/projects", json!({ "name": "Tool", "path": "tool" })).await["id"].as_i64().unwrap();
    let eve_runner = eve.ok_post(&format!("/api/v1/projects/{eve_p}/runners"), json!({ "description": "eve" })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let ework = fx.srv.work_dir("eve");
    must(git(&ework, &["init", "-q", "-b", "main"]).await);
    std::fs::write(ework.join(".gitlab-ci.yml"), "steal:\n  needs: [{project: acme/platform/api, job: gen, ref: main}]\n  script: [x]\n")
        .unwrap();
    must(git(&ework, &["add", "."]).await);
    must(git(&ework, &["commit", "-q", "-m", "ci"]).await);
    must(git(&ework, &["push", "-q", &fx.srv.http_url("eve", "eve/tool"), "main"]).await);
    wait_jobs(&fx.srv.state).await;
    let steal = request_with(&fx, &eve_runner).await.expect("eve's job");
    assert!(steal["dependencies"].as_array().unwrap().is_empty(), "{steal}");
    assert_eq!(download(&fx, &gen_job["id"], &steal["token"]).await, StatusCode::FORBIDDEN);
}

/// A zip of `files`, as `git archive` writes it (deflated entries).
async fn zip_of(dir: &std::path::Path, files: &[(&str, &str)]) -> Vec<u8> {
    must(git(dir, &["init", "-q", "-b", "main"]).await);
    for (path, content) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    must(git(dir, &["add", "."]).await);
    must(git(dir, &["commit", "-q", "-m", "site"]).await);
    let out =
        tokio::process::Command::new("git").args(["-C", dir.to_str().unwrap(), "archive", "--format=zip", "HEAD"]).output().await.unwrap();
    assert!(out.status.success());
    out.stdout
}

/// Pages: a passing `pages` job's `public/` becomes the project's
/// site on `<namespace>.<pages domain>`, for public projects, with its own
/// headers and nothing outside the published folder.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn pages_sites(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| {
        c.ci.long_poll_secs = 1;
        c.pages.domain = "pages.test".into();
        c.pages.http = true;
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (org, sub, project) = org_project(&owner).await;
    for g in [org, sub] {
        owner.ok_put(&format!("/api/v1/groups/{g}"), json!({ "name": format!("g{g}"), "description": "", "visibility": "public" })).await;
    }
    owner.ok_put(&format!("/api/v1/projects/{project}"), json!({ "name": "api", "description": "", "visibility": "public" })).await;
    let runner = owner.ok_post(&format!("/api/v1/projects/{project}/runners"), json!({ "description": "fake" })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let work = srv.work_dir("pages");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join(".gitlab-ci.yml"), "pages:\n  script: [make site]\n  artifacts:\n    paths: [public]\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "ci"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    let fx = Fx { srv, owner, project, work, runner };

    let job = request(&fx).await.expect("the pages job");
    assert_eq!(var(&job, "CI_PAGES_URL").as_deref(), Some("http://acme.pages.test/platform/api"));
    let zip = zip_of(
        &fx.srv.work_dir("site"),
        &[
            ("public/index.html", "<h1>home</h1>"),
            ("public/about/index.html", "about"),
            ("public/style.css", "h1{}"),
            ("public/404.html", "custom 404"),
            ("secret.txt", "not published"),
        ],
    )
    .await;
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(zip).file_name("artifacts.zip"));
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=zip&artifact_type=archive&expire_in=1+day", fx.srv.base, job["id"]))
        .header("JOB-TOKEN", job["token"].as_str().unwrap())
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    finish(&fx, &job, "success").await;
    let info = fx.owner.ok_get(&format!("/api/v1/projects/{project}/pages")).await;
    assert_eq!(
        (info["url"].as_str(), info["deployment"]["publish"].as_str()),
        (Some("http://acme.pages.test/platform/api/"), Some("public"))
    );
    let kept: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT expire_at FROM ci_job_artifacts WHERE build_id = $1 AND file_type = 'archive'")
            .bind(job["id"].as_i64().unwrap())
            .fetch_one(&fx.srv.state.db)
            .await
            .unwrap();
    assert!(kept.is_none(), "the site's archive does not expire");

    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let get = |host: &'static str, path: &'static str| {
        let req = http.get(format!("{}{path}", fx.srv.base)).header("host", host);
        async move { req.send().await.unwrap() }
    };
    let r = get("acme.pages.test", "/platform/api/").await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/html");
    assert!(r.headers().get("set-cookie").is_none() && r.headers().get("content-security-policy").is_none(), "{:?}", r.headers());
    assert_eq!(r.text().await.unwrap(), "<h1>home</h1>");
    for (path, to) in [("/platform/api", "/platform/api/"), ("/platform/api/about", "/platform/api/about/")] {
        let r = get("acme.pages.test", path).await;
        assert_eq!((r.status().as_u16(), r.headers()["location"].to_str().unwrap()), (301, to), "{path}");
    }
    assert_eq!(get("acme.pages.test", "/platform/api/about/").await.text().await.unwrap(), "about");
    assert_eq!(get("ACME.pages.test:8080", "/platform/api/style.css").await.headers()["content-type"], "text/css");
    let r = get("acme.pages.test", "/platform/api/nope").await;
    assert_eq!((r.status().as_u16(), r.text().await.unwrap()), (404, "custom 404".to_string()));
    // nothing outside the published folder
    for path in ["/platform/api/%2e%2e/secret.txt", "/platform/api/../secret.txt", "/platform/secret.txt"] {
        let r = get("acme.pages.test", path).await;
        assert!(r.status() == 404 || r.status() == 400, "{path}: {}", r.status());
    }
    assert_eq!(http.post(format!("{}/platform/api/", fx.srv.base)).header("host", "acme.pages.test").send().await.unwrap().status(), 405);
    // other hosts are Forgust's own pages
    let app = get("x.acme.pages.test", "/api/v1/version").await;
    assert!(app.status() == 200 && app.text().await.unwrap().contains("version"), "not a site: Forgust's API answers");

    // a private project's site is not served; removing takes it down
    fx.owner.ok_put(&format!("/api/v1/projects/{project}"), json!({ "name": "api", "description": "", "visibility": "private" })).await;
    assert_eq!(get("acme.pages.test", "/platform/api/").await.status(), 404);
    fx.owner.ok_put(&format!("/api/v1/projects/{project}"), json!({ "name": "api", "description": "", "visibility": "public" })).await;
    assert_eq!(get("acme.pages.test", "/platform/api/").await.status(), 200);
    assert_eq!(fx.owner.delete(&format!("/api/v1/projects/{project}/pages")).await.0, 200);
    assert_eq!(get("acme.pages.test", "/platform/api/").await.status(), 404);
}
