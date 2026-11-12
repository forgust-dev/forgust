//! ADR-0024: security findings from the reports jobs upload (GitLab's
//! JSON and SARIF): the project's findings and their states, dismissals, what
//! a merge request finds against the default branch, and the merge check.

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/security/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

async fn request(srv: &TestServer, runner: &str) -> Value {
    for _ in 0..10 {
        let r = reqwest::Client::new()
            .post(format!("{}/api/v4/jobs/request", srv.base))
            .json(&json!({ "token": runner, "system_id": "s_sec", "info": { "version": "19.5.3", "executor": "docker" } }))
            .send()
            .await
            .unwrap();
        if r.status() == StatusCode::CREATED {
            return r.json().await.unwrap();
        }
    }
    panic!("no job");
}

/// The job uploads these reports, then passes.
async fn run_job(srv: &TestServer, runner: &str, reports: &[(&str, Vec<u8>)]) -> Value {
    let job = request(srv, runner).await;
    for (kind, data) in reports {
        let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(data.clone()).file_name("gl-report.json"));
        let r = reqwest::Client::new()
            .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=raw&artifact_type={kind}", srv.base, job["id"]))
            .header("JOB-TOKEN", job["token"].as_str().unwrap())
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED, "{kind}");
    }
    let r = reqwest::Client::new()
        .put(format!("{}/api/v4/jobs/{}", srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": "success", "exit_code": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    wait_jobs(&srv.state).await;
    job
}

/// The SAST fixture without its SQL injection.
fn sast_without_sqli() -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(&fixture("semgrep-sast.json")).unwrap();
    v["vulnerabilities"].as_array_mut().unwrap().remove(0);
    serde_json::to_vec(&v).unwrap()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn findings_states_and_merge_requests(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let rep = srv.user("rita", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let runner =
        owner.ok_post(&format!("/api/v1/projects/{p}/runners"), json!({ "description": "r" })).await["token"].as_str().unwrap().to_string();
    let work = srv.work_dir("sec");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::create_dir_all(work.join("app")).unwrap();
    let db_py: String = (1..=11).map(|i| format!("# line {i}\n")).collect::<String>() + "q = f\"SELECT * FROM users WHERE id = {uid}\"\n";
    std::fs::write(work.join("app/db.py"), &db_py).unwrap();
    std::fs::write(work.join("app/hash.py"), "import hashlib\n\nh = hashlib.md5(b)\n").unwrap();
    std::fs::write(work.join(".gitlab-ci.yml"), "scan:\n  script: [scan]\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "init"]).await);
    let url = srv.http_url("olivia", REPO);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    let base = format!("/api/v1/projects/{p}/security/findings");

    // the default branch's scan: SAST and dependencies
    run_job(&srv, &runner, &[("sast", fixture("semgrep-sast.json")), ("dependency_scanning", fixture("gemnasium-dependency.json"))]).await;
    let s = dev.ok_get(&base).await;
    assert_eq!(
        (s["counts"]["critical"].as_i64(), s["counts"]["high"].as_i64(), s["counts"]["medium"].as_i64()),
        (Some(1), Some(1), Some(1)),
        "{s}"
    );
    let titles: Vec<&str> = s["findings"].as_array().unwrap().iter().map(|f| f["title"].as_str().unwrap()).collect();
    assert_eq!(titles[0], "Prototype Pollution in lodash", "most severe first: {titles:?}");
    assert_eq!(s["last_scan_pipeline_iid"], 1);
    let lodash = &s["findings"][0];
    assert_eq!((lodash["fixed_version"].as_str(), lodash["package_name"].as_str()), (Some("4.17.21"), Some("lodash")));
    // Maintainers hear about the dependency with a fix (slice 3)
    let alerts: Vec<_> = srv.state.mailer.sent().into_iter().filter(|m| m.subject.contains("vulnerable dependency to update")).collect();
    assert_eq!(alerts.len(), 1, "one email per scan");
    assert_eq!(alerts[0].to, vec!["olivia@example.com".to_string()], "the Owner; not the Developer");
    assert!(alerts[0].body.contains("lodash 4.17.15 → 4.17.21 (CVE-2020-8203)"), "{}", alerts[0].body);
    // Renovate's merge request updating it shows on the finding
    must(git(&work, &["checkout", "-q", "-b", "renovate/lodash-4.x"]).await);
    commit_file(&work, "renovate.txt", "x\n", "Update dependency lodash to v4.17.21").await;
    must(git(&work, &["push", "-q", &url, "renovate/lodash-4.x"]).await);
    wait_jobs(&srv.state).await;
    let _ = run_job(&srv, &runner, &[]).await;
    owner
        .ok_post(
            &format!("/api/v1/projects/{p}/merge_requests"),
            json!({ "source_branch": "renovate/lodash-4.x", "title": "Update dependency lodash to v4.17.21" }),
        )
        .await;
    must(git(&work, &["checkout", "-q", "main"]).await);
    let lodash_id = lodash["id"].as_i64().unwrap();
    let detail = dev.ok_get(&format!("{base}/{lodash_id}")).await;
    assert_eq!(detail["fix_merge_requests"][0]["iid"], 1, "{detail}");
    // Developers and above only
    assert_eq!(rep.get(&base).await.0, 403);

    // dismissing, with a reason; reopening
    let md5 = s["findings"].as_array().unwrap().iter().find(|f| f["severity"] == "medium").unwrap()["id"].as_i64().unwrap();
    assert_eq!(dev.post(&format!("{base}/{md5}/dismiss"), json!({ "reason": "because" })).await.0, 400);
    let d = dev.ok_post(&format!("{base}/{md5}/dismiss"), json!({ "reason": "used_in_tests", "comment": "only a cache key" })).await;
    assert_eq!((d["state"].as_str(), d["dismissed_by"].as_str()), (Some("dismissed"), Some("dave")));
    assert_eq!(
        d["events"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["detected", "dismissed"]
    );
    let r = dev.ok_post(&format!("{base}/{md5}/reopen"), json!({})).await;
    assert_eq!(r["state"], "detected");
    dev.ok_post(&format!("{base}/{md5}/dismiss"), json!({ "reason": "used_in_tests" })).await;
    assert_eq!(dev.ok_get(&base).await["counts"]["dismissed"], 1);

    // a merge request: its pipeline fixes the SQL injection and adds CodeQL's
    must(git(&work, &["checkout", "-q", "-b", "fix"]).await);
    commit_file(&work, "fix.txt", "x\n", "fix").await;
    must(git(&work, &["push", "-q", &url, "fix"]).await);
    wait_jobs(&srv.state).await;
    // two files under `sast`: the runner sends them one after the other
    let both = [sast_without_sqli(), b"\n".to_vec(), fixture("codeql.sarif")].concat();
    run_job(&srv, &runner, &[("sast", both)]).await;
    dev.ok_post(&format!("/api/v1/projects/{p}/merge_requests"), json!({ "source_branch": "fix", "title": "Fix" })).await;
    let mrs = dev.ok_get(&format!("/api/v1/projects/{p}/merge_requests/2/security")).await;
    let names = |k: &str| mrs[k].as_array().unwrap().iter().map(|f| f["title"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(mrs["report_types"], json!(["sast"]), "dependencies were not scanned by the MR: not compared: {mrs}");
    assert_eq!(names("new"), vec!["Database query built from user-controlled sources".to_string(), "Unused variable".to_string()], "{mrs}");
    assert_eq!(names("fixed"), vec!["Improper Neutralization of Special Elements used in an SQL Command ('SQL Injection')".to_string()]);
    // MR-only findings are not the project's yet
    assert_eq!(dev.ok_get(&base).await["counts"]["high"], 1);

    // the merge check: a new high finding blocks until dismissed
    let settings = |block: bool| {
        json!({ "ci_config_path": ".gitlab-ci.yml", "instance_runners_enabled": true, "default_timeout_secs": 3600, "default_git_depth": 20,
                "only_merge_if_pipeline_succeeds": false, "job_token_scope_enabled": true, "auto_cancel_redundant": true,
                "merge_trains_enabled": false, "block_new_security_findings": block })
    };
    owner.ok_put(&format!("/api/v1/projects/{p}/ci/settings"), settings(true)).await;
    let st = owner.ok_get(&format!("/api/v1/projects/{p}/merge_requests/2/merge_status")).await;
    assert!(st["blockers"].to_string().contains("1 new critical or high security finding"), "{st}");
    let codeql = mrs["new"][0]["id"].as_i64().unwrap();
    dev.ok_post(&format!("{base}/{codeql}/dismiss"), json!({ "reason": "false_positive" })).await;
    let st = owner.ok_get(&format!("/api/v1/projects/{p}/merge_requests/2/merge_status")).await;
    assert!(!st["blockers"].to_string().contains("security"), "{st}");

    // the default branch's next complete scan: the SQL injection is resolved
    must(git(&work, &["checkout", "-q", "main"]).await);
    commit_file(&work, "next.txt", "x\n", "next").await;
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    run_job(&srv, &runner, &[("sast", sast_without_sqli()), ("dependency_scanning", fixture("gemnasium-dependency.json"))]).await;
    let resolved = dev.ok_get(&format!("{base}?state=resolved")).await;
    assert_eq!(resolved["findings"].as_array().unwrap().len(), 1, "{resolved}");
    let sqli = resolved["findings"][0]["id"].as_i64().unwrap();
    assert_eq!(dev.ok_get(&base).await["counts"]["high"], 0);
    // ...and back when it reappears (a failed scan resolves nothing)
    commit_file(&work, "again.txt", "x\n", "again").await;
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    run_job(&srv, &runner, &[("sast", fixture("semgrep-sast.json")), ("dependency_scanning", fixture("gemnasium-dependency.json"))]).await;
    let f = dev.ok_get(&format!("{base}/{sqli}")).await;
    assert_eq!(f["state"], "detected");
    let actions: Vec<&str> = f["events"].as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert_eq!(actions, vec!["detected", "resolved", "redetected"]);
    // the dismissed one stayed dismissed throughout
    assert_eq!(dev.ok_get(&format!("{base}/{md5}")).await["state"], "dismissed");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_broken_report_is_recorded_and_resolves_nothing(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    let runner =
        owner.ok_post(&format!("/api/v1/projects/{p}/runners"), json!({ "description": "r" })).await["token"].as_str().unwrap().to_string();
    let work = srv.work_dir("sec2");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join(".gitlab-ci.yml"), "scan:\n  script: [scan]\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "init"]).await);
    let url = srv.http_url("olivia", REPO);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    run_job(&srv, &runner, &[("secret_detection", fixture("secrets.json"))]).await;
    let base = format!("/api/v1/projects/{p}/security/findings");
    assert_eq!(owner.ok_get(&base).await["counts"]["critical"], 1);
    // who pushed it hears about the secret (to rotate it)
    let mail = srv.state.mailer.sent().into_iter().find(|m| m.subject.contains("A secret was found")).expect("the secret email");
    assert_eq!(mail.to, vec!["olivia@example.com".to_string()]);
    assert!(mail.body.contains("AWS access token in config/settings.py:7") && !mail.body.contains("AKIA"), "{}", mail.body);
    // the instance's view: projects with open critical or high findings
    let admin = srv.user("root", true).await;
    let a = admin.ok_get("/api/v1/admin/security/scanning").await;
    assert_eq!((a["exposed"][0]["full_path"].as_str(), a["exposed"][0]["critical"].as_i64()), (Some(REPO), Some(1)), "{a}");
    commit_file(&work, "x.txt", "x\n", "x").await;
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    run_job(&srv, &runner, &[("secret_detection", b"{ not json".to_vec())]).await;
    let errors: Vec<Option<String>> =
        sqlx::query_scalar("SELECT error FROM security_scans ORDER BY created_at").fetch_all(&srv.state.db).await.unwrap();
    assert!(errors.last().unwrap().as_deref().unwrap().contains("not JSON"), "{errors:?}");
    assert_eq!(owner.ok_get(&base).await["counts"]["critical"], 1, "an unreadable report resolves nothing");
}

/// Built-in scanners (slice 2): Forgust writes the pipeline, instance runners
/// only, no project CI variables; their reports become findings; merge
/// requests are scanned once per head; groups turn it on for their projects.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn built_in_scanners(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let rep = srv.user("rita", false).await;
    let (_org, sub, p) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let work = srv.work_dir("scan");
    seed_repo(&work).await;
    let url = srv.http_url("olivia", REPO);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/variables"), json!({ "key": "DEPLOY_SECRET", "value": "never-in-a-scan" })).await;
    let shared = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared", "run_untagged": true })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let own = owner.ok_post(&format!("/api/v1/projects/{p}/runners"), json!({ "description": "own" })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let scanning = format!("/api/v1/projects/{p}/security/scanning");

    // Maintainers turn it on (a first scan is due now)
    let on = json!({ "enabled": true, "scanners": ["sast", "secret_detection", "dependency_scanning", "iac"] });
    assert_eq!(rep.put(&scanning, on.clone()).await.0, 403);
    assert_eq!(owner.put(&scanning, json!({ "enabled": true, "scanners": ["virus"] })).await.0, 400);
    let st = owner.ok_put(&scanning, on.clone()).await;
    assert_eq!((st["enabled"].as_bool(), st["run_as"].as_str()), (Some(true), Some("olivia")), "{st}");
    forgust_server::services::scanning::run_due(&srv.state).await.unwrap();
    let st = owner.ok_get(&scanning).await;
    assert!(st["last_pipeline_iid"].is_i64(), "{st}");

    // the project's own runner never takes them; the instance's does
    assert_eq!(try_request(&srv, &own).await, None);
    let mut jobs = Vec::new();
    while let Some(j) = try_request(&srv, &shared).await {
        jobs.push(j);
    }
    let names: Vec<&str> = jobs.iter().map(|j| j["job_info"]["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 4, "{names:?}");
    for j in &jobs {
        let vars = j["variables"].to_string();
        assert!(!vars.contains("never-in-a-scan"), "no project CI variables");
        assert_eq!(var(j, "CI_PIPELINE_SOURCE").as_deref(), Some("security"));
        // as the project's scanner bot (a Reporter), not whoever turned scanning on
        assert_eq!(var(j, "GITLAB_USER_LOGIN"), Some(format!("security-bot-{p}")));
        if let Some(c) = j["cache"].as_array().and_then(|c| c.first()) {
            assert_eq!(c["key"], "trivy-forgust_security", "a cache no project job can name");
        }
    }
    let job = |n: &str| jobs.iter().find(|j| j["job_info"]["name"] == n).unwrap().clone();
    assert_eq!(job("code")["image"]["name"], "registry.gitlab.com/security-products/semgrep:5");
    assert_eq!(var(&job("secrets"), "SECRET_DETECTION_HISTORIC_SCAN").as_deref(), Some("true"), "the first scan reads the history");
    assert_eq!(job("dependencies")["image"]["name"], "aquasec/trivy:0.57.1");
    // their reports become findings, as any job's
    upload_and_finish(&srv, &job("code"), &[("sast", fixture("semgrep-sast.json"))]).await;
    upload_and_finish(&srv, &job("dependencies"), &[("dependency_scanning", fixture("trivy-fs.sarif"))]).await;
    for n in ["secrets", "infrastructure"] {
        upload_and_finish(&srv, &job(n), &[]).await;
    }
    let found = owner.ok_get(&format!("/api/v1/projects/{p}/security/findings")).await;
    assert_eq!((found["counts"]["high"].as_i64(), found["counts"]["medium"].as_i64()), (Some(2), Some(1)), "{found}");
    // security pipelines are not the commit's status
    let head = srv.state.storage.resolve(p, "refs/heads/main").await.unwrap().unwrap();
    let statuses = owner.ok_get(&format!("/api/v1/projects/{p}/commit_statuses?shas={head}")).await;
    assert!(statuses.as_array().unwrap().is_empty(), "{statuses}");

    // the next scan: no history; the admin's image
    let mut settings = admin.ok_get("/api/v1/admin/security/scanning").await["settings"].clone();
    settings["sast_image"] = json!("registry.example.com/semgrep:9");
    admin.ok_put("/api/v1/admin/security/scanning", settings).await;
    owner.ok_post(&format!("{scanning}/run"), json!({})).await;
    let mut jobs = Vec::new();
    while let Some(j) = try_request(&srv, &shared).await {
        jobs.push(j);
    }
    let code = jobs.iter().find(|j| j["job_info"]["name"] == "code").unwrap();
    assert_eq!(code["image"]["name"], "registry.example.com/semgrep:9");
    let secrets = jobs.iter().find(|j| j["job_info"]["name"] == "secrets").unwrap();
    assert_eq!(var(secrets, "SECRET_DETECTION_HISTORIC_SCAN"), None);
    assert_eq!(owner.post(&format!("{scanning}/run"), json!({})).await.0, 409, "one at a time");
    for j in &jobs {
        upload_and_finish(&srv, j, &[]).await;
    }

    // a merge request is scanned once per head
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "t.txt", "t\n", "topic").await;
    must(git(&work, &["push", "-q", &url, "topic"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/merge_requests"), json!({ "source_branch": "topic", "title": "Topic" })).await;
    wait_jobs(&srv.state).await;
    let mr_scans: i64 = sqlx::query_scalar("SELECT count(*) FROM ci_pipelines WHERE source = 'security' AND merge_request_id IS NOT NULL")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(mr_scans, 1);
    commit_file(&work, "t.txt", "t2\n", "topic 2").await;
    must(git(&work, &["push", "-q", &url, "topic"]).await);
    wait_jobs(&srv.state).await;
    let mr_scans: i64 = sqlx::query_scalar("SELECT count(*) FROM ci_pipelines WHERE source = 'security' AND merge_request_id IS NOT NULL")
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(mr_scans, 2, "the new head too");

    // off: running scans stop, findings stay
    owner.ok_put(&scanning, json!({ "enabled": false, "scanners": [] })).await;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_pipelines WHERE source = 'security' AND status NOT IN ('success', 'failed', 'canceled', 'skipped')",
    )
    .fetch_one(&srv.state.db)
    .await
    .unwrap();
    assert_eq!(active, 0);
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{p}/security/findings")).await["counts"]["high"], 2);

    // a group turns it on for projects that never chose, and new ones
    let other =
        owner.ok_post("/api/v1/projects", json!({ "name": "Web", "path": "web", "namespace_id": sub })).await["id"].as_i64().unwrap();
    let r = owner.ok_put(&format!("/api/v1/groups/{sub}/security/scanning"), json!({ "enabled": true })).await;
    assert_eq!(r["projects_turned_on"], 1, "web; api chose already: {r}");
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{other}/security/scanning")).await["enabled"], true);
    assert_eq!(owner.ok_get(&scanning).await["enabled"], false, "api's choice stands");
    let newer =
        owner.ok_post("/api/v1/projects", json!({ "name": "Docs", "path": "docs", "namespace_id": sub })).await["id"].as_i64().unwrap();
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{newer}/security/scanning")).await["enabled"], true);
}

fn var(job: &Value, k: &str) -> Option<String> {
    job["variables"].as_array().unwrap().iter().rev().find(|v| v["key"] == k).map(|v| v["value"].as_str().unwrap().to_string())
}

async fn try_request(srv: &TestServer, runner: &str) -> Option<Value> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", srv.base))
        .json(&json!({ "token": runner, "system_id": "s_scan", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    if r.status() == StatusCode::CREATED { Some(r.json().await.unwrap()) } else { None }
}

async fn upload_and_finish(srv: &TestServer, job: &Value, reports: &[(&str, Vec<u8>)]) {
    for (kind, data) in reports {
        let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(data.clone()).file_name("r.json"));
        let r = reqwest::Client::new()
            .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=raw&artifact_type={kind}", srv.base, job["id"]))
            .header("JOB-TOKEN", job["token"].as_str().unwrap())
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
    }
    let r = reqwest::Client::new()
        .put(format!("{}/api/v4/jobs/{}", srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": "success", "exit_code": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    wait_jobs(&srv.state).await;
}

/// The real scanners (opt in with `FORGUST_TEST_SCANNERS=1`: needs Docker and the
/// network for their images and Trivy's database): the jobs Forgust writes, run
/// with their images on a project holding one SQL injection, one hard-coded
/// token and one vulnerable dependency, and what they find read back.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn real_scanners_find_a_sql_injection_a_secret_and_a_vulnerable_dependency(pool: sqlx::PgPool) {
    if std::env::var("FORGUST_TEST_SCANNERS").is_err() {
        eprintln!("skipped: set FORGUST_TEST_SCANNERS=1 to run the real scanners (Docker)");
        return;
    }
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    let work = srv.work_dir("real-scan");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(
        work.join("app.py"),
        "import sqlite3\n\n\ndef find(name):\n    db = sqlite3.connect('app.db')\n    cur = db.cursor()\n    cur.execute(\"SELECT * FROM users WHERE name = '%s'\" % name)\n    return cur.fetchall()\n",
    )
    .unwrap();
    // a fake token for the secrets scanner to find, in two pieces here so that
    // scanners reading this repository do not report it
    let fake_token = ["glpat-", "x7Qk2mVb9LpR4tYw8NzA"].concat();
    std::fs::write(work.join("settings.py"), format!("GITLAB_TOKEN = \"{fake_token}\"\n")).unwrap();
    std::fs::write(work.join("package.json"), r#"{ "name": "app", "version": "1.0.0", "dependencies": { "lodash": "4.17.15" } }"#).unwrap();
    std::fs::write(
        work.join("package-lock.json"),
        r#"{ "name": "app", "version": "1.0.0", "lockfileVersion": 3, "requires": true,
  "packages": { "": { "name": "app", "version": "1.0.0", "dependencies": { "lodash": "4.17.15" } },
                "node_modules/lodash": { "version": "4.17.15", "resolved": "https://registry.npmjs.org/lodash/-/lodash-4.17.15.tgz" } } }"#,
    )
    .unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "app"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    let sha = srv.state.storage.resolve(p, "refs/heads/main").await.unwrap().unwrap();
    let shared = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared", "run_untagged": true })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    owner
        .ok_put(
            &format!("/api/v1/projects/{p}/security/scanning"),
            json!({ "enabled": true, "scanners": ["sast", "secret_detection", "dependency_scanning"] }),
        )
        .await;
    owner.ok_post(&format!("/api/v1/projects/{p}/security/scanning/run"), json!({})).await;

    while let Some(job) = try_request(&srv, &shared).await {
        let image = job["image"]["name"].as_str().unwrap().to_string();
        let script: Vec<String> = job["steps"][0]["script"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect();
        let report: (String, String) = job["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|a| Some((a["artifact_type"].as_str()?.to_string(), a["paths"][0].as_str()?.to_string())))
            .unwrap();
        let name = format!("forgust-scan-{}", job["id"]);
        let _guard = Container(name.clone());
        let mut create = std::process::Command::new("docker");
        create.args(["create", "--name", &name, "--entrypoint", "sh", "-w", "/tmp/src"]);
        // what a runner gives a job: the variables, and where the sources are
        for v in job["variables"].as_array().unwrap() {
            let (k, val) = (v["key"].as_str().unwrap(), v["value"].as_str().unwrap());
            if k.starts_with("CI_") || k.starts_with("SECRET_") || k.starts_with("TRIVY_") || k == "GIT_DEPTH" {
                create.args(["-e", &format!("{k}={}", val.replace("$CI_PROJECT_DIR", "/tmp/src"))]);
            }
        }
        create.args([
            "-e",
            "CI_PROJECT_DIR=/tmp/src",
            "-e",
            "CI_BUILDS_DIR=/tmp",
            // `docker cp` leaves the sources owned by whoever runs the test, not
            // the image's user, and git then refuses the repository ("dubious
            // ownership"); a runner clones inside the container instead
            "-e",
            "GIT_CONFIG_COUNT=1",
            "-e",
            "GIT_CONFIG_KEY_0=safe.directory",
            "-e",
            "GIT_CONFIG_VALUE_0=*",
            &format!("-e=CI_COMMIT_SHA={sha}"),
            &image,
            "-c",
            &script.join("\n"),
        ]);
        let out = create.output().unwrap();
        assert!(out.status.success(), "docker create: {}", String::from_utf8_lossy(&out.stderr));
        let cp = std::process::Command::new("docker")
            .args(["cp", &format!("{}/.", work.display()), &format!("{name}:/tmp/src")])
            .output()
            .unwrap();
        assert!(cp.status.success(), "docker cp in: {}", String::from_utf8_lossy(&cp.stderr));
        let run = std::process::Command::new("docker").args(["start", "-a", &name]).output().unwrap();
        eprintln!(
            "--- {} ({image})\n{}\n{}",
            job["job_info"]["name"],
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(run.status.success(), "the {} scanner failed", job["job_info"]["name"]);
        let local = srv.work_dir(&format!("report-{}", job["id"]));
        let got = std::process::Command::new("docker")
            .args(["cp", &format!("{name}:/tmp/src/{}", report.1), &local.display().to_string()])
            .output()
            .unwrap();
        assert!(got.status.success(), "no report {}: {}", report.1, String::from_utf8_lossy(&got.stderr));
        let data = std::fs::read(local.join(std::path::Path::new(&report.1).file_name().unwrap())).unwrap();
        upload_and_finish(&srv, &job, &[(report.0.as_str(), data)]).await;
    }
    wait_jobs(&srv.state).await;
    let found = owner.ok_get(&format!("/api/v1/projects/{p}/security/findings?state=all&per_page=100")).await;
    let list = found["findings"].as_array().unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&found["counts"]).unwrap());
    let has = |kind: &str, f: &dyn Fn(&Value) -> bool| list.iter().any(|x| x["report_type"] == kind && f(x));
    assert!(
        has("sast", &|x| x["file"] == "app.py"
            && x["identifiers"].as_array().unwrap().iter().any(|i| i["type"] == "cwe" && i["value"] == "89")),
        "the SQL injection: {found}"
    );
    assert!(has("secret_detection", &|x| x["file"] == "settings.py"), "the token: {found}");
    assert!(has("dependency_scanning", &|x| x["package_name"] == "lodash" && x["fixed_version"].is_string()), "lodash: {found}");
    assert!(!found.to_string().contains(&fake_token), "the secret itself is not stored");
}

struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}
