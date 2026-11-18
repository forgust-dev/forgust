//! M3: merge requests, review, approvals, merges, webhooks, live updates.

mod common;

use common::*;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

/// Owner (Maintainer+), a Developer, the project, and a pushed `main`.
async fn setup(srv: &TestServer) -> (Client, Client, i64, std::path::PathBuf) {
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let (org, _sub, proj) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    let work = srv.work_dir("mr");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    (owner, dev, proj, work)
}

fn mr_path(proj: i64, iid: i64) -> String {
    format!("/api/v1/projects/{proj}/merge_requests/{iid}")
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn review_approve_and_merge(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.webhooks_allow_local = true).await;
    let (owner, dev, proj, work) = setup(&srv).await;

    // a webhook receiver on loopback (allowed by the test config only)
    let (hook_url, received) = webhook_listener().await;
    owner
        .ok_post(
            &format!("/api/v1/projects/{proj}/hooks"),
            json!({ "url": hook_url, "secret": "s3cret", "events": ["merge_request", "note", "push"] }),
        )
        .await;
    owner
        .ok_post(
            &format!("/api/v1/projects/{proj}/approval_rules"),
            json!({ "name": "maintainers", "approvals_required": 1, "min_access_level": "maintainer" }),
        )
        .await;

    // the Developer pushes a feature branch and opens an MR
    must(git(&work, &["checkout", "-q", "-b", "feature/greeting"]).await);
    commit_file(&work, "greeting.txt", "hello\n", "add greeting").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "feature/greeting"]).await);
    let mr = dev
        .ok_post(
            &format!("/api/v1/projects/{proj}/merge_requests"),
            json!({ "source_branch": "feature/greeting", "title": "Add a greeting" }),
        )
        .await;
    assert_eq!(mr["iid"], 1);
    assert_eq!(mr["target_branch"], "main");
    let path = mr_path(proj, 1);
    // the same branches cannot have two open MRs
    assert_eq!(
        dev.post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "feature/greeting", "title": "again" }))
            .await
            .0,
        409
    );

    let detail = dev.ok_get(&path).await;
    assert_eq!(detail["versions"].as_array().unwrap().len(), 1);
    assert_eq!(detail["versions"][0]["files_count"], 1);
    let diff = dev.ok_get(&format!("{path}/diff")).await;
    assert_eq!(diff["files"][0]["new_path"], "greeting.txt");
    let commits = dev.ok_get(&format!("{path}/commits")).await;
    assert_eq!(commits[0]["subject"], "add greeting");
    // refs/merge-requests/1/head points at the source head
    let refs = must(git(&work, &["ls-remote", &srv.http_url("dave", REPO), "refs/merge-requests/*"]).await);
    assert!(refs.contains("refs/merge-requests/1/head"));

    // blocked: needs a Maintainer approval, and Developers cannot merge into main
    let st = dev.ok_get(&format!("{path}/merge_status")).await;
    assert_eq!(st["can_merge"], false);
    let blockers = st["blockers"].to_string();
    assert!(blockers.contains("approval rule") && blockers.contains("not allowed to merge"), "{blockers}");
    assert_eq!(dev.post(&format!("{path}/merge"), json!({})).await.0, 409);
    // authors cannot approve their own MR
    assert_eq!(dev.post(&format!("{path}/approve"), json!({})).await.0, 403);

    // review: a comment, a reply, then approval
    let note =
        owner.ok_post(&format!("{path}/notes"), json!({ "body": "Nice", "position": { "new_path": "greeting.txt", "new_line": 1 } })).await;
    dev.ok_post(&format!("{path}/notes"), json!({ "body": "Thanks", "parent_id": note["id"] })).await;
    owner.ok_post(&format!("{path}/notes/{}/resolve", note["id"]), json!({ "resolved": true })).await;
    owner.ok_post(&format!("{path}/approve"), json!({})).await;

    let st = owner.ok_get(&format!("{path}/merge_status")).await;
    assert_eq!(st["can_merge"], true, "{st}");
    // merging what was not reviewed is refused
    let (s, _) = owner.post(&format!("{path}/merge"), json!({ "sha": "0".repeat(40) })).await;
    assert_eq!(s, 409);
    let merged =
        owner.ok_post(&format!("{path}/merge"), json!({ "method": "merge", "sha": st["head_sha"], "remove_source_branch": true })).await;
    assert_eq!(merged["state"], "merged");
    let sha = merged["merge_commit_sha"].as_str().unwrap();
    let c = owner.ok_get(&format!("/api/v1/projects/{proj}/repository/commits/{sha}")).await;
    assert_eq!(c["parents"].as_array().unwrap().len(), 2);
    assert_eq!(c["author"]["name"], "olivia");
    assert_eq!(c["committer"]["name"], "Forgust");
    assert!(c["subject"].as_str().unwrap().starts_with("Merge branch 'feature/greeting' into 'main'"));
    let heads = must(git(&work, &["ls-remote", "--heads", &srv.http_url("olivia", REPO)]).await);
    assert!(heads.contains(sha) && !heads.contains("feature/greeting"), "{heads}");

    // webhooks were delivered and signed; emails went to the other participants
    wait_jobs(&srv.state).await;
    let got = received.lock().unwrap().clone();
    let events: Vec<String> = got.iter().map(|(e, _, _)| e.clone()).collect();
    for ev in ["merge_request", "note", "push"] {
        assert!(events.iter().any(|e| e == ev), "missing {ev} in {events:?}");
    }
    for (_, sig, body) in &got {
        assert_eq!(sig, &forgust_server::services::webhooks::sign("s3cret", body));
    }
    let merge_hook: Value =
        got.iter().filter(|(e, _, _)| e == "merge_request").map(|(_, _, b)| serde_json::from_slice(b).unwrap()).next_back().unwrap();
    assert_eq!(merge_hook["object_attributes"]["action"], "merge");
    let mails = srv.state.mailer.sent();
    assert!(mails.iter().any(|m| m.to == vec!["dave@example.com".to_string()] && m.body.contains("merged into main")), "{mails:?}");

    let deliveries = owner.ok_get(&format!("/api/v1/projects/{proj}/hooks")).await;
    let hook_id = deliveries[0]["id"].as_i64().unwrap();
    let log = owner.ok_get(&format!("/api/v1/projects/{proj}/hooks/{hook_id}/deliveries")).await;
    assert!(log.as_array().unwrap().iter().all(|d| d["status"] == "delivered"), "{log}");
}

