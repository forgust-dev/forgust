//! AI review of merge requests: off until an administrator sets an endpoint;
//! a Maintainer turns it on; someone who can push asks for a review; Forgust
//! runs a job of its own as a Reporter bot (instance runners only, no CI
//! variables of the project, no token of the forge), and posts what the
//! job's report says only once it holds up against the merge request.

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn request(srv: &TestServer, token: &str) -> Option<Value> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", srv.base))
        .json(&json!({ "token": token, "system_id": "s_ai", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    if r.status() == StatusCode::CREATED { Some(r.json().await.unwrap()) } else { None }
}

fn var(job: &Value, k: &str) -> Option<String> {
    job["variables"].as_array().unwrap().iter().rev().find(|v| v["key"] == k).map(|v| v["value"].as_str().unwrap().to_string())
}

fn masked(job: &Value, k: &str) -> bool {
    job["variables"].as_array().unwrap().iter().any(|v| v["key"] == k && v["masked"] == true)
}

/// Upload `report` (when some) as the job's review report, and end the job.
async fn report_and_finish(srv: &TestServer, job: &Value, report: Option<Value>, state: &str) {
    if let Some(r) = report {
        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::bytes(serde_json::to_vec(&r).unwrap()).file_name("review.json"));
        let res = reqwest::Client::new()
            .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=raw&artifact_type=ai_review", srv.base, job["id"]))
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

fn review(job: &Value, comments: Value) -> Value {
    json!({
        "version": 1,
        "base_sha": var(job, "FORGUST_REVIEW_BASE_SHA").unwrap(),
        "head_sha": var(job, "FORGUST_REVIEW_HEAD_SHA").unwrap(),
        "model": "test-model",
        "summary": "Divides by zero.",
        "comments": comments,
        "skipped": [{ "path": "Cargo.lock", "reason": "generated or a lock file" }],
    })
}

/// Forgust's own pipelines are listed twice (the server and the database):
/// they say the same.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn the_system_pipelines_are_the_same_on_both_sides(pool: sqlx::PgPool) {
    let sources = [
        "push",
        "web",
        "api",
        "schedule",
        "merge_request_event",
        "trigger",
        "parent_pipeline",
        "pipeline",
        "renovate",
        "security",
        "ai_review",
    ];
    for s in sources {
        let db: bool = sqlx::query_scalar("SELECT is_system_source($1)").bind(s).fetch_one(&pool).await.unwrap();
        assert_eq!(db, forgust_server::ci::pipeline::is_system(s), "{s}");
    }
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn ai_review_is_off_until_an_administrator_sets_an_endpoint(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_, _, project) = org_project(&owner).await;
    let settings = format!("/api/v1/projects/{project}/ai_review");
    let st = owner.ok_get(&settings).await;
    assert_eq!(st["available"], false, "{st}");
    assert_eq!(owner.put(&settings, json!({ "enabled": true })).await.0, 409);

    let admin = srv.user("root", true).await;
    assert_eq!(owner.get("/api/v1/admin/ai_review").await.0, 403);
    let mut a = admin.ok_get("/api/v1/admin/ai_review").await;
    assert_eq!(a["enabled"], false);
    a["enabled"] = json!(true);
    // on needs somewhere to send the code
    assert_eq!(admin.put("/api/v1/admin/ai_review", a.clone()).await.0, 400);
    a["base_url"] = json!("https://user:pw@models.example.com/v1");
    a["model"] = json!("m");
    assert_eq!(admin.put("/api/v1/admin/ai_review", a.clone()).await.0, 400, "no credentials in the URL");
    a["base_url"] = json!("https://models.example.com/v1");
    a["api_key"] = json!("sk-not-returned");
    let (s, saved) = admin.put("/api/v1/admin/ai_review", a.clone()).await;
    assert_eq!(s, 200, "{saved}");
    assert_eq!(saved["api_key_set"], true);
    assert!(saved.get("api_key").is_none() && !saved.to_string().contains("sk-not-returned"), "{saved}");
    // saving without the key keeps it
    a.as_object_mut().unwrap().remove("api_key");
    assert_eq!(admin.ok_put("/api/v1/admin/ai_review", a.clone()).await["api_key_set"], true);
    let sealed: Option<String> = sqlx::query_scalar("SELECT api_key_enc FROM ai_review_instance").fetch_one(&srv.state.db).await.unwrap();
    assert!(!sealed.unwrap().contains("sk-not-returned"), "stored encrypted");

    // the reviewer's options: any provider name, names checked, secrets never returned
    a["provider"] = json!("anthropic");
    a["options"] = json!({ "MODEL": "x" });
    assert_eq!(admin.put("/api/v1/admin/ai_review", a.clone()).await.0, 400, "MODEL is Forgust's");
    a["options"] = json!({ "TEMPERATURE": "0.2", "max": "1" });
    assert_eq!(admin.put("/api/v1/admin/ai_review", a.clone()).await.0, 400, "A-Z, 0-9, _");
    a["options"] = json!({ "TEMPERATURE": "0.2" });
    a["secret_options"] = json!({ "AWS_SECRET_ACCESS_KEY": "aws-not-returned" });
    let saved = admin.ok_put("/api/v1/admin/ai_review", a.clone()).await;
    assert_eq!((saved["provider"].as_str(), saved["options"]["TEMPERATURE"].as_str()), (Some("anthropic"), Some("0.2")));
    assert_eq!(saved["secret_option_names"], json!(["AWS_SECRET_ACCESS_KEY"]));
    assert!(!saved.to_string().contains("aws-not-returned"), "{saved}");
    a.as_object_mut().unwrap().remove("secret_options");
    assert_eq!(admin.ok_put("/api/v1/admin/ai_review", a.clone()).await["secret_option_names"], json!(["AWS_SECRET_ACCESS_KEY"]), "kept");
    a["secret_options"] = json!({});
    assert_eq!(admin.ok_put("/api/v1/admin/ai_review", a.clone()).await["secret_option_names"], json!([]), "removed");
    a.as_object_mut().unwrap().remove("secret_options");

    let st = owner.ok_put(&settings, json!({ "enabled": true })).await;
    assert_eq!(st["available"], true);
    assert_eq!(st["enabled"], true);
    assert_eq!(st["endpoint"], "models.example.com", "people see where the code goes");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_review_runs_as_a_reporter_bot_and_only_what_holds_up_is_posted(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| {
        c.ci.long_poll_secs = 1;
        c.ai_review.enabled = true;
        c.ai_review.base_url = "http://models.internal:11434/v1".into();
        c.ai_review.model = "qwen".into();
        c.ai_review.api_key = Some("sk-model-key-123".into());
        c.ai_review.tags = vec!["ai".into()];
        c.ai_review.max_comments = 2;
        c.ai_review.options.insert("MAX_DIFF_BYTES".into(), "50000".into());
        // a name Forgust sets itself is never passed over it
        c.ai_review.options.insert("BASE_URL".into(), "http://elsewhere/v1".into());
        c.ai_review.secret_options.insert("AWS_SECRET".into(), "s3cr3t-value-42".into());
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_, _, project) = org_project(&owner).await;
    let work = srv.work_dir("ai");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "src/main.rs", "fn main() {\n    println!(\"hello, world\");\n    let x = 1 / 0;\n}\n", "divide").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/variables"), json!({ "key": "PROJECT_SECRET", "value": "never-in-a-review" })).await;
    let dev = srv.user("dave", false).await;
    let rep = srv.user("rita", false).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let mr = dev
        .ok_post(
            &format!("/api/v1/projects/{project}/merge_requests"),
            json!({ "source_branch": "topic", "title": "Divide", "description": "Why: speed." }),
        )
        .await;
    let iid = mr["iid"].as_i64().unwrap();
    let mr_id = mr["id"].as_i64().unwrap();
    wait_jobs(&srv.state).await;
    let admin = srv.user("root", true).await;
    let runner =
        admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared", "tags": ["ai"], "run_untagged": false })).await["token"]
            .as_str()
            .unwrap()
            .to_string();
    let own_runner = owner
        .ok_post(&format!("/api/v1/projects/{project}/runners"), json!({ "description": "mine", "tags": ["ai"], "run_untagged": false }))
        .await["token"]
        .as_str()
        .unwrap()
        .to_string();

    // a Maintainer turns it on; the bot joins as a Reporter
    let settings = format!("/api/v1/projects/{project}/ai_review");
    assert_eq!(dev.put(&settings, json!({ "enabled": true })).await.0, 403);
    owner.ok_put(&settings, json!({ "enabled": true })).await;
    let bot = format!("ai-review-bot-{project}");
    let members = owner.ok_get(&format!("/api/v1/projects/{project}/members")).await;
    let m = members.as_array().unwrap().iter().find(|m| m["username"] == bot.as_str()).unwrap_or_else(|| panic!("{members}"));
    assert_eq!(m["access_level"], "reporter");

    // who can push asks; a Reporter cannot
    let path = format!("/api/v1/projects/{project}/merge_requests/{iid}/ai_review");
    let st = rep.ok_get(&path).await;
    assert_eq!(st["available"], true);
    assert_eq!(st["can_request"], false);
    assert!(st["endpoint"].is_null());
    assert_eq!(rep.post(&path, json!({})).await.0, 403);
    let st = dev.ok_get(&path).await;
    assert_eq!(st["can_request"], true, "{st}");
    let (s, st) = dev.post(&path, json!({})).await;
    assert_eq!(s, 200, "{st}");
    assert_eq!(st["latest"]["state"], "pending");
    assert_eq!(dev.post(&path, json!({})).await.0, 409, "one review at a time");
    let head = srv.state.storage.resolve(project, "refs/heads/topic").await.unwrap().unwrap();
    let main_head = srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap();
    // it is no pipeline of the merge request's head
    assert_eq!(forgust_db::ci::latest_status_for_sha(&srv.state.db, project, &head, mr_id).await.unwrap(), None);

    // instance runners only; the model's settings, the key masked, nothing of the project's
    assert!(request(&srv, &own_runner).await.is_none());
    let job = request(&srv, &runner).await.expect("the review job");
    assert_eq!(var(&job, "CI_PROJECT_ID"), Some(project.to_string()));
    assert_eq!(var(&job, "AI_REVIEW_BASE_URL").as_deref(), Some("http://models.internal:11434/v1"));
    assert_eq!(var(&job, "AI_REVIEW_MODEL").as_deref(), Some("qwen"));
    assert_eq!(var(&job, "AI_REVIEW_API_KEY").as_deref(), Some("sk-model-key-123"));
    assert!(masked(&job, "AI_REVIEW_API_KEY"));
    assert_eq!(var(&job, "AI_REVIEW_PROVIDER").as_deref(), Some("openai"));
    assert_eq!(var(&job, "AI_REVIEW_MAX_DIFF_BYTES").as_deref(), Some("50000"), "the reviewer's options, passed on");
    assert_eq!(var(&job, "AI_REVIEW_AWS_SECRET").as_deref(), Some("s3cr3t-value-42"));
    assert!(masked(&job, "AI_REVIEW_AWS_SECRET"));
    assert_eq!(job["image"]["name"], format!("ghcr.io/forgust-dev/forgust-reviewer:{}", env!("CARGO_PKG_VERSION")), "the server's version");
    assert_eq!(var(&job, "FORGUST_REVIEW_HEAD_SHA"), Some(head.clone()));
    assert_eq!(var(&job, "FORGUST_REVIEW_BASE_SHA"), Some(main_head.clone()));
    assert_eq!(var(&job, "FORGUST_REVIEW_TARGET_SHA"), Some(main_head.clone()));
    assert_eq!(var(&job, "PROJECT_SECRET"), None);
    assert_eq!(var(&job, "CI_MERGE_REQUEST_DESCRIPTION").as_deref(), Some("Why: speed."));
    assert_eq!(job["git_info"]["depth"], 0, "a full clone: the base can be any older commit");

    // what holds up: on a line of the diff, at most 2, once
    let comments = json!([
        { "path": "src/main.rs", "new_line": 3, "body": "Division by zero: this panics. @olivia" },
        { "path": "src/main.rs", "new_line": 99, "body": "not in the diff" },
        { "path": "src/elsewhere.rs", "new_line": 1, "body": "not a changed file" },
        { "path": "src/main.rs", "new_line": 3, "body": "Division by zero: this panics. @olivia" },
        { "path": "src/main.rs", "new_line": 1, "body": "context line" },
        { "path": "src/main.rs", "new_line": 2, "body": "past the limit" },
    ]);
    report_and_finish(&srv, &job, Some(review(&job, comments)), "success").await;
    let st = dev.ok_get(&path).await;
    assert_eq!(st["latest"]["state"], "posted", "{st}");
    assert_eq!(st["latest"]["comments_posted"], 2);
    assert_eq!(st["latest"]["comments_dropped"], 3);
    assert_eq!(st["latest"]["current"], true);
    let notes = dev.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes")).await;
    let by_bot: Vec<&Value> = notes.as_array().unwrap().iter().filter(|n| n["author_username"] == bot.as_str()).collect();
    assert_eq!(by_bot.len(), 3, "{notes}");
    assert!(by_bot.iter().all(|n| n["author_bot"] == true));
    let summary = by_bot.iter().find(|n| n["new_path"].is_null()).unwrap();
    let body = summary["body"].as_str().unwrap();
    assert!(
        body.contains("Divides by zero.") && body.contains("test-model") && body.contains("2 threads") && body.contains("Cargo.lock"),
        "{body}"
    );
    let lines: Vec<i64> = by_bot.iter().filter_map(|n| n["new_line"].as_i64()).collect();
    assert_eq!(lines, vec![3, 1]);
    // nobody is notified or mentioned by what a model wrote
    assert!(owner.ok_get("/api/v1/user/todos").await.as_array().unwrap().is_empty());
    // asked again on the same version: all of it again, nothing posted twice
    dev.ok_post(&path, json!({})).await;
    let job = request(&srv, &runner).await.expect("the review asked again");
    assert_eq!(var(&job, "FORGUST_REVIEW_BASE_SHA"), Some(main_head.clone()));
    let again = json!([{ "path": "src/main.rs", "new_line": 3, "body": "Division by zero: this panics. @olivia" }]);
    report_and_finish(&srv, &job, Some(review(&job, again)), "success").await;
    let st = dev.ok_get(&path).await;
    assert_eq!((st["latest"]["state"].as_str(), st["latest"]["comments_posted"].as_i64()), (Some("posted"), Some(0)), "{st}");
    // its job retried (another model): the retry's report is read, and posts what is new
    let retry = dev.ok_post(&format!("/api/v1/projects/{project}/jobs/{}/retry", job["id"]), json!({})).await;
    assert!(retry["id"].is_i64(), "{retry}");
    assert_eq!(dev.ok_get(&path).await["latest"]["state"], "pending", "reviewing again");
    let job = request(&srv, &runner).await.expect("the retried job");
    let newer = json!([{ "path": "src/main.rs", "new_line": 1, "body": "From the retry." }]);
    report_and_finish(&srv, &job, Some(review(&job, newer)), "success").await;
    let st = dev.ok_get(&path).await;
    assert_eq!((st["latest"]["state"].as_str(), st["latest"]["comments_posted"].as_i64()), (Some("posted"), Some(1)), "{st}");
    let notes = dev.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes")).await;
    assert!(notes.to_string().contains("From the retry."));

    // the bot's review is advice: it cannot approve or request changes
    let bot_user = forgust_db::users::find_by_username(&srv.state.db, &bot).await.unwrap().unwrap();
    let principal = forgust_server::auth::Principal::User { user: bot_user, scopes: None };
    let p = forgust_db::projects::find(&srv.state.db, project).await.unwrap().unwrap();
    let m = forgust_db::merge_requests::find(&srv.state.db, project, iid).await.unwrap().unwrap();
    assert!(forgust_server::services::mr::approve(&srv.state, &principal, &p, &m, true).await.is_err());
    assert!(forgust_server::services::people::request_changes(&srv.state, &principal, &p, &m, Some("no")).await.is_err());

    // new commits: what changed since the last review, and its report is
    // outdated once the merge request moves on
    commit_file(&work, "src/main.rs", "fn main() {\n    println!(\"hello, world\");\n    let x = 1 / 1;\n}\n", "fix").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&srv.state).await;
    let (s, st) = dev.post(&path, json!({})).await;
    assert_eq!(s, 200, "{st}");
    let job = request(&srv, &runner).await.expect("the second review");
    assert_eq!(var(&job, "FORGUST_REVIEW_BASE_SHA"), Some(head.clone()), "since the last review");
    assert_eq!(var(&job, "FORGUST_REVIEW_TARGET_SHA"), Some(main_head.clone()));
    commit_file(&work, "src/main.rs", "fn main() {\n    println!(\"hello, world\");\n}\n", "drop it").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&srv.state).await;
    report_and_finish(&srv, &job, Some(review(&job, json!([{ "path": "src/main.rs", "new_line": 3, "body": "late" }]))), "success").await;
    let st = dev.ok_get(&path).await;
    assert_eq!(st["latest"]["state"], "outdated", "{st}");
    let notes = dev.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes")).await;
    assert!(!notes.to_string().contains("late"));

    // a failed review says why, and posts nothing
    dev.ok_post(&path, json!({})).await;
    let job = request(&srv, &runner).await.expect("the third review");
    let mut failed = review(&job, json!([]));
    failed["error"] = json!("reaching http://models.internal:11434/v1: connection refused");
    report_and_finish(&srv, &job, Some(failed), "failed").await;
    let st = dev.ok_get(&path).await;
    assert_eq!(st["latest"]["state"], "failed");
    assert!(st["latest"]["error"].as_str().unwrap().contains("connection refused"));
    // not for who cannot push: the error and the endpoint name internal hosts
    let seen = rep.ok_get(&path).await;
    assert!(seen["latest"]["error"].is_null() && seen["endpoint"].is_null(), "{seen}");
    let n = dev.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes")).await.as_array().unwrap().len();

    // a job that ends without a report: failed once the sweep sees it
    dev.ok_post(&path, json!({})).await;
    let job = request(&srv, &runner).await.expect("the fourth review");
    report_and_finish(&srv, &job, None, "failed").await;
    assert_eq!(dev.ok_get(&path).await["latest"]["state"], "pending", "no report read yet");
    sqlx::query("UPDATE ci_pipelines SET finished_at = now() - interval '10 minutes' WHERE source = 'ai_review'")
        .execute(&srv.state.db)
        .await
        .unwrap();
    forgust_server::services::ai_review::sweep(&srv.state).await.unwrap();
    let st = dev.ok_get(&path).await;
    assert_eq!(st["latest"]["state"], "failed");
    assert_eq!(dev.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes")).await.as_array().unwrap().len(), n);

    // a review no runner takes is canceled by the sweep
    dev.ok_post(&path, json!({})).await;
    sqlx::query("UPDATE ci_pipelines SET created_at = now() - interval '2 days' WHERE source = 'ai_review' AND status <> ALL('{success,failed,canceled}')")
        .execute(&srv.state.db)
        .await
        .unwrap();
    forgust_server::services::ai_review::sweep(&srv.state).await.unwrap();
    assert_eq!(dev.ok_get(&path).await["latest"]["pipeline_status"], "canceled");

    // off: reviews going on stop, no button
    dev.ok_post(&path, json!({})).await;
    owner.ok_put(&settings, json!({ "enabled": false })).await;
    let st = dev.ok_get(&path).await;
    assert_eq!(st["available"], false);
    assert_eq!(st["latest"]["pipeline_status"], "canceled");
    assert_eq!(dev.post(&path, json!({})).await.0, 409);
}

/// A stock runner (Docker executor) runs a review with the reviewer image, as
/// that image's user, against a model the test plays: the report it uploads
/// is read and posted. Opt in with `FORGUST_TEST_RUNNER=gitlab/gitlab-runner:<version>`
/// and `FORGUST_TEST_AI_REVIEW_IMAGE=<a forgust-reviewer image>` (built from
/// the repository's root: `docker build -f crates/forgust-reviewer/Dockerfile -t forgust-reviewer:local .`).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_stock_runner_runs_a_review(pool: sqlx::PgPool) {
    let (Ok(runner_image), Ok(review_image)) = (std::env::var("FORGUST_TEST_RUNNER"), std::env::var("FORGUST_TEST_AI_REVIEW_IMAGE")) else {
        eprintln!("skipped: set FORGUST_TEST_RUNNER and FORGUST_TEST_AI_REVIEW_IMAGE to run a review on a real runner");
        return;
    };
    // the model: answers one comment on the added line
    let model = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|axum::Json(b): axum::Json<Value>| async move {
            let user = b["messages"][1]["content"].as_str().unwrap_or_default().to_string();
            let line = if user.contains("L3 +     let x = 1 / 0;") { 3 } else { 1 };
            let answer =
                json!({ "summary": "Divides by zero.", "comments": [{ "path": "src/main.rs", "line": line, "body": "This panics." }] });
            axum::Json(json!({ "choices": [{ "message": { "content": answer.to_string() }, "finish_reason": "stop" }] }))
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}/v1", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, model).await.unwrap() });

    let srv = TestServer::start_with(pool, |c| {
        c.ai_review.enabled = true;
        c.ai_review.base_url = model_url.clone();
        c.ai_review.model = "stub".into();
        c.ai_review.image = review_image.clone();
        c.ai_review.tags = vec!["ai".into()];
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_, _, project) = org_project(&owner).await;
    let work = srv.work_dir("ai-real");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "src/main.rs", "fn main() {\n    println!(\"hello, world\");\n    let x = 1 / 0;\n}\n", "divide").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&srv.state).await;
    let iid = owner
        .ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "topic", "title": "Divide" }))
        .await["iid"]
        .as_i64()
        .unwrap();
    owner.ok_put(&format!("/api/v1/projects/{project}/ai_review"), json!({ "enabled": true })).await;
    let admin = srv.user("root", true).await;
    let token =
        admin.ok_post("/api/v1/admin/runners", json!({ "description": "real", "tags": ["ai"], "run_untagged": false })).await["token"]
            .as_str()
            .unwrap()
            .to_string();

    let name = format!("forgust-test-ai-runner-{}", std::process::id());
    async fn docker(args: Vec<String>) -> (bool, String) {
        let out = tokio::process::Command::new("docker").args(&args).output().await.unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
    struct Gone(String);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::process::Command::new("docker").args(["rm", "-f", &self.0]).output();
            let _ = std::process::Command::new("docker").args(["volume", "rm", "-f", &format!("{}-config", self.0)]).output();
        }
    }
    let _gone = Gone(name.clone());
    let conf = format!("{name}-config:/etc/gitlab-runner");
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let (ok, out) = docker(s(&[
        "run",
        "--rm",
        "--network",
        "host",
        "-v",
        &conf,
        &runner_image,
        "register",
        "--non-interactive",
        "--url",
        &srv.base,
        "--token",
        &token,
        "--executor",
        "docker",
        "--docker-image",
        "alpine:3.20",
        "--docker-network-mode",
        "host",
        "--docker-pull-policy",
        "if-not-present",
        "--name",
        &name,
    ]))
    .await;
    assert!(ok, "register: {out}");
    let (ok, out) = docker(s(&[
        "run",
        "-d",
        "--name",
        &name,
        "--network",
        "host",
        "-v",
        "/var/run/docker.sock:/var/run/docker.sock",
        "-v",
        &conf,
        &runner_image,
    ]))
    .await;
    assert!(ok, "runner: {out}");

    let path = format!("/api/v1/projects/{project}/merge_requests/{iid}/ai_review");
    owner.ok_post(&path, json!({})).await;
    let start = std::time::Instant::now();
    let st = loop {
        wait_jobs(&srv.state).await;
        let st = owner.ok_get(&path).await;
        if st["latest"]["state"] != "pending" || start.elapsed() > std::time::Duration::from_secs(240) {
            break st;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    };
    let job =
        owner.ok_get(&format!("/api/v1/projects/{project}/pipelines/{}", st["latest"]["pipeline_iid"])).await["jobs"][0]["id"].clone();
    let log = owner.ok_get(&format!("/api/v1/projects/{project}/jobs/{job}/trace")).await;
    assert_eq!(st["latest"]["state"], "posted", "{st}\n{}", log["content"].as_str().unwrap_or_default());
    assert_eq!(st["latest"]["comments_posted"], 1, "{st}");
    let notes = owner.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes")).await;
    assert!(
        notes.as_array().unwrap().iter().any(|n| n["new_line"] == 3 && n["body"] == "This panics." && n["author_bot"] == true),
        "{notes}"
    );
}

