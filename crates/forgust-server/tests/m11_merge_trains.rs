//! Merge trains. Cars are tested on the merge of everything ahead of
//! them and merge in order; a failing car leaves and the train behind it is
//! rebuilt.

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";
const CI: &str = "test:\n  script: [make test]\n  rules:\n    - if: $CI_PIPELINE_SOURCE == \"merge_request_event\"\n";

async fn request(srv: &TestServer, token: &str) -> Option<Value> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", srv.base))
        .json(&json!({ "token": token, "system_id": "s_mt", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    if r.status() == StatusCode::CREATED { Some(r.json().await.unwrap()) } else { None }
}

async fn finish(srv: &TestServer, job: &Value, state: &str) {
    let r = reqwest::Client::new()
        .put(format!("{}/api/v4/jobs/{}", srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": state, "exit_code": if state == "success" { 0 } else { 1 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    wait_jobs(&srv.state).await;
}

/// A variable as the runner sees it: the last of its name wins.
fn var(job: &Value, k: &str) -> Option<String> {
    job["variables"].as_array().unwrap().iter().rev().find(|v| v["key"] == k).map(|v| v["value"].as_str().unwrap().to_string())
}

/// Take jobs until the train job of MR `iid` comes; other jobs (the MRs' own
/// pipelines) pass.
async fn train_job(srv: &TestServer, token: &str, iid: i64) -> Value {
    for _ in 0..20 {
        let Some(job) = request(srv, token).await else { break };
        if var(&job, "CI_MERGE_REQUEST_EVENT_TYPE").as_deref() == Some("merge_train")
            && var(&job, "CI_MERGE_REQUEST_IID") == Some(iid.to_string())
        {
            return job;
        }
        if var(&job, "CI_MERGE_REQUEST_EVENT_TYPE").as_deref() == Some("merge_train") {
            panic!("another car's job came first: {}", var(&job, "CI_MERGE_REQUEST_IID").unwrap_or_default());
        }
        finish(srv, &job, "success").await;
    }
    panic!("no train job for !{iid}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn merge_trains_merge_in_order_and_drop_failures(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let runner = owner.ok_post(&format!("/api/v1/projects/{project}/runners"), json!({ "description": "r" })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let work = srv.work_dir("mt");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join(".gitlab-ci.yml"), CI).unwrap();
    std::fs::write(work.join("README.md"), "x\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "init"]).await);
    let url = srv.http_url("olivia", REPO);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    let settings = |trains: bool| {
        json!({ "ci_config_path": ".gitlab-ci.yml", "instance_runners_enabled": true, "default_timeout_secs": 3600, "default_git_depth": 20,
                "only_merge_if_pipeline_succeeds": true, "job_token_scope_enabled": true, "auto_cancel_redundant": false,
                "merge_trains_enabled": trains })
    };
    let (s, body) = owner
        .put(&format!("/api/v1/projects/{project}/ci/settings"), json!({ "only_merge_if_pipeline_succeeds": false, "merge_trains_enabled": true, "ci_config_path": ".gitlab-ci.yml", "instance_runners_enabled": true, "default_timeout_secs": 3600, "default_git_depth": 20, "job_token_scope_enabled": true }))
        .await;
    assert_eq!(s, 400, "trains need pipelines to succeed: {body}");
    let (s, body) = owner.put(&format!("/api/v1/projects/{project}/ci/settings"), settings(true)).await;
    assert_eq!(s, 200, "{body}");

    // four MRs on four files
    for (i, b) in ["a", "b", "c", "d"].iter().enumerate() {
        must(git(&work, &["checkout", "-q", "-b", b, "main"]).await);
        commit_file(&work, &format!("{b}.txt"), &format!("{b}\n"), &format!("add {b}")).await;
        must(git(&work, &["push", "-q", &url, b]).await);
        wait_jobs(&srv.state).await;
        let mr = owner
            .ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": b, "title": format!("Add {b}") }))
            .await;
        assert_eq!(mr["iid"], i as i64 + 1);
    }
    wait_jobs(&srv.state).await;
    let mrp = |iid: i64| format!("/api/v1/projects/{project}/merge_requests/{iid}");

    // joining needs the right to merge into the target
    owner
        .ok_post(
            &format!("/api/v1/projects/{project}/protected_branches"),
            json!({ "name": "main", "push_access_level": "maintainer", "merge_access_level": "maintainer" }),
        )
        .await;
    let dev = srv.user("dave", false).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    assert_eq!(dev.post(&format!("{}/merge_train", mrp(1)), json!({})).await.0, 403);

    // 1 and 2 join (over the v1 and the v4 API)
    owner.ok_post(&format!("{}/merge_train", mrp(1)), json!({})).await;
    let token =
        owner.ok_post("/api/v1/user/tokens", json!({ "name": "v4", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4 = Client::with_token(&srv.base, &token);
    let (s, train) = v4.post(&format!("/api/v4/projects/{project}/merge_trains/merge_requests/2"), json!({})).await;
    assert_eq!(s, 201, "{train}");
    assert_eq!(train.as_array().unwrap().len(), 2);
    assert_eq!(train[0]["merge_request"]["iid"], 1);
    assert_eq!(owner.ok_get(&format!("{}/merge_status", mrp(2))).await["train_position"], 2);
    assert_eq!(owner.post(&format!("{}/merge_train", mrp(1)), json!({})).await.0, 409, "already on it");

    // car 2 is built on car 1
    let cars = owner.ok_get(&format!("/api/v1/projects/{project}/merge_trains?target_branch=main")).await;
    assert_eq!(cars[1]["base_sha"], cars[0]["train_sha"]);
    let main_before = srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap();
    assert_eq!(cars[0]["base_sha"], main_before);

    // car 2 passes first: it waits for car 1
    let j1 = train_job(&srv, &runner, 1).await;
    let j2 = train_job(&srv, &runner, 2).await;
    finish(&srv, &j2, "success").await;
    assert_eq!(owner.ok_get(&mrp(2)).await["state"], "opened");
    // car 1 passes: both merge, the target moves to exactly the tested commits
    finish(&srv, &j1, "success").await;
    assert_eq!(owner.ok_get(&mrp(1)).await["state"], "merged");
    assert_eq!(owner.ok_get(&mrp(2)).await["state"], "merged");
    let main_now = srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap();
    assert_eq!(main_now, cars[1]["train_sha"].as_str().unwrap(), "main is car 2's tested commit");
    assert!(owner.ok_get(&format!("/api/v1/projects/{project}/merge_trains")).await.as_array().unwrap().is_empty());

    // 3 fails: it leaves, and 4 (behind it) is rebuilt on main and merges
    owner.ok_post(&format!("{}/merge_train", mrp(3)), json!({})).await;
    owner.ok_post(&format!("{}/merge_train", mrp(4)), json!({})).await;
    let before = owner.ok_get(&format!("/api/v1/projects/{project}/merge_trains")).await;
    let j3 = train_job(&srv, &runner, 3).await;
    finish(&srv, &j3, "failed").await;
    assert_eq!(owner.ok_get(&mrp(3)).await["state"], "opened");
    let notes = owner.ok_get(&format!("{}/notes", mrp(3))).await;
    assert!(notes.as_array().unwrap().iter().any(|n| n["body"].as_str().unwrap_or("").contains("its pipeline failed")), "{notes}");
    let after = owner.ok_get(&format!("/api/v1/projects/{project}/merge_trains")).await;
    assert_eq!(after.as_array().unwrap().len(), 1);
    assert_eq!(after[0]["base_sha"], main_now.as_str(), "rebuilt on main");
    assert_ne!(after[0]["train_sha"], before[1]["train_sha"]);
    let j4 = train_job(&srv, &runner, 4).await;
    finish(&srv, &j4, "success").await;
    assert_eq!(owner.ok_get(&mrp(4)).await["state"], "merged");

    // the target moves outside the train while a car waits: the car is rebuilt
    // on it, and merging it keeps the pushed commit
    must(git(&work, &["fetch", "-q", &url, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "e", "FETCH_HEAD"]).await);
    commit_file(&work, "e.txt", "e\n", "add e").await;
    must(git(&work, &["push", "-q", &url, "e"]).await);
    owner.ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "e", "title": "Add e" })).await;
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("{}/merge_train", mrp(5)), json!({})).await;
    must(git(&work, &["checkout", "-q", "-b", "hotfix", "FETCH_HEAD"]).await);
    commit_file(&work, "hotfix.txt", "h\n", "hotfix").await;
    must(git(&work, &["push", "-q", &url, "hotfix:main"]).await);
    wait_jobs(&srv.state).await;
    let pushed = srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap();
    let cars = owner.ok_get(&format!("/api/v1/projects/{project}/merge_trains")).await;
    assert_eq!(cars[0]["base_sha"], pushed.as_str(), "rebuilt on the pushed commit: {cars}");
    let j5 = train_job(&srv, &runner, 5).await;
    finish(&srv, &j5, "success").await;
    assert_eq!(owner.ok_get(&mrp(5)).await["state"], "merged");
    let main_end = srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap();
    assert_eq!(main_end, cars[0]["train_sha"].as_str().unwrap());
    assert_eq!(
        srv.state.storage.merge_base(project, &pushed, &main_end).await.unwrap().as_deref(),
        Some(pushed.as_str()),
        "the hotfix stays"
    );

    // squashed: the train tests one commit on top of main, main moves to it,
    // and the source branch goes
    must(git(&work, &["fetch", "-q", &url, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "f", "FETCH_HEAD"]).await);
    commit_file(&work, "f.txt", "f\n", "add f").await;
    commit_file(&work, "f.txt", "f2\n", "fix f").await;
    must(git(&work, &["push", "-q", &url, "f"]).await);
    owner.ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "f", "title": "Add f" })).await;
    wait_jobs(&srv.state).await;
    let car = owner.ok_post(&format!("{}/merge_train", mrp(6)), json!({ "squash": true, "remove_source_branch": true })).await;
    assert_eq!((car["squash"].as_bool(), car["remove_source_branch"].as_bool()), (Some(true), Some(true)), "{car}");
    let tested = car["train_sha"].as_str().unwrap().to_string();
    let c = srv.state.storage.commit(project, &tested).await.unwrap().unwrap();
    assert_eq!((c.parents.clone(), c.subject.as_str()), (vec![main_end.clone()], "Add f"), "one squashed commit on main");
    let j6 = train_job(&srv, &runner, 6).await;
    finish(&srv, &j6, "success").await;
    assert_eq!(owner.ok_get(&mrp(6)).await["state"], "merged");
    assert_eq!(srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap(), tested);
    assert!(srv.state.storage.resolve(project, "refs/heads/f").await.unwrap().is_none(), "the source branch is deleted");

    // not ready: its pipeline failed, or someone requested changes
    must(git(&work, &["fetch", "-q", &url, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "g", "FETCH_HEAD"]).await);
    commit_file(&work, "g.txt", "g\n", "add g").await;
    must(git(&work, &["push", "-q", &url, "g"]).await);
    owner.ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "g", "title": "Add g" })).await;
    wait_jobs(&srv.state).await;
    let job = request(&srv, &runner).await.expect("the MR pipeline's job");
    finish(&srv, &job, "failed").await;
    let pipes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_pipelines WHERE merge_request_id = (SELECT id FROM merge_requests WHERE iid = 7)")
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    assert_eq!(pipes, 1, "one merge request pipeline for the commit, whichever of the push and the MR came first");
    let (s, body) = owner.post(&format!("{}/merge_train", mrp(7)), json!({})).await;
    assert_eq!(s, 409, "{body}");
    assert!(body.to_string().contains("pipeline failed"), "{body}");
    let rita = srv.user("rita", false).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": "rita", "access_level": "maintainer" })).await;
    let job = {
        must(git(&work, &["commit", "-q", "--allow-empty", "-m", "retry"]).await);
        must(git(&work, &["push", "-q", &url, "g"]).await);
        wait_jobs(&srv.state).await;
        request(&srv, &runner).await.expect("the new MR pipeline's job")
    };
    finish(&srv, &job, "success").await;
    rita.ok_post(&format!("{}/request_changes", mrp(7)), json!({ "body": "not yet" })).await;
    let (s, body) = owner.post(&format!("{}/merge_train", mrp(7)), json!({})).await;
    assert_eq!((s, body.to_string().contains("requested changes")), (409, true), "{body}");

    // rebased: each car is its own commits on top of what is ahead,
    // and main moves along them; no merge commit
    let main_now = srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap();
    for (b, commits) in [("h", vec!["add h", "fix h"]), ("i", vec!["add i"])] {
        // from an older main: they need the rebase
        must(git(&work, &["checkout", "-q", "-b", b, &main_end]).await);
        for (n, msg) in commits.iter().enumerate() {
            commit_file(&work, &format!("{b}.txt"), &format!("{b}{n}\n"), msg).await;
        }
        must(git(&work, &["push", "-q", &url, b]).await);
        wait_jobs(&srv.state).await;
        owner
            .ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": b, "title": format!("Add {b}") }))
            .await;
    }
    wait_jobs(&srv.state).await;
    assert_eq!(owner.post(&format!("{}/merge_train", mrp(8)), json!({ "squash": true, "rebase": true })).await.0, 400);
    let c8 = owner.ok_post(&format!("{}/merge_train", mrp(8)), json!({ "rebase": true })).await;
    assert_eq!(c8["rebase"], true, "{c8}");
    owner.ok_post(&format!("{}/merge_train", mrp(9)), json!({ "rebase": true })).await;
    let cars = owner.ok_get(&format!("/api/v1/projects/{project}/merge_trains?target_branch=main")).await;
    let (t8, t9) = (cars[0]["train_sha"].as_str().unwrap().to_string(), cars[1]["train_sha"].as_str().unwrap().to_string());
    let c = |sha: String| {
        let srv = &srv;
        async move { srv.state.storage.commit(project, &sha).await.unwrap().unwrap() }
    };
    let fix_h = c(t8.clone()).await;
    assert_eq!(fix_h.subject, "fix h");
    let add_h = c(fix_h.parents[0].clone()).await;
    assert_eq!((add_h.subject.as_str(), add_h.parents.clone()), ("add h", vec![main_now.clone()]), "h's two commits on main");
    let add_i = c(t9.clone()).await;
    assert_eq!((add_i.subject.as_str(), add_i.parents.clone()), ("add i", vec![t8.clone()]), "i's commit on h's");
    let j8 = train_job(&srv, &runner, 8).await;
    finish(&srv, &j8, "success").await;
    let j9 = train_job(&srv, &runner, 9).await;
    finish(&srv, &j9, "success").await;
    assert_eq!(
        (owner.ok_get(&mrp(8)).await["state"].as_str(), owner.ok_get(&mrp(9)).await["state"].as_str()),
        (Some("merged"), Some("merged"))
    );
    assert_eq!(srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap(), t9, "linear: main is i's commit");
}