/// Exit criterion: rebase-and-merge of a 20-commit MR, no working copy.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn rebase_and_merge_twenty_commits(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, _dev, proj, work) = setup(&srv).await;
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    for i in 0..20 {
        commit_file(&work, "topic.txt", &format!("{i}\n").repeat(i + 1), &format!("topic {i}")).await;
    }
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "topic"]).await);
    // main moves on meanwhile (no conflict)
    must(git(&work, &["checkout", "-q", "main"]).await);
    commit_file(&work, "other.txt", "x\n", "main moves").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    let main_before = must(git(&work, &["rev-parse", "HEAD"]).await);

    owner.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "topic", "title": "Twenty commits" })).await;
    let path = mr_path(proj, 1);
    let st = owner.ok_get(&format!("{path}/merge_status")).await;
    assert_eq!(st["fast_forward"], false);
    let merged = owner.ok_post(&format!("{path}/merge"), json!({ "method": "rebase", "sha": st["head_sha"] })).await;
    let new_main = merged["merge_commit_sha"].as_str().unwrap().to_string();

    must(git(&work, &["fetch", "-q", &srv.http_url("olivia", REPO), "main:refs/remotes/o/main", "topic:refs/remotes/o/topic"]).await);
    assert_eq!(must(git(&work, &["rev-parse", "refs/remotes/o/main"]).await), new_main);
    let linear = must(git(&work, &["rev-list", "--count", &format!("{main_before}..{new_main}")]).await);
    assert_eq!(linear, "20");
    let merges = must(git(&work, &["rev-list", "--merges", "--count", &format!("{main_before}..{new_main}")]).await);
    assert_eq!(merges, "0");
    // the source branch follows its rebased commits; authorship is kept
    assert_eq!(must(git(&work, &["rev-parse", "refs/remotes/o/topic"]).await), new_main);
    assert_eq!(must(git(&work, &["log", "-1", "--format=%an %s", &new_main]).await), "Ada topic 19");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn squash_fast_forward_and_conflicts(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, _dev, proj, work) = setup(&srv).await;
    let url = srv.http_url("olivia", REPO);

    // squash: one commit on main with the MR title, authored by the MR author
    must(git(&work, &["checkout", "-q", "-b", "sq"]).await);
    commit_file(&work, "a.txt", "1\n", "wip 1").await;
    commit_file(&work, "a.txt", "1\n2\n", "wip 2").await;
    must(git(&work, &["push", "-q", &url, "sq"]).await);
    owner
        .ok_post(
            &format!("/api/v1/projects/{proj}/merge_requests"),
            json!({ "source_branch": "sq", "title": "Squashed feature", "squash": true }),
        )
        .await;
    let m = owner.ok_post(&format!("{}/merge", mr_path(proj, 1)), json!({})).await;
    let c = owner.ok_get(&format!("/api/v1/projects/{proj}/repository/commits/{}", m["merge_commit_sha"].as_str().unwrap())).await;
    assert_eq!(c["parents"].as_array().unwrap().len(), 1);
    assert_eq!(c["subject"], "Squashed feature");

    // fast-forward: only when main has not moved
    must(git(&work, &["fetch", "-q", &url, "main"]).await);
    must(git(&work, &["checkout", "-q", "-B", "ff", "FETCH_HEAD"]).await);
    commit_file(&work, "b.txt", "b\n", "ff change").await;
    let ff_head = must(git(&work, &["rev-parse", "HEAD"]).await);
    must(git(&work, &["push", "-q", &url, "ff"]).await);
    owner.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "ff", "title": "FF" })).await;
    let st = owner.ok_get(&format!("{}/merge_status", mr_path(proj, 2))).await;
    assert_eq!(st["fast_forward"], true);
    let m = owner.ok_post(&format!("{}/merge", mr_path(proj, 2)), json!({ "method": "fast_forward" })).await;
    assert_eq!(m["merge_commit_sha"], ff_head.as_str());

    // conflicts are reported and block the merge
    must(git(&work, &["checkout", "-q", "-B", "clash", "HEAD~1"]).await);
    commit_file(&work, "b.txt", "other\n", "clash").await;
    must(git(&work, &["push", "-q", &url, "clash"]).await);
    owner.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "clash", "title": "Clash" })).await;
    let st = owner.ok_get(&format!("{}/merge_status", mr_path(proj, 3))).await;
    assert_eq!(st["conflicts"], json!(["b.txt"]));
    assert_eq!(owner.post(&format!("{}/merge", mr_path(proj, 3)), json!({})).await.0, 409);
    // close and reopen
    assert_eq!(owner.ok_post(&format!("{}/close", mr_path(proj, 3)), json!({})).await["state"], "closed");
    assert_eq!(owner.ok_post(&format!("{}/reopen", mr_path(proj, 3)), json!({})).await["state"], "opened");
    let list = owner.ok_get(&format!("/api/v1/projects/{proj}/merge_requests?state=merged")).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    // counts per state, with the list's filters
    let c = owner.ok_get(&format!("/api/v1/projects/{proj}/merge_requests_count")).await;
    assert_eq!(c, json!({ "opened": 1, "merged": 2, "closed": 0 }));
    let c = owner.ok_get(&format!("/api/v1/projects/{proj}/merge_requests_count?search=clash")).await;
    assert_eq!(c, json!({ "opened": 1, "merged": 0, "closed": 0 }));
}