/// A review's changes ready to apply: one that holds up gets a suggestion
/// block people apply in one click; one whose lines do not read what the
/// model said, or a block the model wrote itself, is posted as text only.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn suggested_changes_are_checked_then_applied_by_a_person(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| {
        c.ci.long_poll_secs = 1;
        c.ai_review.enabled = true;
        c.ai_review.base_url = "http://models.internal/v1".into();
        c.ai_review.model = "m".into();
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_, _, project) = org_project(&owner).await;
    let work = srv.work_dir("ai-sugg");
    seed_repo(&work).await;
    let remote = srv.http_url("olivia", "acme/platform/api");
    must(git(&work, &["push", "-q", &remote, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "src/main.rs", "fn main() {\n    println!(\"hello, world\");\n    let x = 1 / 0;\n}\n", "divide").await;
    must(git(&work, &["push", "-q", &remote, "topic"]).await);
    wait_jobs(&srv.state).await;
    let iid = owner
        .ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "topic", "title": "Divide" }))
        .await["iid"]
        .as_i64()
        .unwrap();
    owner.ok_put(&format!("/api/v1/projects/{project}/ai_review"), json!({ "enabled": true })).await;
    let admin = srv.user("root", true).await;
    let runner = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared" })).await["token"].as_str().unwrap().to_string();
    let path = format!("/api/v1/projects/{project}/merge_requests/{iid}/ai_review");
    owner.ok_post(&path, json!({})).await;
    let job = request(&srv, &runner).await.expect("the review job");
    let comments = json!([
        { "path": "src/main.rs", "new_line": 3, "body": "Division by zero.",
          "suggestion": { "from_line": 3, "to_line": 3, "original": "    let x = 1 / 0;", "code": "    let x = 1;" } },
        { "path": "src/main.rs", "new_line": 2, "body": "Greet by name.",
          "suggestion": { "from_line": 2, "to_line": 2, "original": "println!(\"hello\");", "code": "    println!(\"hello, {name}\");" } },
        { "path": "src/main.rs", "new_line": 1, "body": "Main:\n```suggestion\nfn main() -> Result<(), ()> {\n```\nmaybe." },
    ]);
    report_and_finish(&srv, &job, Some(review(&job, comments)), "success").await;
    let st = owner.ok_get(&path).await;
    assert_eq!((st["latest"]["state"].as_str(), st["latest"]["comments_posted"].as_i64()), (Some("posted"), Some(3)), "{st}");

    let notes = owner.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes")).await;
    let on = |line: i64| notes.as_array().unwrap().iter().find(|n| n["new_line"] == line).unwrap_or_else(|| panic!("{notes}")).clone();
    let fix = on(3);
    assert_eq!(fix["body"], "Division by zero.\n\n```suggestion:-0+0\n    let x = 1;\n```", "the server writes the block");
    assert!(!on(2)["body"].as_str().unwrap().contains("```suggestion"), "the lines do not read that: text only");
    assert_eq!(on(1)["body"], "Main:\nmaybe.", "a block the model wrote is not posted");
    let summary = notes.as_array().unwrap().iter().find(|n| n["new_path"].is_null() && n["author_bot"] == true).unwrap();
    let text = summary["body"].as_str().unwrap();
    assert!(text.contains("3 threads on the changed lines, 1 with a change to apply."), "{text}");
    assert!(text.contains("1 suggested change did not match the code"), "{text}");

    // the bot never commits: a person applies it
    let r =
        owner.ok_post(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes/{}/apply_suggestion", fix["id"]), json!({})).await;
    let head = r["head_sha"].as_str().unwrap().to_string();
    let b = srv.state.storage.blob(project, &head, "src/main.rs", 1 << 20).await.unwrap();
    assert_eq!(String::from_utf8(b.data).unwrap(), "fn main() {\n    println!(\"hello, world\");\n    let x = 1;\n}\n");
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE username = 'olivia'").fetch_one(&srv.state.db).await.unwrap();
    let commit = srv.state.storage.commit(project, &head).await.unwrap().unwrap();
    assert_eq!(commit.author.email, email, "committed as who applied it, not the bot: {commit:?}");
}