/// A config without merge request jobs: the train runs no pipeline of its own,
/// so the car waits for the branch pipeline instead of leaving at once.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_car_without_a_train_pipeline_waits_for_the_branch_pipeline(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let runner = owner.ok_post(&format!("/api/v1/projects/{project}/runners"), json!({ "description": "r" })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let work = srv.work_dir("mt2");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join(".gitlab-ci.yml"), "test:\n  script: [make test]\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "init"]).await);
    let url = srv.http_url("olivia", REPO);
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    while let Some(job) = request(&srv, &runner).await {
        finish(&srv, &job, "success").await;
    }
    let (s, body) = owner
        .put(
            &format!("/api/v1/projects/{project}/ci/settings"),
            json!({ "ci_config_path": ".gitlab-ci.yml", "instance_runners_enabled": true, "default_timeout_secs": 3600, "default_git_depth": 20,
                    "only_merge_if_pipeline_succeeds": true, "job_token_scope_enabled": true, "merge_trains_enabled": true }),
        )
        .await;
    assert_eq!(s, 200, "{body}");
    must(git(&work, &["checkout", "-q", "-b", "f", "main"]).await);
    commit_file(&work, "f.txt", "f\n", "add f").await;
    must(git(&work, &["push", "-q", &url, "f"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "f", "title": "Add f" })).await;
    let mr = format!("/api/v1/projects/{project}/merge_requests/1");

    owner.ok_post(&format!("{mr}/merge_train"), json!({})).await;
    assert_eq!(owner.ok_get(&format!("{mr}/merge_status")).await["train_position"], 1, "waits on the train");
    assert_eq!(owner.ok_get(&mr).await["state"], "opened");
    let job = request(&srv, &runner).await.expect("the branch pipeline's job");
    assert_eq!(var(&job, "CI_COMMIT_REF_NAME").as_deref(), Some("f"));
    finish(&srv, &job, "success").await;
    assert_eq!(owner.ok_get(&mr).await["state"], "merged");

    // the branch pipeline of a car without a train pipeline fails: the car
    // leaves instead of holding up the train
    must(git(&work, &["fetch", "-q", &url, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "h", "FETCH_HEAD"]).await);
    commit_file(&work, "h.txt", "h\n", "add h").await;
    must(git(&work, &["push", "-q", &url, "h"]).await);
    wait_jobs(&srv.state).await;
    let mr_h =
        owner.ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "h", "title": "Add h" })).await;
    let mrh = format!("/api/v1/projects/{project}/merge_requests/{}", mr_h["iid"]);
    owner.ok_post(&format!("{mrh}/merge_train"), json!({})).await;
    loop {
        let job = request(&srv, &runner).await.expect("h's branch pipeline job");
        if var(&job, "CI_COMMIT_REF_NAME").as_deref() == Some("h") {
            finish(&srv, &job, "failed").await;
            break;
        }
        finish(&srv, &job, "success").await;
    }
    assert!(owner.ok_get(&format!("{mrh}/merge_status")).await["train_position"].is_null(), "off the train");
    let notes = owner.ok_get(&format!("{mrh}/notes")).await;
    assert!(notes.as_array().unwrap().iter().any(|n| n["body"].as_str().unwrap_or("").contains("its pipeline failed")), "{notes}");

    // a push to a car's source branch takes it off: the new commits were not
    // vetted by who added it
    must(git(&work, &["fetch", "-q", &url, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "g", "FETCH_HEAD"]).await);
    commit_file(&work, "g.txt", "g\n", "add g").await;
    must(git(&work, &["push", "-q", &url, "g"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "g", "title": "Add g" })).await;
    let mr2 = format!("/api/v1/projects/{project}/merge_requests/3");
    owner.ok_post(&format!("{mr2}/merge_train"), json!({})).await;
    commit_file(&work, "g.txt", "sneaky\n", "more g").await;
    must(git(&work, &["push", "-q", &url, "g"]).await);
    wait_jobs(&srv.state).await;
    assert!(owner.ok_get(&format!("{mr2}/merge_status")).await["train_position"].is_null(), "off the train");
    let notes = owner.ok_get(&format!("{mr2}/notes")).await;
    assert!(notes.as_array().unwrap().iter().any(|n| n["body"].as_str().unwrap_or("").contains("source branch was updated")), "{notes}");
}