/// Exit criterion: a comment stays on the right line after a force push.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn comments_follow_force_pushes(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, proj, work) = setup(&srv).await;
    let url = srv.http_url("dave", REPO);
    let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    must(git(&work, &["checkout", "-q", "-b", "doc"]).await);
    commit_file(&work, "doc.txt", &lines, "add doc").await;
    must(git(&work, &["push", "-q", &url, "doc"]).await);
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "doc", "title": "Doc" })).await;
    let path = mr_path(proj, 1);

    let on5 = owner
        .ok_post(&format!("{path}/notes"), json!({ "body": "about line 5", "position": { "new_path": "doc.txt", "new_line": 5 } }))
        .await;
    let on10 = owner
        .ok_post(&format!("{path}/notes"), json!({ "body": "about line 10", "position": { "new_path": "doc.txt", "new_line": 10 } }))
        .await;
    owner.ok_post(&format!("{path}/approve"), json!({})).await;

    // rewrite history: 3 lines inserted on top, line 10 changed; force push
    let rewritten: String = ["new a\n", "new b\n", "new c\n"].concat()
        + &(1..=20).map(|i| if i == 10 { "line TEN\n".to_string() } else { format!("line {i}\n") }).collect::<String>();
    std::fs::write(work.join("doc.txt"), rewritten).unwrap();
    must(git(&work, &["commit", "-q", "-a", "--amend", "-m", "add doc (rewritten)"]).await);
    must(git(&work, &["push", "-q", "-f", &url, "doc"]).await);
    wait_jobs(&srv.state).await;

    let detail = owner.ok_get(&path).await;
    assert_eq!(detail["versions"].as_array().unwrap().len(), 2);
    // new commits reset approvals
    assert_eq!(detail["approvals"].as_array().unwrap().len(), 0);

    let notes = owner.ok_get(&format!("{path}/notes")).await;
    let find = |id: &Value| notes.as_array().unwrap().iter().find(|n| n["id"] == *id).unwrap().clone();
    let n5 = find(&on5["id"]);
    assert_eq!(n5["version"], 1);
    assert_eq!(n5["position"]["version"], 2);
    assert_eq!(n5["position"]["new_line"], 8, "{n5}");
    assert_eq!(n5["outdated"], false);
    let n10 = find(&on10["id"]);
    assert_eq!(n10["outdated"], true, "{n10}");
    // on version 1 the note is where it was written
    let v1 = owner.ok_get(&format!("{path}/notes?version=1")).await;
    let n5v1 = v1.as_array().unwrap().iter().find(|n| n["id"] == on5["id"]).unwrap().clone();
    assert_eq!(n5v1["position"]["new_line"], 5);
    // the old head is still reachable (keep-around) for the version 1 diff
    assert_eq!(owner.get(&format!("{path}/diff?version=1")).await.0, 200);
    let cmp = owner.ok_get(&format!("{path}/diff?version=2&from_version=1")).await;
    assert_eq!(cmp["files"][0]["new_path"], "doc.txt");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn live_updates_and_permissions(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, proj, work) = setup(&srv).await;
    must(git(&work, &["checkout", "-q", "-b", "f"]).await);
    commit_file(&work, "f.txt", "f\n", "f").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "f"]).await);
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "f", "title": "F" })).await;

    // an SSE stream for this project receives MR events
    let resp = owner
        .http
        .get(format!("{}/api/v1/events?projects={proj}", srv.base))
        .header("cookie", owner.cookie.clone().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let mut stream = resp.bytes_stream();
    owner.ok_post(&format!("{}/notes", mr_path(proj, 1)), json!({ "body": "ping" })).await;
    let chunk =
        tokio::time::timeout(std::time::Duration::from_secs(5), futures::StreamExt::next(&mut stream)).await.unwrap().unwrap().unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("merge_request") && text.contains(&format!("\"project_id\":{proj}")), "{text}");

    // outsiders see nothing, not even through the event stream
    let otto = srv.user("otto", false).await;
    assert_eq!(otto.get(&mr_path(proj, 1)).await.0, 404);
    assert_eq!(otto.get(&format!("/api/v1/projects/{proj}/merge_requests")).await.0, 404);
    // only the author or a Maintainer deletes a comment; authors edit their own
    let n = dev.ok_post(&format!("{}/notes", mr_path(proj, 1)), json!({ "body": "mine" })).await;
    assert_eq!(owner.put(&format!("{}/notes/{}", mr_path(proj, 1), n["id"]), json!({ "body": "edited" })).await.0, 403);
    assert_eq!(dev.put(&format!("{}/notes/{}", mr_path(proj, 1), n["id"]), json!({ "body": "edited" })).await.0, 200);
    assert_eq!(owner.delete(&format!("{}/notes/{}", mr_path(proj, 1), n["id"])).await.0, 200);
    // webhooks to internal addresses are refused when not explicitly allowed
    let (s, _) = owner.post(&format!("/api/v1/projects/{proj}/hooks"), json!({ "url": "ftp://x", "events": ["push"] })).await;
    assert_eq!(s, 400);
}

