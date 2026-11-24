//! The Smart View: off until a Maintainer turns it on for a project; then Forgust runs a job
//! of its own (the assay image, on the instance's runners, as a Reporter bot) on each new
//! commit of the default branch and of open merge requests, reads the report it uploads, and
//! keeps the latest map of each: older ones, and those nothing shows any more, go.

mod common;

use std::path::Path;

use common::*;
use forgust_server::events::Event;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::broadcast::{Receiver, error::TryRecvError};

async fn request(srv: &TestServer, token: &str) -> Option<Value> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", srv.base))
        .json(&json!({ "token": token, "system_id": "s_map", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    if r.status() == StatusCode::CREATED { Some(r.json().await.unwrap()) } else { None }
}

fn var(job: &Value, k: &str) -> Option<String> {
    job["variables"].as_array().unwrap().iter().rev().find(|v| v["key"] == k).map(|v| v["value"].as_str().unwrap().to_string())
}

/// Upload `report` (when some) as the job's Smart View report, and end the job.
async fn report_and_finish(srv: &TestServer, job: &Value, report: Option<Value>, state: &str) {
    if let Some(r) = report {
        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::bytes(serde_json::to_vec(&r).unwrap()).file_name("assay-report.json"));
        let res = reqwest::Client::new()
            .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=raw&artifact_type=smart_view", srv.base, job["id"]))
            .header("JOB-TOKEN", job["token"].as_str().unwrap())
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
    }
    let res = reqwest::Client::new()
        .put(format!("{}/api/v4/jobs/{}", srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": state, "exit_code": if state == "success" { 0 } else { 1 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    wait_jobs(&srv.state).await;
}

/// What the assay image writes for `work`, a clone at the job's commit.
async fn assay_report(job: &Value, work: &Path) -> Value {
    assert_eq!(
        must(git(work, &["rev-parse", "HEAD"]).await).trim(),
        var(job, "CI_COMMIT_SHA").unwrap(),
        "the clone is at the job's commit"
    );
    let opts = forgust_assay::Options { name: var(job, "FORGUST_SMART_VIEW_NAME"), ..Default::default() };
    serde_json::to_value(forgust_assay::report::report(work, &opts).unwrap()).unwrap()
}

/// The runner takes the map job waiting, runs assay on `work`, and uploads its report.
async fn run_map_job(srv: &TestServer, runner: &str, work: &Path) -> Value {
    let job = request(srv, runner).await.expect("a map job waits");
    let report = assay_report(&job, work).await;
    report_and_finish(srv, &job, Some(report), "success").await;
    job
}

/// How many times the project's open pages were told its map changed, since last asked.
fn told(events: &mut Receiver<Event>, project_id: i64) -> usize {
    let mut n = 0;
    loop {
        match events.try_recv() {
            Ok(Event::SmartView { project_id: id }) if id == project_id => n += 1,
            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
            Err(_) => return n,
        }
    }
}

async fn maps_kept(srv: &TestServer) -> Vec<(Option<i64>, String, String)> {
    sqlx::query_as("SELECT merge_request_id, sha, state FROM smart_view_maps ORDER BY id").fetch_all(&srv.state.db).await.unwrap()
}

/// Off by default; turned on, a job maps the default branch's head, then each new one: the
/// blocks, their links, the people linked to their accounts; an assay.toml that cannot be read
/// says why; a report of another commit is refused; turned off, the maps go.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_job_maps_the_default_branch(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    // what the project's open pages hear: each change of its map, so they need no reload
    let mut events = srv.state.events.subscribe();
    let work = srv.work_dir("map");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    for (path, text) in [
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
        (
            "crates/api/Cargo.toml",
            "[package]\nname = \"api\"\ndescription = \"Serves the shop\"\n\n[dependencies]\naxum = \"0.8\"\ncore = { path = \"../core\" }\n",
        ),
        ("crates/api/src/main.rs", "fn main() {}\n"),
        ("crates/core/Cargo.toml", "[package]\nname = \"core\"\n"),
        ("crates/core/src/lib.rs", "\n"),
        ("Dockerfile", "FROM rust\nCOPY crates crates\nRUN cargo build -p api\n"),
        ("README.md", "# Demo\n"),
    ] {
        std::fs::create_dir_all(work.join(path).parent().unwrap()).unwrap();
        std::fs::write(work.join(path), text).unwrap();
    }
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "first"]).await);
    // Olivia's commits, under her verified address, link her account
    let olivia = [("GIT_AUTHOR_NAME", "Olivia"), ("GIT_AUTHOR_EMAIL", "olivia@example.com")];
    for n in 1..=2 {
        std::fs::write(work.join("crates/api/src/main.rs"), format!("fn main() {{ println!(\"{n}\"); }}\n")).unwrap();
        must(git_env(&work, &["commit", "-q", "-am", "greet"], &olivia).await);
    }
    let url = srv.http_url("olivia", "acme/platform/api");
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    let admin = srv.user("root", true).await;
    let runner = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared", "tags": ["maps"], "run_untagged": false })).await
        ["token"]
        .as_str()
        .unwrap()
        .to_string();

    // off: no map, no job, and a file is in no block
    let base = format!("/api/v1/projects/{p}/repository/map");
    assert_eq!(owner.ok_get(&base).await, json!({ "enabled": false, "map": null, "run": null }));
    assert!(owner.ok_get(&format!("{base}/block?path=crates/api")).await["block"].is_null());
    assert!(request(&srv, &runner).await.is_none());

    // the administrator sets the image and the runners' tags
    let status = admin.ok_get("/api/v1/admin/smart_view").await;
    let default_image = format!("ghcr.io/forgust-dev/forgust-assay:{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(status["default_image"], default_image.as_str());
    assert_eq!(status["settings"], json!({ "enabled": true, "image": "", "tags": [], "timeout_secs": 600 }));
    assert_eq!(owner.get("/api/v1/admin/smart_view").await.0, 403);
    let set = |image: &str| json!({ "enabled": true, "image": image, "tags": ["maps"], "timeout_secs": 300 });
    assert_eq!(admin.put("/api/v1/admin/smart_view", set("an image")).await.0, 400);
    let saved = admin.ok_put("/api/v1/admin/smart_view", set("registry.local/assay:1")).await;
    assert_eq!(saved["settings"]["image"], "registry.local/assay:1");

    // a Maintainer turns it on: the default branch's head is taken to map, by a Reporter bot
    let settings = format!("/api/v1/projects/{p}/smart_view");
    assert_eq!(dev.put(&settings, json!({ "enabled": true })).await.0, 403);
    assert_eq!(owner.ok_get(&settings).await, json!({ "available": true, "enabled": false }));
    assert_eq!(owner.ok_put(&settings, json!({ "enabled": true })).await, json!({ "available": true, "enabled": true }));
    assert!(told(&mut events, p) >= 1, "turned on: its pages hear the head is being mapped");
    let head = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    let waiting = owner.ok_get(&base).await;
    assert_eq!((&waiting["enabled"], &waiting["map"]), (&json!(true), &Value::Null));
    assert_eq!((&waiting["run"]["commit"], &waiting["run"]["state"]), (&json!(head), &json!("pending")), "{waiting}");
    assert!(waiting["run"]["pipeline_iid"].is_i64());
    let bot = format!("smart-view-bot-{p}");
    let members = owner.ok_get(&format!("/api/v1/projects/{p}/members")).await;
    let m = members.as_array().unwrap().iter().find(|m| m["username"] == bot.as_str()).expect("the bot is a member");
    assert_eq!(m["access_level"], "reporter");

    // the job: the image set, a full clone, the project's name, a report to upload
    let job = request(&srv, &runner).await.expect("a map job waits");
    assert_eq!(job["image"]["name"], "registry.local/assay:1");
    assert_eq!(job["git_info"]["depth"], 0, "a full clone: the history tells who knows each block");
    assert_eq!(var(&job, "FORGUST_SMART_VIEW_NAME").as_deref(), Some("api"));
    assert_eq!(var(&job, "GITLAB_USER_LOGIN").as_deref(), Some(bot.as_str()));
    assert!(job.to_string().contains("assay report --name"), "{job}");
    let report = assay_report(&job, &work).await;
    report_and_finish(&srv, &job, Some(report), "success").await;
    assert_eq!(told(&mut events, p), 1, "the map kept: its pages hear it once");

    let view = owner.ok_get(&base).await;
    assert!(view["run"].is_null(), "{view}");
    let m = &view["map"];
    assert_eq!((&m["commit"], &m["ref"]), (&json!(head), &json!("main")));
    let ids: Vec<&str> = m["blocks"].as_array().unwrap().iter().map(|b| b["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["api", "core", "image"]);
    let api = &m["blocks"][0];
    assert_eq!((&api["kind"], &api["role"], &api["description"]), (&json!("rust"), &json!("api"), &json!("Serves the shop")));
    assert_eq!(
        (&api["roots"][0], &api["manifests"][0]),
        (&json!({ "path": "crates/api", "dir": true }), &json!({ "path": "crates/api/Cargo.toml", "dir": false }))
    );
    assert_eq!(api["languages"][0], json!({ "name": "Rust", "files": 1, "color": "#dea584" }));
    let people: Vec<(&str, Option<&str>, i64)> = api["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["name"].as_str().unwrap(), x["username"].as_str(), x["commits"].as_i64().unwrap()))
        .collect();
    assert_eq!(people, [("Olivia", Some("olivia"), 2), ("Ada", None, 1)]);
    assert_eq!(api["bus_factor"], 2);
    let links: Vec<(&str, &str, &str)> = m["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["from"].as_str().unwrap(), l["to"].as_str().unwrap(), l["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(links, [("api", "core", "uses"), ("image", "api", "builds")], "the image builds only what its cargo build names");
    assert_eq!((&m["unassigned"], &m["days"], &m["config_file"]), (&json!(2), &json!(365), &json!(false)));
    assert!(m["config_error"].is_null() && m["warnings"].as_array().unwrap().is_empty(), "{m}");
    // the block of a file, of a folder
    let b = owner.ok_get(&format!("{base}/block?path=crates/api/src/main.rs")).await;
    assert_eq!(b["block"]["id"], "api");
    assert_eq!(owner.ok_get(&format!("{base}/block?path=crates/core")).await["block"]["id"], "core");
    assert!(owner.ok_get(&format!("{base}/block?path=README.md")).await["block"].is_null());

    // an assay.toml with a mistake: the last map stays shown while the new head is mapped, then
    // nothing is mapped, and the page can say why
    std::fs::write(work.join("assay.toml"), "[blocks.api]\nrenamed = \"API\"\n").unwrap();
    must(git(&work, &["add", "assay.toml"]).await);
    must(git(&work, &["commit", "-q", "-m", "name the blocks"]).await);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    let second = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    assert_eq!(told(&mut events, p), 1, "a new head: its pages hear it is being mapped");
    let during = owner.ok_get(&base).await;
    assert_eq!(
        (&during["map"]["commit"], &during["run"]["commit"], &during["run"]["state"]),
        (&json!(head), &json!(second), &json!("pending"))
    );
    run_map_job(&srv, &runner, &work).await;
    let broken = owner.ok_get(&base).await;
    assert!(broken["map"]["config_error"].as_str().unwrap().contains("renamed"), "{broken}");
    assert!(broken["map"]["blocks"].as_array().unwrap().is_empty());
    assert_eq!(broken["map"]["config_file"], true);
    // only the latest is kept
    assert_eq!(maps_kept(&srv).await, [(None, second.clone(), "ready".to_string())]);

    // a push over a commit still being mapped: that run is given up
    must(git(&work, &["rm", "-q", "assay.toml"]).await);
    must(git(&work, &["commit", "-q", "-m", "no assay.toml"]).await);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    std::fs::write(work.join("README.md"), "# Demo\n\nMore.\n").unwrap();
    must(git(&work, &["commit", "-q", "-am", "more"]).await);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    let fourth = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    assert_eq!(maps_kept(&srv).await, [(None, second.clone(), "ready".to_string()), (None, fourth.clone(), "pending".to_string())]);
    // a report of another commit is not this run's: the last map stays, and the page says why
    let job = request(&srv, &runner).await.expect("the newest commit's job waits");
    assert_eq!(var(&job, "CI_COMMIT_SHA").as_deref(), Some(fourth.as_str()));
    let mut wrong = assay_report(&job, &work).await;
    wrong["commit"] = json!(second);
    report_and_finish(&srv, &job, Some(wrong), "success").await;
    let failed = owner.ok_get(&base).await;
    assert_eq!((&failed["map"]["commit"], &failed["run"]["state"]), (&json!(second), &json!("failed")));
    assert!(told(&mut events, p) >= 1, "it could not be mapped: its pages hear it");
    assert!(failed["run"]["error"].as_str().unwrap().contains("not of this commit"), "{failed}");
    assert!(request(&srv, &runner).await.is_none(), "the run given up was canceled");

    // who cannot read the code sees none of it
    let other = srv.user("mallory", false).await;
    assert_eq!(other.get(&base).await.0, 404);

    // turned off: the maps go
    told(&mut events, p);
    owner.ok_put(&settings, json!({ "enabled": false })).await;
    assert_eq!(told(&mut events, p), 1, "turned off: its pages hear it");
    assert_eq!(owner.ok_get(&base).await, json!({ "enabled": false, "map": null, "run": null }));
    assert!(maps_kept(&srv).await.is_empty());
    assert_eq!(srv.audit_events("smart_view_enabled").await.len(), 1);
    assert_eq!(srv.audit_events("smart_view_disabled").await.len(), 1);

    // the administrator turns it off on the instance: no project turns it on
    admin.ok_put("/api/v1/admin/smart_view", json!({ "enabled": false, "image": "", "tags": [], "timeout_secs": 600 })).await;
    assert_eq!(owner.ok_get(&settings).await, json!({ "available": false, "enabled": false }));
    assert_eq!(owner.put(&settings, json!({ "enabled": true })).await.0, 409);
}

/// The Smart View and merge requests: the blocks one changes and who could review it, from the
/// default branch's map then from its own head's; once merged, its map goes and its reviewer is
/// among the people to ask.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn merge_requests_get_their_own_map(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let (org, _sub, p) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    let admin = srv.user("root", true).await;
    let runner = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared" })).await["token"].as_str().unwrap().to_string();
    let work = srv.work_dir("map-mr");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    for (path, text) in [
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
        ("crates/api/Cargo.toml", "[package]\nname = \"api\"\n\n[dependencies]\ncore = { path = \"../core\" }\n"),
        ("crates/api/src/main.rs", "fn main() {}\n"),
        ("crates/core/Cargo.toml", "[package]\nname = \"core\"\n"),
        ("crates/core/src/lib.rs", "\n"),
        ("README.md", "# Demo\n"),
    ] {
        std::fs::create_dir_all(work.join(path).parent().unwrap()).unwrap();
        std::fs::write(work.join(path), text).unwrap();
    }
    must(git(&work, &["add", "."]).await);
    // Olivia made it, under her verified address
    must(
        git_env(&work, &["commit", "-q", "-m", "first"], &[("GIT_AUTHOR_NAME", "Olivia"), ("GIT_AUTHOR_EMAIL", "olivia@example.com")])
            .await,
    );
    let url = srv.http_url("olivia", "acme/platform/api");
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_put(&format!("/api/v1/projects/{p}/smart_view"), json!({ "enabled": true })).await;
    run_map_job(&srv, &runner, &work).await;
    let map = format!("/api/v1/projects/{p}/repository/map");
    let main_head = owner.ok_get(&map).await["map"]["commit"].as_str().unwrap().to_string();

    // Dave changes the API: until its head is mapped, the blocks it touches are the default
    // branch's, with who knows them to review it, not him
    must(git(&work, &["checkout", "-q", "-b", "feature/hello"]).await);
    std::fs::write(work.join("crates/api/src/main.rs"), "fn main() { println!(\"hello\"); }\n").unwrap();
    must(
        git_env(&work, &["commit", "-q", "-am", "say hello"], &[("GIT_AUTHOR_NAME", "Dave"), ("GIT_AUTHOR_EMAIL", "dave@example.com")])
            .await,
    );
    must(git(&work, &["push", "-q", &srv.http_url("dave", "acme/platform/api"), "feature/hello"]).await);
    dev.ok_post(&format!("/api/v1/projects/{p}/merge_requests"), json!({ "source_branch": "feature/hello", "title": "Say hello" })).await;
    wait_jobs(&srv.state).await;
    let mr_head = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    let mr = format!("/api/v1/projects/{p}/merge_requests/1");
    let touched = dev.ok_get(&format!("{mr}/blocks")).await;
    assert_eq!(touched["commit"], main_head.as_str());
    let blocks: Vec<(&str, i64)> =
        touched["blocks"].as_array().unwrap().iter().map(|t| (t["block"]["id"].as_str().unwrap(), t["files"].as_i64().unwrap())).collect();
    assert_eq!(blocks, [("api", 1)], "{touched}");
    assert_eq!(touched["unassigned"], 0);
    let suggested: Vec<&str> = touched["suggested_reviewers"].as_array().unwrap().iter().map(|u| u["username"].as_str().unwrap()).collect();
    assert_eq!(suggested, ["olivia"]);
    assert_eq!(srv.user("mallory", false).await.get(&format!("{mr}/blocks")).await.0, 404);
    // the merge request's page of the Smart View: the default branch's map, its own on the way
    let of_mr = owner.ok_get(&format!("{map}?merge_request=1")).await;
    assert_eq!((&of_mr["map"]["commit"], &of_mr["run"]["commit"]), (&json!(main_head), &json!(mr_head)), "{of_mr}");

    // its head is mapped, once (its opening and its push both ask): its own map now
    let job = run_map_job(&srv, &runner, &work).await;
    assert_eq!(var(&job, "CI_COMMIT_SHA").as_deref(), Some(mr_head.as_str()));
    assert!(request(&srv, &runner).await.is_none());
    assert_eq!(dev.ok_get(&format!("{mr}/blocks")).await["commit"], mr_head.as_str());
    let of_mr = owner.ok_get(&format!("{map}?merge_request=1")).await;
    assert_eq!((&of_mr["map"]["commit"], &of_mr["map"]["ref"]), (&json!(mr_head), &json!("feature/hello")));
    assert!(of_mr["run"].is_null());
    assert_eq!(owner.ok_get(&map).await["map"]["commit"], main_head.as_str(), "the default branch keeps its own");
    assert_eq!(maps_kept(&srv).await.len(), 2);

    // pushed further, then back to the commit already mapped: the run of the commit it left is
    // given up, and would not put its map over the head's
    let dave = [("GIT_AUTHOR_NAME", "Dave"), ("GIT_AUTHOR_EMAIL", "dave@example.com")];
    let dave_url = srv.http_url("dave", "acme/platform/api");
    std::fs::write(work.join("crates/api/src/main.rs"), "fn main() { println!(\"hello, world\"); }\n").unwrap();
    must(git_env(&work, &["commit", "-q", "-am", "say more"], &dave).await);
    must(git(&work, &["push", "-q", &dave_url, "feature/hello"]).await);
    wait_jobs(&srv.state).await;
    assert_eq!(owner.ok_get(&format!("{map}?merge_request=1")).await["run"]["state"], "pending");
    must(git(&work, &["reset", "-q", "--hard", &mr_head]).await);
    must(git(&work, &["push", "-q", "-f", &dave_url, "feature/hello"]).await);
    wait_jobs(&srv.state).await;
    let of_mr = owner.ok_get(&format!("{map}?merge_request=1")).await;
    assert_eq!(of_mr["map"]["commit"], mr_head.as_str());
    assert!(of_mr["run"].is_null(), "{of_mr}");
    assert!(request(&srv, &runner).await.is_none(), "the run of the commit it left was canceled");

    // a job that fails says so at once; a file assay leaves out of every block (a fixture) is
    // in none, whatever folder holds it
    std::fs::create_dir_all(work.join("crates/api/tests/fixtures")).unwrap();
    std::fs::write(work.join("crates/api/tests/fixtures/sample.txt"), "x\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git_env(&work, &["commit", "-q", "-m", "a fixture"], &dave).await);
    must(git(&work, &["push", "-q", &dave_url, "feature/hello"]).await);
    wait_jobs(&srv.state).await;
    let job = request(&srv, &runner).await.expect("the new head's job waits");
    report_and_finish(&srv, &job, None, "failed").await;
    let of_mr = owner.ok_get(&format!("{map}?merge_request=1")).await;
    assert_eq!((&of_mr["map"]["commit"], &of_mr["run"]["state"]), (&json!(mr_head), &json!("failed")), "{of_mr}");
    assert!(of_mr["run"]["error"].as_str().unwrap().contains("failed"), "{of_mr}");
    let touched = dev.ok_get(&format!("{mr}/blocks")).await;
    let blocks: Vec<(&str, i64)> =
        touched["blocks"].as_array().unwrap().iter().map(|t| (t["block"]["id"].as_str().unwrap(), t["files"].as_i64().unwrap())).collect();
    assert_eq!((blocks, &touched["unassigned"]), (vec![("api", 1)], &json!(1)), "{touched}");
    assert!(owner.ok_get(&format!("{map}/block?path=crates/api/tests/fixtures/sample.txt")).await["block"].is_null());

    // Olivia approves it and merges it: its map goes, the default branch's new head is mapped,
    // and she has reviewed the API
    owner.ok_post(&format!("{mr}/approve"), json!({})).await;
    let st = owner.ok_get(&format!("{mr}/merge_status")).await;
    owner.ok_post(&format!("{mr}/merge"), json!({ "method": "merge", "sha": st["head_sha"] })).await;
    wait_jobs(&srv.state).await;
    forgust_server::services::smart_view::sweep(&srv.state).await.unwrap();
    assert!(maps_kept(&srv).await.iter().all(|(mr, ..)| mr.is_none()), "a merged merge request's map is not kept");
    must(git(&work, &["checkout", "-q", "main"]).await);
    must(git(&work, &["pull", "-q", "--ff-only", &url, "main"]).await);
    run_map_job(&srv, &runner, &work).await;
    let after = owner.ok_get(&map).await["map"].clone();
    let of = |id: &str| after["blocks"].as_array().unwrap().iter().find(|b| b["id"] == id).unwrap();
    let reviewers: Vec<(&str, i64)> = of("api")["reviewers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["username"].as_str().unwrap(), r["reviews"].as_i64().unwrap()))
        .collect();
    assert_eq!(reviewers, [("olivia", 1)], "{after}");
    assert!(of("core")["reviewers"].as_array().unwrap().is_empty(), "the merge request did not change it");

    // Olivia opts out by her name: she is left out of who changed it and of who reviewed it
    std::fs::write(work.join("assay.toml"), "[people]\nexclude = [\"Olivia\"]\n").unwrap();
    must(git(&work, &["add", "assay.toml"]).await);
    must(git(&work, &["commit", "-q", "-m", "leave Olivia out"]).await);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    run_map_job(&srv, &runner, &work).await;
    let out = owner.ok_get(&map).await["map"].clone();
    assert_eq!(out["config_file"], true);
    let api = out["blocks"].as_array().unwrap().iter().find(|b| b["id"] == "api").unwrap();
    assert!(api["reviewers"].as_array().unwrap().is_empty(), "{api}");
    assert!(api["people"].as_array().unwrap().iter().all(|x| x["name"] != "Olivia"), "{api}");

    // a huge `days` in assay.toml is a number like another: nothing breaks
    std::fs::write(work.join("assay.toml"), "[people]\ndays = 4000000000\n").unwrap();
    must(git(&work, &["commit", "-q", "-am", "all of history"]).await);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    run_map_job(&srv, &runner, &work).await;
    assert_eq!(owner.ok_get(&map).await["map"]["days"], 4_000_000_000u64);
}