/// A renamed file: the job names it by its new path, removed lines too. A
/// merge of the target branch into the merge request: the next review is of
/// all of it again, never of what the merge brought in.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn renamed_files_and_merges_of_the_target_branch(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| {
        c.ci.long_poll_secs = 1;
        c.ai_review.enabled = true;
        c.ai_review.base_url = "http://models.internal/v1".into();
        c.ai_review.model = "m".into();
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_, _, project) = org_project(&owner).await;
    let work = srv.work_dir("ai-rename");
    seed_repo(&work).await;
    let lines: Vec<String> = (1..=10).map(|i| format!("pub const N{i}: u32 = {i};")).collect();
    commit_file(&work, "src/consts.rs", &(lines.join("\n") + "\n"), "consts").await;
    let remote = srv.http_url("olivia", "acme/platform/api");
    must(git(&work, &["push", "-q", &remote, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    must(git(&work, &["mv", "src/consts.rs", "src/app.rs"]).await);
    let kept: Vec<&String> = lines.iter().filter(|l| !l.contains("N2:")).collect();
    std::fs::write(work.join("src/app.rs"), kept.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    must(git(&work, &["commit", "-q", "-am", "rename, drop N2"]).await);
    must(git(&work, &["push", "-q", &remote, "topic"]).await);
    wait_jobs(&srv.state).await;
    let iid = owner
        .ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "topic", "title": "Rename" }))
        .await["iid"]
        .as_i64()
        .unwrap();
    owner.ok_put(&format!("/api/v1/projects/{project}/ai_review"), json!({ "enabled": true })).await;
    let admin = srv.user("root", true).await;
    let runner = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared" })).await["token"].as_str().unwrap().to_string();
    let path = format!("/api/v1/projects/{project}/merge_requests/{iid}/ai_review");
    owner.ok_post(&path, json!({})).await;
    let job = request(&srv, &runner).await.expect("the review job");
    let comments = json!([{ "path": "src/app.rs", "old_line": 2, "body": "N2 is still used." }]);
    report_and_finish(&srv, &job, Some(review(&job, comments)), "success").await;
    let st = owner.ok_get(&path).await;
    assert_eq!(st["latest"]["comments_posted"], 1, "{st}");
    let notes = owner.ok_get(&format!("/api/v1/projects/{project}/merge_requests/{iid}/notes")).await;
    let n = notes.as_array().unwrap().iter().find(|n| n["old_line"] == 2).unwrap_or_else(|| panic!("{notes}"));
    assert_eq!((n["old_path"].as_str(), n["new_path"].as_str()), (Some("src/consts.rs"), Some("src/app.rs")));

    // main moves on and is merged into the branch: a review of all of it again
    must(git(&work, &["checkout", "-q", "main"]).await);
    commit_file(&work, "README.md", "# Demo\n\nMore.\n", "readme").await;
    must(git(&work, &["push", "-q", &remote, "main"]).await);
    must(git(&work, &["checkout", "-q", "topic"]).await);
    must(git(&work, &["merge", "-q", "--no-edit", "main"]).await);
    must(git(&work, &["push", "-q", &remote, "topic"]).await);
    wait_jobs(&srv.state).await;
    let first_head = var(&job, "FORGUST_REVIEW_HEAD_SHA").unwrap();
    owner.ok_post(&path, json!({})).await;
    let job = request(&srv, &runner).await.expect("the second review");
    let base = var(&job, "FORGUST_REVIEW_BASE_SHA").unwrap();
    assert_ne!(base, first_head, "not since the last review: the merge would bring main's changes in");
    assert_eq!(Some(base), var(&job, "FORGUST_REVIEW_TARGET_SHA"), "the merge request's own base");
}