/// Informative timing: an MR with 1,000 changed files (ADR-0008 targets are for the bench host).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn thousand_file_merge_request(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, _dev, proj, work) = setup(&srv).await;
    must(git(&work, &["checkout", "-q", "-b", "big"]).await);
    std::fs::create_dir_all(work.join("gen")).unwrap();
    for i in 0..1000 {
        std::fs::write(work.join(format!("gen/f{i:04}.rs")), format!("fn f{i}() -> u32 {{ {i} }}\n")).unwrap();
    }
    must(git(&work, &["add", "gen"]).await);
    must(git(&work, &["commit", "-q", "-m", "1000 files"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "big"]).await);
    owner.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "big", "title": "Big" })).await;
    let path = mr_path(proj, 1);
    let time = |p: String| {
        let c = &owner;
        async move {
            let t = std::time::Instant::now();
            let (s, v) = c.get(&p).await;
            assert_eq!(s, 200);
            (t.elapsed().as_secs_f64() * 1000.0, v)
        }
    };
    let (cold_page, v) = time(path.clone()).await;
    assert_eq!(v["versions"][0]["files_count"], 1000);
    let (cold_diff, d) = time(format!("{path}/diff")).await;
    assert_eq!(d["total_files"], 1000);
    assert_eq!(d["files"].as_array().unwrap().len(), 100);
    let (warm_page, _) = time(path.clone()).await;
    let (warm_diff, _) = time(format!("{path}/diff")).await;
    eprintln!(
        "1000-file MR (debug build, informative): page cold {cold_page:.1} ms warm {warm_page:.1} ms; diff page cold {cold_diff:.1} ms warm {warm_diff:.1} ms"
    );
}

/// Collects (event, signature, body) of webhook calls.
async fn webhook_listener() -> (String, std::sync::Arc<std::sync::Mutex<Vec<(String, String, Vec<u8>)>>>) {
    use axum::body::Bytes;
    use axum::http::HeaderMap;
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let g = got.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: HeaderMap, body: Bytes| {
            let g = g.clone();
            async move {
                let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                g.lock().unwrap().push((h("x-forgust-event"), h("x-forgust-signature"), body.to_vec()));
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/hook"), got)
}

/// Server-side rewrites of the source branch obey its protection like a
/// force push; MR versions follow the branch whatever order push jobs run in;
/// a rebase-merge replays the branch as checked, not the MR's head ref.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn source_rewrites_and_late_push_jobs(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, proj, work) = setup(&srv).await;
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "topic.txt", "1\n", "topic 1").await;
    let topic1 = must(git(&work, &["rev-parse", "HEAD"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "topic"]).await);
    must(git(&work, &["checkout", "-q", "main"]).await);
    commit_file(&work, "other.txt", "x\n", "main moves").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    let mr = dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "topic", "title": "Topic" })).await;
    let mr_id = mr["id"].as_i64().unwrap();
    let path = mr_path(proj, 1);

    // developers push to `topic`, nobody force-pushes it
    let protected = format!("/api/v1/projects/{proj}/protected_branches");
    let protect = |force: bool| {
        owner.post(
            &protected,
            json!({ "name": "topic", "push_access_level": "developer", "merge_access_level": "maintainer", "allow_force_push": force }),
        )
    };
    assert_eq!(protect(false).await.0, 200);
    let (s, body) = dev.post(&format!("{path}/rebase"), json!({})).await;
    assert_eq!(s, 403, "{body}");
    let st = owner.ok_get(&format!("{path}/merge_status")).await;
    let (s, body) = owner.post(&format!("{path}/merge"), json!({ "method": "rebase", "sha": st["head_sha"] })).await;
    assert_eq!(s, 403, "a rebase-merge rewrites the protected source too: {body}");
    assert_eq!(protect(true).await.0, 200);

    // move `topic` behind the push worker's back (objects via a throwaway tag)
    let sneak = |n: u32, old: String| {
        let work = work.clone();
        let srv = &srv;
        async move {
            must(git(&work, &["checkout", "-q", "topic"]).await);
            commit_file(&work, &format!("late{n}.txt"), "late\n", &format!("late {n}")).await;
            let sha = must(git(&work, &["rev-parse", "HEAD"]).await);
            must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), &format!("HEAD:refs/tags/side-late{n}")]).await);
            wait_jobs(&srv.state).await;
            srv.state.storage.update_ref(proj, "refs/heads/topic", &sha, Some(&old)).await.unwrap();
            sha
        }
    };
    let late1 = sneak(1, topic1.clone()).await;
    let update = |new: &str| forgust_server::jobs::RefUpdate { old: topic1.clone(), new: new.into(), name: "refs/heads/topic".into() };
    forgust_server::services::mr::on_push(&srv.state, proj, &[update(&late1)]).await.unwrap();
    // the job of an older push running late (or retried) changes nothing
    forgust_server::services::mr::on_push(&srv.state, proj, &[update(&topic1)]).await.unwrap();
    let latest = forgust_db::merge_requests::latest_diff(&srv.state.db, mr_id).await.unwrap().unwrap();
    assert_eq!(latest.head_sha, late1);

    // the MR head ref now lags the branch: the rebase-merge must still take everything
    let late2 = sneak(2, late1.clone()).await;
    let st = owner.ok_get(&format!("{path}/merge_status")).await;
    assert_eq!(st["head_sha"], late2.as_str());
    let merged = owner.ok_post(&format!("{path}/merge"), json!({ "method": "rebase", "sha": late2 })).await;
    let new_main = merged["merge_commit_sha"].as_str().unwrap().to_string();
    must(git(&work, &["fetch", "-q", &srv.http_url("olivia", REPO), "main:refs/remotes/o/main"]).await);
    let files = must(git(&work, &["ls-tree", "--name-only", &new_main]).await);
    for f in ["topic.txt", "late1.txt", "late2.txt", "other.txt"] {
        assert!(files.lines().any(|l| l == f), "{f} missing from {files}");
    }
}

/// A hook for a chat app gets one message per event worth telling,
/// and push hooks carry the pusher and the commits, as GitLab's.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn chat_webhooks_post_one_message(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.webhooks_allow_local = true).await;
    let (owner, dev, proj, work) = setup(&srv).await;
    let (generic_url, generic) = webhook_listener().await;
    let (slack_url, slack) = webhook_listener().await;
    let hooks = format!("/api/v1/projects/{proj}/hooks");
    assert_eq!(owner.post(&hooks, json!({ "url": slack_url, "events": ["push"], "format": "irc" })).await.0, 400);
    owner.ok_post(&hooks, json!({ "url": generic_url, "events": ["push"] })).await;
    let list = owner.ok_post(&hooks, json!({ "url": slack_url, "events": ["push", "merge_request"], "format": "slack" })).await;
    assert_eq!(list[1]["format"], "slack");

    must(git(&work, &["checkout", "-q", "-b", "feature/chat"]).await);
    commit_file(&work, "a.txt", "a\n", "First change").await;
    commit_file(&work, "b.txt", "b\n", "Second change").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "feature/chat"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "feature/chat", "title": "Chat" })).await;
    wait_jobs(&srv.state).await;
    // a new push updates the MR: the push is told, the MR update is not
    commit_file(&work, "c.txt", "c\n", "Third change").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "feature/chat"]).await);
    wait_jobs(&srv.state).await;

    let push: Value = serde_json::from_slice(&generic.lock().unwrap()[0].2).unwrap();
    assert_eq!((push["user_username"].as_str(), push["total_commits_count"].as_u64()), (Some("dave"), Some(2)), "{push}");
    assert_eq!(push["commits"][0]["title"], "Second change", "newest first, from the default branch: {push}");
    let texts: Vec<String> = slack
        .lock()
        .unwrap()
        .iter()
        .map(|(_, _, b)| serde_json::from_slice::<Value>(b).unwrap()["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(texts.len(), 3, "two pushes and the opening; not the MR update: {texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("dave pushed 1 commit to <")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("dave pushed the new branch <") && t.contains("Second change (Ada)")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("dave opened <") && t.contains("|!1 Chat>")), "{texts:?}");
    let hook_id = list[1]["id"].as_i64().unwrap();
    let log = owner.ok_get(&format!("{hooks}/{hook_id}/deliveries")).await;
    let skipped: Vec<_> =
        log.as_array().unwrap().iter().filter(|d| d["error"].as_str().is_some_and(|e| e.starts_with("not sent"))).collect();
    assert_eq!(skipped.len(), 1, "the MR update is logged, not sent: {log}");
}

/// The last delivery attempt is final even when the hook has many newer deliveries.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn webhook_last_attempt_on_a_busy_hook(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.webhooks_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, proj) = org_project(&owner).await;
    // a receiver that is down
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    owner
        .ok_post(&format!("/api/v1/projects/{proj}/hooks"), json!({ "url": format!("http://127.0.0.1:{port}/hook"), "events": ["push"] }))
        .await;
    let hook_id = owner.ok_get(&format!("/api/v1/projects/{proj}/hooks")).await[0]["id"].as_i64().unwrap();
    let db = &srv.state.db;
    let first = forgust_db::webhooks::create_delivery(db, hook_id, "push", &json!({})).await.unwrap();
    for _ in 0..210 {
        forgust_db::webhooks::create_delivery(db, hook_id, "push", &json!({})).await.unwrap();
    }
    sqlx::query("UPDATE webhook_deliveries SET attempts = $2 WHERE id = $1")
        .bind(first)
        .bind(forgust_server::services::webhooks::MAX_ATTEMPTS - 1)
        .execute(db)
        .await
        .unwrap();
    forgust_server::services::webhooks::deliver(&srv.state, first).await.expect("the last attempt is final, not retried");
    let status: String = sqlx::query_scalar("SELECT status FROM webhook_deliveries WHERE id = $1").bind(first).fetch_one(db).await.unwrap();
    assert_eq!(status, "failed");
}

/// A hook keeps the deliveries its page lists; older finished ones go with
/// their payload, and a pending one stays for its retries.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn old_webhook_deliveries_are_deleted(pool: sqlx::PgPool) {
    use forgust_db::webhooks::{KEPT_DELIVERIES, create_delivery};
    let srv = TestServer::start_with(pool, |c| c.features.webhooks_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, proj) = org_project(&owner).await;
    let hooks = format!("/api/v1/projects/{proj}/hooks");
    owner.ok_post(&hooks, json!({ "url": "http://127.0.0.1:9/busy", "events": ["push"] })).await;
    let list = owner.ok_post(&hooks, json!({ "url": "http://127.0.0.1:9/quiet", "events": ["push"] })).await;
    let (busy, quiet) = (list[0]["id"].as_i64().unwrap(), list[1]["id"].as_i64().unwrap());
    let db = &srv.state.db;
    let mut ids = Vec::new();
    for i in 0..60 {
        ids.push(create_delivery(db, busy, "push", &json!({ "i": i })).await.unwrap());
    }
    for i in 0..10 {
        create_delivery(db, quiet, "push", &json!({ "i": i })).await.unwrap();
    }
    // the oldest of the busy hook is still being retried; the rest went out
    sqlx::query("UPDATE webhook_deliveries SET status = 'delivered' WHERE id <> $1").bind(ids[0]).execute(db).await.unwrap();
    let page = format!("{hooks}/{busy}/deliveries");
    let before = owner.ok_get(&page).await;
    assert_eq!(before.as_array().unwrap().len(), KEPT_DELIVERIES as usize);

    assert_eq!(forgust_server::services::webhooks::prune_deliveries(&srv.state).await.unwrap(), 9);
    let left: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM webhook_deliveries WHERE webhook_id = $1 ORDER BY id").bind(busy).fetch_all(db).await.unwrap();
    assert_eq!(left, [&ids[..1], &ids[10..]].concat(), "the pending one and the newest 50");
    let quiet_left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM webhook_deliveries WHERE webhook_id = $1").bind(quiet).fetch_one(db).await.unwrap();
    assert_eq!(quiet_left, 10);
    assert_eq!(owner.ok_get(&page).await, before, "the page lists the same deliveries");
    assert_eq!(forgust_server::services::webhooks::prune_deliveries(&srv.state).await.unwrap(), 0);
}

async fn main_head(srv: &TestServer, proj: i64) -> String {
    srv.state.storage.resolve(proj, "refs/heads/main").await.unwrap().unwrap()
}

/// A double click, or a close racing a merge: the MR changes state once, and
/// what it says is what git did.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_merge_request_merges_or_closes_once(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, _dev, proj, work) = setup(&srv).await;
    let url = srv.http_url("olivia", REPO);
    for b in ["twice", "race"] {
        must(git(&work, &["checkout", "-q", "-b", b, "main"]).await);
        commit_file(&work, &format!("{b}.txt"), "1\n", b).await;
        must(git(&work, &["push", "-q", &url, b]).await);
        wait_jobs(&srv.state).await;
        owner.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": b, "title": b })).await;
    }

    // two merges at once: one merges, the other is refused
    let path = mr_path(proj, 1);
    let before = main_head(&srv, proj).await;
    let merge = format!("{path}/merge");
    let ((s1, b1), (s2, b2)) =
        tokio::join!(owner.post(&merge, json!({ "method": "merge" })), owner.post(&merge, json!({ "method": "merge" })));
    let mut codes = [s1, s2];
    codes.sort_unstable();
    assert_eq!(codes, [200, 409], "{b1} {b2}");
    assert_eq!(owner.ok_get(&path).await["state"], "merged");
    let after = main_head(&srv, proj).await;
    let merge_commit = srv.state.storage.commit(proj, &after).await.unwrap().unwrap();
    assert_eq!(merge_commit.parents[0], before, "one merge commit on main, not two");
    let notes = owner.ok_get(&format!("{path}/notes")).await;
    let merged_notes = notes.as_array().unwrap().iter().filter(|n| n["body"].as_str().unwrap_or("").starts_with("merged into")).count();
    assert_eq!(merged_notes, 1, "{notes}");

    // a merge and a close at once: whichever wins, the other is refused
    let path = mr_path(proj, 2);
    let before = main_head(&srv, proj).await;
    let (merge, close) = (format!("{path}/merge"), format!("{path}/close"));
    let ((sm, bm), (sc, bc)) = tokio::join!(owner.post(&merge, json!({ "method": "merge" })), owner.post(&close, json!({})));
    let state = owner.ok_get(&path).await["state"].as_str().unwrap().to_string();
    match state.as_str() {
        "merged" => {
            assert_eq!((sm, sc), (200, 409), "{bm} {bc}");
            assert_ne!(main_head(&srv, proj).await, before);
        }
        "closed" => {
            assert_eq!((sm, sc), (409, 200), "{bm} {bc}");
            assert_eq!(main_head(&srv, proj).await, before, "a closed merge request did not merge");
        }
        other => panic!("{other}: {bm} {bc}"),
    }
}

/// Approvals are for the version they were given on: commits the push worker
/// has not recorded yet do not merge with them, a page showing the older
/// version cannot merge the newer commits, and reopening records what was
/// pushed while the merge request was closed.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn approvals_do_not_cover_commits_they_were_not_given_on(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, proj, work) = setup(&srv).await;
    owner
        .ok_post(
            &format!("/api/v1/projects/{proj}/approval_rules"),
            json!({ "name": "maintainers", "approvals_required": 1, "min_access_level": "maintainer" }),
        )
        .await;
    let url = srv.http_url("dave", REPO);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "topic.txt", "1\n", "topic 1").await;
    let v1 = must(git(&work, &["rev-parse", "HEAD"]).await);
    must(git(&work, &["push", "-q", &url, "topic"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "topic", "title": "Topic" })).await;
    let path = mr_path(proj, 1);
    owner.ok_post(&format!("{path}/approve"), json!({})).await;
    assert_eq!(owner.ok_get(&format!("{path}/merge_status")).await["can_merge"], true);

    // new commits on the branch, before the push worker records them (objects
    // through a throwaway tag, the branch moved behind its back)
    commit_file(&work, "topic.txt", "2\n", "topic 2").await;
    let v2 = must(git(&work, &["rev-parse", "HEAD"]).await);
    must(git(&work, &["push", "-q", &url, "HEAD:refs/tags/side"]).await);
    wait_jobs(&srv.state).await;
    srv.state.storage.update_ref(proj, "refs/heads/topic", &v2, Some(&v1)).await.unwrap();
    let st = owner.ok_get(&format!("{path}/merge_status")).await;
    assert_eq!(st["head_sha"], v2.as_str());
    assert_eq!(st["can_merge"], false, "the approval was given on the commit before: {st}");
    // the page still shows version 1: merging is refused, and the attempt
    // records the new commits as the worker will (a version; approvals reset)
    assert_eq!(owner.post(&format!("{path}/merge"), json!({ "sha": v1 })).await.0, 409);
    let detail = owner.ok_get(&path).await;
    assert_eq!(detail["versions"].as_array().unwrap().len(), 2, "{detail}");
    assert_eq!(detail["versions"][0]["head_sha"], v2.as_str());
    assert!(detail["approvals"].as_array().unwrap().is_empty(), "{detail}");
    owner.ok_post(&format!("{path}/approve"), json!({})).await;
    assert_eq!(owner.ok_post(&format!("{path}/merge"), json!({ "sha": v2 })).await["state"], "merged");

    // closed, pushed to, reopened: the commits are a version, approvals reset
    must(git(&work, &["checkout", "-q", "-b", "later"]).await);
    commit_file(&work, "later.txt", "1\n", "later 1").await;
    must(git(&work, &["push", "-q", &url, "later"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "later", "title": "Later" })).await;
    let path = mr_path(proj, 2);
    owner.ok_post(&format!("{path}/approve"), json!({})).await;
    dev.ok_post(&format!("{path}/close"), json!({})).await;
    commit_file(&work, "later.txt", "2\n", "later 2").await;
    let pushed = must(git(&work, &["rev-parse", "HEAD"]).await);
    must(git(&work, &["push", "-q", &url, "later"]).await);
    wait_jobs(&srv.state).await;
    assert_eq!(owner.ok_get(&path).await["versions"].as_array().unwrap().len(), 1, "pushes skip closed merge requests");
    let reopened = dev.ok_post(&format!("{path}/reopen"), json!({})).await;
    assert_eq!(reopened["state"], "opened");
    let detail = owner.ok_get(&path).await;
    assert_eq!(detail["versions"].as_array().unwrap().len(), 2, "{detail}");
    assert_eq!(detail["versions"][0]["head_sha"], pushed.as_str());
    assert!(detail["approvals"].as_array().unwrap().is_empty(), "{detail}");
    assert_eq!(owner.ok_get(&format!("{path}/merge_status")).await["can_merge"], false);
}

/// One merge request that cannot follow a push (its target was force-pushed
/// to commits with no history in common) leaves the push's other merge
/// requests, and its pipelines and webhooks, to go on.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_merge_request_that_cannot_follow_does_not_stop_the_push(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, _dev, proj, work) = setup(&srv).await;
    let url = srv.http_url("olivia", REPO);
    must(git(&work, &["push", "-q", &url, "main:side"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "topic.txt", "1\n", "topic 1").await;
    must(git(&work, &["push", "-q", &url, "topic"]).await);
    wait_jobs(&srv.state).await;
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    owner.ok_post(&mrs, json!({ "source_branch": "topic", "target_branch": "side", "title": "Into side" })).await;
    owner.ok_post(&mrs, json!({ "source_branch": "topic", "target_branch": "main", "title": "Into main" })).await;
    // side becomes unrelated history
    must(git(&work, &["checkout", "-q", "--orphan", "lonely"]).await);
    commit_file(&work, "lonely.txt", "x\n", "unrelated").await;
    must(git(&work, &["push", "-q", "-f", &url, "lonely:side"]).await);
    wait_jobs(&srv.state).await;

    must(git(&work, &["checkout", "-q", "topic"]).await);
    commit_file(&work, "topic.txt", "2\n", "topic 2").await;
    let head = must(git(&work, &["rev-parse", "HEAD"]).await);
    must(git(&work, &["push", "-q", &url, "topic"]).await);
    wait_jobs(&srv.state).await;
    let into_main = owner.ok_get(&format!("{mrs}/2")).await;
    assert_eq!(into_main["versions"].as_array().unwrap().len(), 2, "{into_main}");
    assert_eq!(into_main["versions"][0]["head_sha"], head.as_str());
    // the push's follow-up succeeds as a whole
    let update = forgust_server::jobs::RefUpdate { old: head.clone(), new: head.clone(), name: "refs/heads/topic".into() };
    forgust_server::services::mr::on_push(&srv.state, proj, &[update]).await.expect("the push is followed up");
}
