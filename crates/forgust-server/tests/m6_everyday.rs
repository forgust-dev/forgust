//! M6: everyday use — assignees, reviewers, and the rest of the everyday features.

mod common;

use common::*;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

/// Owner, a Developer (dave), a Reporter (rita), a pushed `main`.
async fn setup(srv: &TestServer) -> (Client, Client, Client, i64, std::path::PathBuf) {
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let rep = srv.user("rita", false).await;
    let (org, _sub, proj) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let work = srv.work_dir("m6");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    (owner, dev, rep, proj, work)
}

async fn user_id(c: &Client, proj: i64, name: &str) -> i64 {
    let list = c.ok_get(&format!("/api/v1/projects/{proj}/users?search={name}")).await;
    list.as_array().unwrap().iter().find(|u| u["username"] == name).unwrap_or_else(|| panic!("{name} in {list}"))["id"].as_i64().unwrap()
}

fn names(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|u| u["username"].as_str().unwrap().to_string()).collect()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn assignees_and_reviewers(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, work) = setup(&srv).await;
    let stranger = srv.user("sam", false).await;
    let dave = user_id(&owner, proj, "dave").await;
    let rita = user_id(&owner, proj, "rita").await;
    let olivia = user_id(&owner, proj, "olivia").await;
    // the picker lists members only (Reporter+), not strangers
    let all = owner.ok_get(&format!("/api/v1/projects/{proj}/users")).await;
    assert_eq!(names(&all), vec!["dave", "olivia", "rita"]);
    let sam = stranger.ok_get("/api/v1/user").await["id"].as_i64().unwrap();

    must(git(&work, &["checkout", "-q", "-b", "feature/a"]).await);
    commit_file(&work, "a.txt", "a\n", "add a").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "feature/a"]).await);
    let mr = dev
        .ok_post(
            &format!("/api/v1/projects/{proj}/merge_requests"),
            json!({ "source_branch": "feature/a", "title": "A", "assignee_ids": [dave], "reviewer_ids": [olivia] }),
        )
        .await;
    let path = format!("/api/v1/projects/{proj}/merge_requests/{}", mr["iid"]);
    let d = dev.ok_get(&path).await;
    assert_eq!(names(&d["assignees"]), vec!["dave"]);
    assert_eq!(names(&d["reviewers"]), vec!["olivia"]);
    assert_eq!(d["reviewers"][0]["state"], "unreviewed");
    wait_jobs(&srv.state).await;
    let mails = srv.state.mailer.sent();
    assert!(mails.iter().any(|m| m.to == vec!["olivia@example.com".to_string()] && m.body.contains("asked you to review")), "{mails:?}");
    // nobody is emailed about assigning themselves
    assert!(!mails.iter().any(|m| m.to.contains(&"dave@example.com".to_string()) && m.body.contains("assigned you")));

    // people without access cannot be assigned
    let (code, _) = dev.put(&path, json!({ "assignee_ids": [sam] })).await;
    assert_eq!(code, 400);
    // a Reporter cannot edit someone else's MR
    let (code, _) = rep.put(&path, json!({ "assignee_ids": [rita] })).await;
    assert_eq!(code, 403);

    // replace: rita joins as reviewer, olivia keeps her state
    let (code, _) = dev.put(&path, json!({ "assignee_ids": [dave, rita], "reviewer_ids": [olivia, rita] })).await;
    assert_eq!(code, 200);
    let notes = dev.ok_get(&format!("{path}/notes")).await;
    let system: Vec<&str> = notes.as_array().unwrap().iter().filter(|n| n["system"] == true).map(|n| n["body"].as_str().unwrap()).collect();
    assert!(system.contains(&"assigned to @dave"), "{system:?}");
    assert!(system.contains(&"requested review from @olivia"), "{system:?}");
    assert!(system.contains(&"assigned to @rita"), "{system:?}");
    assert!(system.contains(&"requested review from @rita"), "{system:?}");

    // a comment marks the reviewer reviewed; an approval, approved
    rep.ok_post(&format!("{path}/notes"), json!({ "body": "looks fine" })).await;
    owner.ok_post(&format!("{path}/approve"), json!({})).await;
    let d = dev.ok_get(&path).await;
    let state_of = |name: &str| d["reviewers"].as_array().unwrap().iter().find(|r| r["username"] == name).unwrap()["state"].clone();
    assert_eq!(state_of("rita"), "reviewed");
    assert_eq!(state_of("olivia"), "approved");

    // requested changes block the merge until re-requested
    rep.ok_post(&format!("{path}/request_changes"), json!({})).await;
    let st = dev.ok_get(&format!("{path}/merge_status")).await;
    assert!(st["blockers"].as_array().unwrap().iter().any(|b| b == "@rita requested changes"), "{st}");
    dev.ok_post(&format!("{path}/reviewers/{rita}/rerequest"), json!({})).await;
    let st = dev.ok_get(&format!("{path}/merge_status")).await;
    assert!(!st["blockers"].as_array().unwrap().iter().any(|b| b == "@rita requested changes"), "{st}");

    // with a comment: it says what to change, carries the verdict, and the
    // review request points at it (no separate "requested changes" note)
    let before = dev.ok_get(&format!("{path}/notes")).await.as_array().unwrap().len();
    rep.ok_post(&format!("{path}/request_changes"), json!({ "body": "Please add a test for the empty cart." })).await;
    let notes = dev.ok_get(&format!("{path}/notes")).await;
    let added = &notes.as_array().unwrap()[before..];
    assert_eq!(added.len(), 1, "{added:?}");
    assert_eq!((added[0]["system"].as_bool(), added[0]["review"].as_str()), (Some(false), Some("requested_changes")));
    assert_eq!(added[0]["body"], "Please add a test for the empty cart.");
    let d = dev.ok_get(&path).await;
    let rita_r = d["reviewers"].as_array().unwrap().iter().find(|r| r["username"] == "rita").unwrap().clone();
    assert_eq!((rita_r["state"].as_str(), rita_r["review_note_id"].as_i64()), (Some("requested_changes"), added[0]["id"].as_i64()));
    dev.ok_post(&format!("{path}/reviewers/{rita}/rerequest"), json!({})).await;
    let d = dev.ok_get(&path).await;
    let rita_r = d["reviewers"].as_array().unwrap().iter().find(|r| r["username"] == "rita").unwrap().clone();
    assert!(rita_r["review_note_id"].is_null(), "only while changes are requested: {rita_r}");

    // list filters
    let mine = dev.ok_get(&format!("/api/v1/projects/{proj}/merge_requests?reviewer_id={rita}")).await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert_eq!(names(&mine[0]["assignees"]), vec!["dave", "rita"]);
    let none = dev.ok_get(&format!("/api/v1/projects/{proj}/merge_requests?assignee_id={olivia}")).await;
    assert_eq!(none.as_array().unwrap().len(), 0);

    // issues: a Reporter may assign
    let issue = rep.ok_post(&format!("/api/v1/projects/{proj}/issues"), json!({ "title": "Bug", "assignee_ids": [dave] })).await;
    assert_eq!(names(&issue["assignees"]), vec!["dave"]);
    let ipath = format!("/api/v1/projects/{proj}/issues/{}", issue["iid"]);
    assert_eq!(names(&rep.ok_get(&ipath).await["assignees"]), vec!["dave"]);
    let (code, _) = rep.put(&ipath, json!({ "assignee_ids": [] })).await;
    assert_eq!(code, 200);
    assert_eq!(names(&rep.ok_get(&ipath).await["assignees"]), Vec::<String>::new());
    rep.put(&ipath, json!({ "assignee_ids": [rita] })).await;
    let list = rep.ok_get(&format!("/api/v1/projects/{proj}/issues?assignee_id={rita}")).await;
    assert_eq!(list["issues"].as_array().unwrap().len(), 1);
}

/// The `/api/v4` shapes Renovate and glab use.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn assignees_over_api_v4(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, _dev, _rep, proj, work) = setup(&srv).await;
    let dave = user_id(&owner, proj, "dave").await;
    let rita = user_id(&owner, proj, "rita").await;
    let tok =
        owner.ok_post("/api/v1/user/tokens", json!({ "name": "renovate", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let http = reqwest::Client::new();
    let v4 = |path: String| format!("{}/api/v4{path}", srv.base);

    must(git(&work, &["checkout", "-q", "-b", "renovate/x"]).await);
    commit_file(&work, "x.txt", "x\n", "bump x").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "renovate/x"]).await);
    let r = http
        .post(v4(format!("/projects/{proj}/merge_requests")))
        .header("PRIVATE-TOKEN", &tok)
        .json(&json!({ "source_branch": "renovate/x", "target_branch": "main", "title": "Update x", "reviewer_ids": [rita] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let mr: Value = r.json().await.unwrap();
    assert_eq!(names(&mr["reviewers"]), vec!["rita"]);
    let iid = mr["iid"].as_i64().unwrap();

    // Renovate: assignees in the query string, no body
    let r = http
        .put(v4(format!("/projects/{proj}/merge_requests/{iid}?assignee_ids[]={dave}&assignee_ids[]={rita}")))
        .header("PRIVATE-TOKEN", &tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let mr: Value = r.json().await.unwrap();
    assert_eq!(names(&mr["assignees"]), vec!["dave", "rita"]);
    assert_eq!(mr["assignee"]["username"], "dave");

    let r = http
        .get(v4(format!("/projects/{proj}/merge_requests?state=opened&reviewer_username=rita")))
        .header("PRIVATE-TOKEN", &tok)
        .send()
        .await
        .unwrap();
    let list: Value = r.json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let r = http
        .get(v4(format!("/projects/{proj}/merge_requests?state=opened&assignee_id={}", dave + 1000)))
        .header("PRIVATE-TOKEN", &tok)
        .send()
        .await
        .unwrap();
    let list: Value = r.json().await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);

    // glab: issue with assignee_ids
    let r = http
        .post(v4(format!("/projects/{proj}/issues")))
        .header("PRIVATE-TOKEN", &tok)
        .json(&json!({ "title": "Dashboard", "assignee_ids": [dave] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let issue: Value = r.json().await.unwrap();
    assert_eq!(names(&issue["assignees"]), vec!["dave"]);
}

fn changed_paths(diff: &Value) -> Vec<String> {
    let mut v: Vec<String> = diff["files"].as_array().unwrap().iter().map(|f| f["new_path"].as_str().unwrap().to_string()).collect();
    v.sort();
    v
}

/// Exit criterion: a three-MR stack merges bottom up, each diff showing only its
/// own layer after every merge (ADR-0019).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn stacked_merge_requests(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, _rep, proj, work) = setup(&srv).await;
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    let push = |b: &'static str| {
        let work = work.clone();
        let url = srv.http_url("dave", REPO);
        async move { must(git(&work, &["push", "-q", "-f", &url, b]).await) }
    };
    // three branches, each on the one before
    for (b, file) in [("layer-a", "a.txt"), ("layer-b", "b.txt"), ("layer-c", "c.txt")] {
        must(git(&work, &["checkout", "-q", "-b", b]).await);
        commit_file(&work, file, &format!("{file}\n"), &format!("add {file}")).await;
        push(b).await;
    }
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "layer-a", "title": "A" })).await;
    // stacking picks the target: the branch of the MR below
    let b = dev.ok_post(&mrs, json!({ "source_branch": "layer-b", "title": "B", "stack_on_iid": 1 })).await;
    assert_eq!(b["target_branch"], "layer-a");
    // a stack is one chain: nothing else can stack on !1 now
    let (code, body) =
        dev.post(&mrs, json!({ "source_branch": "layer-c", "title": "C", "stack_on_iid": 1, "target_branch": "layer-a" })).await;
    assert_eq!(code, 409, "{body}");
    dev.ok_post(&mrs, json!({ "source_branch": "layer-c", "title": "C", "stack_on_iid": 2 })).await;

    let v = dev.ok_get(&format!("{mrs}/2")).await;
    let stack = &v["stack"];
    assert_eq!(stack["position"], 2);
    assert_eq!(stack["size"], 3);
    assert_eq!(stack["trunk"], "main");
    assert_eq!(stack["next_to_merge"], 1);
    let order: Vec<i64> = stack["entries"].as_array().unwrap().iter().map(|e| e["iid"].as_i64().unwrap()).collect();
    assert_eq!(order, vec![1, 2, 3]);
    let list = dev.ok_get(&mrs).await;
    assert!(list.as_array().unwrap().iter().all(|m| m["stack"]["size"] == 3), "{list}");
    // each MR shows only its own layer
    assert_eq!(changed_paths(&dev.ok_get(&format!("{mrs}/2/diff")).await), vec!["b.txt"]);
    assert_eq!(changed_paths(&dev.ok_get(&format!("{mrs}/3/diff")).await), vec!["c.txt"]);

    // only the bottom can merge
    let st = owner.ok_get(&format!("{mrs}/2/merge_status")).await;
    assert!(st["blockers"].to_string().contains("merge !1 first"), "{st}");
    assert_eq!(owner.post(&format!("{mrs}/2/merge"), json!({})).await.0, 409);

    // a new commit on the bottom layer: the one above needs a rebase
    must(git(&work, &["checkout", "-q", "layer-a"]).await);
    commit_file(&work, "a2.txt", "a2\n", "more a").await;
    push("layer-a").await;
    wait_jobs(&srv.state).await;
    let v = dev.ok_get(&format!("{mrs}/2")).await;
    assert_eq!(v["stack"]["entries"][1]["needs_rebase"], true, "{}", v["stack"]);
    let r = dev.ok_post(&format!("{mrs}/2/stack/rebase"), json!({})).await;
    assert_eq!(r["rebased"], 2);
    wait_jobs(&srv.state).await;
    let v = dev.ok_get(&format!("{mrs}/3")).await;
    assert!(v["stack"]["entries"].as_array().unwrap().iter().all(|e| e["needs_rebase"] == false), "{}", v["stack"]);
    assert_eq!(changed_paths(&dev.ok_get(&format!("{mrs}/2/diff")).await), vec!["b.txt"]);
    assert_eq!(changed_paths(&dev.ok_get(&format!("{mrs}/3/diff")).await), vec!["c.txt"]);

    // squash-merge the bottom: !2 is retargeted to main and rebased onto the squash
    let st = owner.ok_get(&format!("{mrs}/1/merge_status")).await;
    assert_eq!(st["can_merge"], true, "{st}");
    owner.ok_post(&format!("{mrs}/1/merge"), json!({ "method": "squash", "sha": st["head_sha"], "remove_source_branch": true })).await;
    wait_jobs(&srv.state).await;
    let v = dev.ok_get(&format!("{mrs}/2")).await;
    assert_eq!(v["target_branch"], "main");
    assert_eq!(v["stack"]["next_to_merge"], 2);
    assert_eq!(changed_paths(&dev.ok_get(&format!("{mrs}/2/diff")).await), vec!["b.txt"]);
    assert_eq!(changed_paths(&dev.ok_get(&format!("{mrs}/3/diff")).await), vec!["c.txt"]);
    let notes = dev.ok_get(&format!("{mrs}/2/notes")).await;
    assert!(notes.to_string().contains("retargeted to `main` after !1 merged"), "{notes}");
    // layer-a was deleted: nothing targets it any more
    let branches = dev.ok_get(&format!("/api/v1/projects/{proj}/repository/branches")).await.to_string();
    assert!(!branches.contains("\"layer-a\""), "{branches}");

    // a merge commit lands !2's own commits: !3 is retargeted without a rebase
    let st = owner.ok_get(&format!("{mrs}/2/merge_status")).await;
    assert_eq!(st["can_merge"], true, "{st}");
    owner.ok_post(&format!("{mrs}/2/merge"), json!({ "method": "merge", "sha": st["head_sha"] })).await;
    wait_jobs(&srv.state).await;
    let v = dev.ok_get(&format!("{mrs}/3")).await;
    assert_eq!(v["target_branch"], "main");
    assert_eq!(changed_paths(&dev.ok_get(&format!("{mrs}/3/diff")).await), vec!["c.txt"]);
    let st = owner.ok_get(&format!("{mrs}/3/merge_status")).await;
    assert_eq!(st["can_merge"], true, "{st}");
    owner.ok_post(&format!("{mrs}/3/merge"), json!({ "method": "merge", "sha": st["head_sha"] })).await;
    let v = dev.ok_get(&format!("{mrs}/3")).await;
    assert!(v["stack"]["entries"].as_array().unwrap().iter().all(|e| e["state"] == "merged"));
    // main has everything
    let tree = dev.ok_get(&format!("/api/v1/projects/{proj}/repository/tree?ref=main")).await.to_string();
    for f in ["a.txt", "a2.txt", "b.txt", "c.txt"] {
        assert!(tree.contains(f), "{f} in {tree}");
    }
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn joining_and_leaving_a_stack(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (_owner, dev, rep, proj, work) = setup(&srv).await;
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    for (b, file) in [("one", "1.txt"), ("two", "2.txt"), ("three", "3.txt")] {
        must(git(&work, &["checkout", "-q", "-b", b]).await);
        commit_file(&work, file, "x\n", file).await;
        must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), b]).await);
    }
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "one", "title": "1" })).await;
    dev.ok_post(&mrs, json!({ "source_branch": "two", "target_branch": "one", "title": "2" })).await;
    dev.ok_post(&mrs, json!({ "source_branch": "three", "target_branch": "two", "title": "3" })).await;
    assert!(dev.ok_get(&format!("{mrs}/2")).await["stack"].is_null());
    // existing MRs join: the target must already be the branch below
    assert_eq!(dev.put(&format!("{mrs}/3/stack"), json!({ "on_iid": 1 })).await.0, 400);
    assert_eq!(rep.put(&format!("{mrs}/2/stack"), json!({ "on_iid": 1 })).await.0, 403);
    assert_eq!(dev.put(&format!("{mrs}/2/stack"), json!({ "on_iid": 1 })).await.0, 200);
    assert_eq!(dev.put(&format!("{mrs}/3/stack"), json!({ "on_iid": 2 })).await.0, 200);
    assert_eq!(dev.ok_get(&format!("{mrs}/1")).await["stack"]["size"], 3);
    // a stacked MR keeps its target
    assert_eq!(dev.put(&format!("{mrs}/3"), json!({ "target_branch": "main" })).await.0, 400);
    // closing the middle takes it and the layer above out; one left: dissolved
    dev.ok_post(&format!("{mrs}/2/close"), json!({})).await;
    for iid in 1..=3 {
        assert!(dev.ok_get(&format!("{mrs}/{iid}")).await["stack"].is_null(), "!{iid}");
    }
    // leave
    dev.ok_post(&format!("{mrs}/2/reopen"), json!({})).await;
    assert_eq!(dev.put(&format!("{mrs}/2/stack"), json!({ "on_iid": 1 })).await.0, 200);
    assert_eq!(dev.delete(&format!("{mrs}/2/stack")).await.0, 200);
    assert!(dev.ok_get(&format!("{mrs}/1")).await["stack"].is_null());
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn references_mentions_todos_and_closing(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, work) = setup(&srv).await;
    let rita = user_id(&owner, proj, "rita").await;
    let issues = format!("/api/v1/projects/{proj}/issues");
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    rep.ok_post(&issues, json!({ "title": "Crash on start" })).await; // #1
    rep.ok_post(&issues, json!({ "title": "Typo" })).await; // #2

    must(git(&work, &["checkout", "-q", "-b", "fix/crash"]).await);
    commit_file(&work, "fix.txt", "fix\n", "fix the crash").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "fix/crash"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(
        &mrs,
        json!({ "source_branch": "fix/crash", "title": "Fix the crash", "description": "Closes #1. Related to #2; cc @rita, `@olivia` in code", "reviewer_ids": [rita] }),
    )
    .await;
    wait_jobs(&srv.state).await;

    // the description links what exists, not what is in code
    let d = dev.ok_get(&format!("{mrs}/1")).await;
    let html = d["description_html"].as_str().unwrap();
    assert!(html.contains(r#"href="/acme/platform/api/-/issues/1""#), "{html}");
    assert!(html.contains(r#"class="gfm-ref user-mention""#), "{html}");
    assert!(html.contains("<code>@olivia</code>"), "{html}");
    // the issues hear about it
    let notes = rep.ok_get(&format!("{issues}/2/notes")).await.to_string();
    assert!(notes.contains("mentioned in merge request !1"), "{notes}");
    // and list it: #1 is closed by it, #2 only mentioned
    let one = rep.ok_get(&format!("{issues}/1")).await;
    assert_eq!(one["merge_requests"], json!([{ "iid": 1, "title": "Fix the crash", "state": "opened", "closes": true }]));
    assert_eq!(rep.ok_get(&format!("{issues}/2")).await["merge_requests"][0]["closes"], false);

    // rita: a review request and a mention, one to-do each; the email for the mention
    let todos = rep.ok_get("/api/v1/user/todos").await;
    let actions: Vec<&str> = todos.as_array().unwrap().iter().map(|t| t["action"].as_str().unwrap()).collect();
    assert!(actions.contains(&"review_requested") && actions.contains(&"mentioned"), "{todos}");
    assert_eq!(todos[0]["project_path"], "acme/platform/api");
    let mails = srv.state.mailer.sent();
    assert!(mails.iter().any(|m| m.to == vec!["rita@example.com".to_string()] && m.body.contains("dave mentioned you")), "{mails:?}");
    // not olivia: her name is in code
    assert!(!mails.iter().any(|m| m.to.contains(&"olivia@example.com".to_string()) && m.body.contains("mentioned you")), "{mails:?}");
    let w = rep.ok_get("/api/v1/user/work").await;
    assert_eq!(w["review_requests"][0]["iid"], 1);
    assert_eq!(w["review_requests"][0]["review_state"], "unreviewed");
    assert_eq!(w["todos_pending"], 2);
    assert_eq!(dev.ok_get("/api/v1/user/work").await["your_merge_requests"][0]["title"], "Fix the crash");

    // a comment mentioning rita: she gets the mention email, not the comment email too
    olivia_comments(&owner, &format!("{mrs}/1/notes")).await;
    wait_jobs(&srv.state).await;
    let to_rita: Vec<_> = srv
        .state
        .mailer
        .sent()
        .into_iter()
        .filter(|m| m.to.contains(&"rita@example.com".to_string()) && m.body.starts_with("olivia"))
        .collect();
    assert_eq!(to_rita.len(), 1, "{to_rita:?}");
    assert!(to_rita[0].body.contains("mentioned you"));

    // acting on the MR clears rita's to-dos for it
    rep.ok_post(&format!("{mrs}/1/notes"), json!({ "body": "on it" })).await;
    assert_eq!(rep.ok_get("/api/v1/user/work").await["todos_pending"], 0);
    let done = rep.ok_get("/api/v1/user/todos?state=done").await;
    assert!(done.as_array().unwrap().len() >= 2);

    // merging into the default branch closes #1, not #2
    let st = owner.ok_get(&format!("{mrs}/1/merge_status")).await;
    owner.ok_post(&format!("{mrs}/1/merge"), json!({ "method": "merge", "sha": st["head_sha"] })).await;
    assert_eq!(rep.ok_get(&format!("{issues}/1")).await["state"], "closed");
    assert_eq!(rep.ok_get(&format!("{issues}/2")).await["state"], "opened");
    let notes = rep.ok_get(&format!("{issues}/1/notes")).await.to_string();
    assert!(notes.contains("closed via merge request !1"), "{notes}");
    // a closed issue renders as such
    let c = rep.ok_post(&format!("{issues}/2/notes"), json!({ "body": "dup of #1" })).await;
    let _ = c;
    let notes = rep.ok_get(&format!("{issues}/2/notes")).await;
    let last = notes.as_array().unwrap().iter().rev().find(|n| n["system"] == false).unwrap();
    assert!(last["body_html"].as_str().unwrap().contains("ref-issue closed"), "{last}");
    // the mark-all endpoint
    owner.ok_post("/api/v1/user/todos/done", json!({})).await;
    assert_eq!(owner.ok_get("/api/v1/user/work").await["todos_pending"], 0);
}

async fn olivia_comments(owner: &Client, notes: &str) {
    owner.ok_post(notes, json!({ "body": "@rita can you check the log path?" })).await;
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn contributions_calendar(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, _rep, proj, work) = setup(&srv).await;
    let stranger = srv.user("sam", false).await;
    // dave: a push of two commits, an MR, a comment, an issue
    must(git(&work, &["checkout", "-q", "-b", "feat"]).await);
    commit_file(&work, "1.txt", "1\n", "one").await;
    commit_file(&work, "2.txt", "2\n", "two").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "feat"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "feat", "title": "Feat" })).await;
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests/1/notes"), json!({ "body": "ready" })).await;
    dev.ok_post(&format!("/api/v1/projects/{proj}/issues"), json!({ "title": "Idea" })).await;

    let c = owner.ok_get("/api/v1/users/dave/contributions?tz=Europe/Paris").await;
    assert_eq!(c["total"], 4, "{c}");
    assert_eq!(c["days"].as_array().unwrap().len(), 1);
    let actions: Vec<&str> = c["activity"].as_array().unwrap().iter().map(|a| a["action"].as_str().unwrap()).collect();
    assert_eq!(actions.len(), 4, "{actions:?}");
    for a in ["pushed", "opened_merge_request", "commented_merge_request", "opened_issue"] {
        assert!(actions.contains(&a), "{a} in {actions:?}");
    }
    let push = c["activity"].as_array().unwrap().iter().find(|a| a["action"] == "pushed").unwrap();
    assert_eq!(push["title"], "feat");
    assert_eq!(push["count"], 2);
    assert_eq!(push["project_path"], "acme/platform/api");

    // a private project's work is not shown to those who cannot see it
    assert_eq!(stranger.ok_get("/api/v1/users/dave/contributions").await["total"], 0);
    // profiles are for signed-in people (like the user search)
    let anon = reqwest::get(format!("{}/api/v1/users/dave/contributions", srv.base)).await.unwrap();
    assert_eq!(anon.status(), 401);
    let anon = reqwest::get(format!("{}/api/v1/users/dave", srv.base)).await.unwrap();
    assert_eq!(anon.status(), 401);
    // anyone signed in sees the profile; the private namespace's projects stay hidden
    let p = stranger.ok_get("/api/v1/users/dave").await;
    assert_eq!(p["username"], "dave");
    assert_eq!(p["is_you"], false);
    assert_eq!(p["projects"].as_array().unwrap().len(), 0);
    assert_eq!(dev.ok_get("/api/v1/users/dave").await["is_you"], true);
    let r = stranger.ok_get("/api/v1/resolve?path=dave").await;
    assert_eq!(r["kind"], "user");
    // a bad time zone falls back to UTC
    assert_eq!(owner.ok_get("/api/v1/users/dave/contributions?tz=Mars/Olympus").await["total"], 4);
    assert_eq!(owner.get("/api/v1/users/nobody/contributions").await.0, 404);
}

/// A runner stand-in: takes pending jobs and finishes the ones of `sha` with
/// `state` (others succeed), until a job of `sha` has been finished.
async fn finish_jobs(srv: &TestServer, runner: &str, sha: &str, state: &str) {
    let http = reqwest::Client::new();
    for _ in 0..20 {
        let r = http
            .post(format!("{}/api/v4/jobs/request", srv.base))
            .json(&json!({ "token": runner, "system_id": "s_m6", "info": { "version": "19.5.3", "executor": "docker" } }))
            .send()
            .await
            .unwrap();
        if r.status() != 201 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }
        let job: Value = r.json().await.unwrap();
        let mine = job["git_info"]["sha"] == sha;
        let r = http
            .put(format!("{}/api/v4/jobs/{}", srv.base, job["id"]))
            .json(&json!({ "token": job["token"], "state": if mine { state } else { "success" } }))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.status());
        if mine {
            wait_jobs(&srv.state).await;
            return;
        }
    }
    panic!("no job for {sha}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn auto_merge_when_checks_pass(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let (owner, dev, _rep, proj, work) = setup(&srv).await;
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    let runner = owner.ok_post(&format!("/api/v1/projects/{proj}/runners"), json!({})).await["token"].as_str().unwrap().to_string();
    // a one-job pipeline on every branch
    must(git(&work, &["checkout", "-q", "main"]).await);
    commit_file(&work, ".gitlab-ci.yml", "test:\n  script: [\"true\"]\n", "ci").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    let branch = |name: &'static str| {
        let work = work.clone();
        let url = srv.http_url("dave", REPO);
        async move {
            must(git(&work, &["checkout", "-q", "-B", name, "main"]).await);
            commit_file(&work, &format!("{name}.txt"), "x\n", name).await;
            must(git(&work, &["push", "-q", "-f", &url, name]).await);
            must(git(&work, &["rev-parse", "HEAD"]).await)
        }
    };

    // 1. waits for the pipeline, then merges as the person who set it
    let sha = branch("green").await;
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "green", "title": "Green" })).await;
    let r = owner.ok_post(&format!("{mrs}/1/auto_merge"), json!({ "sha": sha, "remove_source_branch": true })).await;
    assert_eq!(r["outcome"], "waiting", "{r}");
    let d = dev.ok_get(&format!("{mrs}/1")).await;
    assert_eq!(d["auto_merge_user"]["username"], "olivia");
    finish_jobs(&srv, &runner, &sha, "success").await;
    let d = dev.ok_get(&format!("{mrs}/1")).await;
    assert_eq!(d["state"], "merged", "{}", d);
    assert!(d["auto_merge_user"].is_null());
    let merged_by = d["merged_by"].as_i64().unwrap();
    assert_eq!(merged_by, user_id(&owner, proj, "olivia").await);
    let branches = dev.ok_get(&format!("/api/v1/projects/{proj}/repository/branches")).await.to_string();
    assert!(!branches.contains("\"green\""), "source branch removed");

    // 2. new commits cancel it
    let sha = branch("pushed").await;
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "pushed", "title": "Pushed" })).await;
    owner.ok_post(&format!("{mrs}/2/auto_merge"), json!({ "sha": sha })).await;
    commit_file(&work, "more.txt", "more\n", "more").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "pushed"]).await);
    wait_jobs(&srv.state).await;
    let d = dev.ok_get(&format!("{mrs}/2")).await;
    assert!(d["auto_merge_user"].is_null(), "{d}");
    assert!(dev.ok_get(&format!("{mrs}/2/notes")).await.to_string().contains("canceled auto-merge: new commits were pushed"));

    // 3. a failed pipeline cancels it
    let sha = branch("red").await;
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "red", "title": "Red" })).await;
    owner.ok_post(&format!("{mrs}/3/auto_merge"), json!({ "sha": sha })).await;
    finish_jobs(&srv, &runner, &sha, "failed").await;
    let d = dev.ok_get(&format!("{mrs}/3")).await;
    assert_eq!(d["state"], "opened");
    assert!(dev.ok_get(&format!("{mrs}/3/notes")).await.to_string().contains("canceled auto-merge: the pipeline failed"));

    // 4. waits for an approval too; canceling by hand
    owner
        .ok_post(
            &format!("/api/v1/projects/{proj}/approval_rules"),
            json!({ "name": "one", "approvals_required": 1, "min_access_level": "maintainer" }),
        )
        .await;
    let sha = branch("approve").await;
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "approve", "title": "Approve" })).await;
    finish_jobs(&srv, &runner, &sha, "success").await;
    // only someone who may merge into main can set it
    assert_eq!(dev.post(&format!("{mrs}/4/auto_merge"), json!({ "sha": sha })).await.0, 403);
    owner.ok_post(&format!("{mrs}/4/auto_merge"), json!({ "sha": sha })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(dev.ok_get(&format!("{mrs}/4")).await["state"], "opened", "no approval yet");
    owner.ok_post(&format!("{mrs}/4/approve"), json!({})).await;
    wait_jobs(&srv.state).await;
    assert_eq!(dev.ok_get(&format!("{mrs}/4")).await["state"], "merged");

    // 5. /api/v4: Renovate's merge_when_pipeline_succeeds, and cancel
    let sha = branch("renovate").await;
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "renovate", "title": "Renovate" })).await;
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "r", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let http = reqwest::Client::new();
    let r = http
        .put(format!("{}/api/v4/projects/{proj}/merge_requests/5/merge", srv.base))
        .header("PRIVATE-TOKEN", &tok)
        .json(&json!({ "merge_when_pipeline_succeeds": true, "sha": sha }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["merge_when_pipeline_succeeds"], true, "{v}");
    let r = http
        .post(format!("{}/api/v4/projects/{proj}/merge_requests/5/cancel_merge_when_pipeline_succeeds", srv.base))
        .header("PRIVATE-TOKEN", &tok)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["merge_when_pipeline_succeeds"], false);

    // 6. consent is for one target: the author retargeting cancels it
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main:release"]).await);
    let sha = branch("retarget").await;
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "retarget", "title": "Retarget" })).await;
    owner.ok_post(&format!("{mrs}/6/auto_merge"), json!({ "sha": sha })).await;
    assert_eq!(dev.put(&format!("{mrs}/6"), json!({ "target_branch": "release" })).await.0, 200);
    wait_jobs(&srv.state).await;
    finish_jobs(&srv, &runner, &sha, "success").await;
    let d = dev.ok_get(&format!("{mrs}/6")).await;
    assert_eq!(d["state"], "opened", "{d}");
    assert!(d["auto_merge_user"].is_null());
    assert!(dev.ok_get(&format!("{mrs}/6/notes")).await.to_string().contains("canceled auto-merge: the target branch changed"));
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn suggested_changes(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, work) = setup(&srv).await;
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    must(git(&work, &["checkout", "-q", "-b", "greet"]).await);
    commit_file(&work, "greet.txt", "line one\nhelo world\nline three\nline four\n", "greet").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "greet"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "greet", "title": "Greet" })).await;
    let v = dev.ok_get(&format!("{mrs}/1")).await["versions"][0]["version"].as_i64().unwrap();
    let pos = |line: i64| json!({ "version": v, "old_path": "greet.txt", "new_path": "greet.txt", "new_line": line });
    // a reviewer suggests a fix for line 2, and a two-line rewrite around line 3
    let s1 = rep.ok_post(&format!("{mrs}/1/notes"), json!({ "body": "Typo:\n```suggestion\nhello world\n```", "position": pos(2) })).await;
    let s2 = rep
        .ok_post(&format!("{mrs}/1/notes"), json!({ "body": "```suggestion:-0+1\nlines three\nand four\n```", "position": pos(3) }))
        .await;
    let plain = rep.ok_post(&format!("{mrs}/1/notes"), json!({ "body": "no suggestion here", "position": pos(1) })).await;

    // a Reporter cannot push: no applying
    assert_eq!(rep.post(&format!("{mrs}/1/notes/{}/apply_suggestion", s1["id"]), json!({})).await.0, 403);
    assert_eq!(dev.post(&format!("{mrs}/1/notes/{}/apply_suggestion", plain["id"]), json!({})).await.0, 400);
    let r = dev.ok_post(&format!("{mrs}/1/notes/{}/apply_suggestion", s1["id"]), json!({})).await;
    let head = r["head_sha"].as_str().unwrap().to_string();
    wait_jobs(&srv.state).await;
    // applied once only; the thread is resolved; a new version records it
    assert_eq!(dev.post(&format!("{mrs}/1/notes/{}/apply_suggestion", s1["id"]), json!({})).await.0, 409);
    let notes = dev.ok_get(&format!("{mrs}/1/notes")).await;
    let n1 = notes.as_array().unwrap().iter().find(|n| n["id"] == s1["id"]).unwrap();
    assert_eq!(n1["suggestion_applied_sha"], head.as_str());
    assert!(n1["resolved_at"].is_string());
    assert!(notes.to_string().contains("applied a suggestion to `greet.txt`"));
    let d = dev.ok_get(&format!("{mrs}/1")).await;
    assert_eq!(d["versions"][0]["head_sha"], head.as_str());

    // the second suggestion still applies on top (its lines did not change)
    dev.ok_post(&format!("{mrs}/1/notes/{}/apply_suggestion", s2["id"]), json!({})).await;
    wait_jobs(&srv.state).await;
    let raw = owner.get_bytes(&format!("/api/v1/projects/{proj}/repository/raw?ref=greet&path=greet.txt")).await.1;
    assert_eq!(String::from_utf8(raw).unwrap(), "line one\nhello world\nlines three\nand four\n");
    let c = owner.ok_get(&format!("/api/v1/projects/{proj}/repository/commits?ref=greet")).await;
    assert!(c.to_string().contains("Apply suggestion to greet.txt"), "{c}");

    // a suggestion on lines someone has changed since is refused
    let s3 = rep.ok_post(&format!("{mrs}/1/notes"), json!({ "body": "```suggestion\nLINE ONE\n```", "position": pos(1) })).await;
    must(git(&work, &["pull", "-q", &srv.http_url("dave", REPO), "greet"]).await);
    commit_file(&work, "greet.txt", "first line\nhello world\nlines three\nand four\n", "reword").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "greet"]).await);
    wait_jobs(&srv.state).await;
    assert_eq!(dev.post(&format!("{mrs}/1/notes/{}/apply_suggestion", s3["id"]), json!({})).await.0, 409);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn comments_on_line_ranges(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (_owner, dev, rep, proj, work) = setup(&srv).await;
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    must(git(&work, &["checkout", "-q", "-b", "many"]).await);
    commit_file(&work, "many.txt", "1\n2\n3\n4\n5\n6\n", "many").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "many"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&mrs, json!({ "source_branch": "many", "title": "Many" })).await;
    let v = dev.ok_get(&format!("{mrs}/1")).await["versions"][0]["version"].as_i64().unwrap();
    let pos = |start: i64, end: i64| json!({ "version": v, "old_path": "many.txt", "new_path": "many.txt", "start_new_line": start, "new_line": end });
    let n = rep.ok_post(&format!("{mrs}/1/notes"), json!({ "body": "these three", "position": pos(2, 4) })).await;
    assert_eq!(n["start_new_line"], 2);
    assert_eq!(n["new_line"], 4);
    let listed = dev.ok_get(&format!("{mrs}/1/notes")).await;
    assert_eq!(listed.as_array().unwrap().iter().find(|x| x["id"] == n["id"]).unwrap()["start_new_line"], 2);
    // a range ends where it starts or later
    assert_eq!(rep.post(&format!("{mrs}/1/notes"), json!({ "body": "x", "position": pos(5, 3) })).await.0, 400);
    assert_eq!(rep.post(&format!("{mrs}/1/notes"), json!({ "body": "x", "position": pos(0, 3) })).await.0, 400);
    // a suggestion over the range replaces all of it
    let s =
        rep.ok_post(&format!("{mrs}/1/notes"), json!({ "body": "```suggestion:-2+0\ntwo\nthree\nfour\n```", "position": pos(2, 4) })).await;
    dev.ok_post(&format!("{mrs}/1/notes/{}/apply_suggestion", s["id"]), json!({})).await;
    let raw = dev.get_bytes(&format!("/api/v1/projects/{proj}/repository/raw?ref=many&path=many.txt")).await.1;
    assert_eq!(String::from_utf8(raw).unwrap(), "1\ntwo\nthree\nfour\n5\n6\n");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn password_checks_in_a_session_are_limited(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let u = srv.user("victim", false).await;
    let wrong = json!({ "current": "not-it", "new": "a-brand-new-password-1" });
    let mut last = (0, Value::Null);
    for _ in 0..11 {
        last = u.put("/api/v1/user/password", wrong.clone()).await;
    }
    assert_eq!(last.0, 403);
    assert!(last.1.to_string().contains("too many attempts"), "{}", last.1);
    // even the right password waits now
    let right = json!({ "current": password("victim"), "new": "a-brand-new-password-1" });
    let (code, body) = u.put("/api/v1/user/password", right).await;
    assert_eq!(code, 403, "{body}");
}

fn link_token(body: &str, page: &str) -> String {
    let i = body.find(&format!("/{page}?token=")).unwrap_or_else(|| panic!("no {page} link in {body}"));
    body[i + page.len() + 8..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

fn last_mail_to(srv: &TestServer, to: &str) -> Option<forgust_server::services::notify::Email> {
    srv.state.mailer.sent().into_iter().rev().find(|m| m.to.iter().any(|t| t == to))
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn password_reset_by_email(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let u = srv.user("forgetful", false).await;
    let anon = Client::new(&srv.base);
    // the same answer for an unknown account, and no email
    assert_eq!(anon.post("/api/v1/auth/password/forgot", json!({ "login": "nobody" })).await.0, 200);
    assert_eq!(anon.post("/api/v1/auth/password/forgot", json!({ "login": "Forgetful@Example.com" })).await.0, 200);
    wait_jobs(&srv.state).await;
    let mail = last_mail_to(&srv, "forgetful@example.com").expect("a reset email");
    assert_eq!(mail.subject, "Reset your password");
    let token = link_token(&mail.body, "reset-password");
    assert_eq!(anon.post("/api/v1/auth/password/reset", json!({ "token": token, "password": "short" })).await.0, 400);
    // a refused password does not use the link up
    let (code, body) = anon.post("/api/v1/auth/password/reset", json!({ "token": token, "password": "a-new-long-password" })).await;
    assert_eq!(code, 200, "{body}");
    // once only; every session ends; the new password works
    assert_eq!(anon.post("/api/v1/auth/password/reset", json!({ "token": token, "password": "another-password-9" })).await.0, 400);
    assert_eq!(u.get("/api/v1/user").await.0, 401);
    let mut again = Client::new(&srv.base);
    again.login("forgetful", "a-new-long-password").await;
    assert_eq!(again.ok_get("/api/v1/user").await["username"], "forgetful");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn email_confirmation(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    sqlx::query("UPDATE instance_settings SET signup_enabled = true").execute(&srv.state.db).await.unwrap();
    // sign-up: the account starts unconfirmed and gets a link
    let anon = Client::new(&srv.base);
    let (code, body) = anon
        .post("/api/v1/auth/register", json!({ "username": "newbie", "email": "newbie@example.com", "password": "newbie-password-1" }))
        .await;
    assert_eq!(code, 200, "{body}");
    let mut newbie = Client::new(&srv.base);
    newbie.login("newbie", "newbie-password-1").await;
    let me = newbie.ok_get("/api/v1/user").await;
    assert_eq!(me["email_verified"], false);
    wait_jobs(&srv.state).await;
    let mail = last_mail_to(&srv, "newbie@example.com").expect("a confirmation email");
    assert_eq!(mail.subject, "Confirm your email address");
    let token = link_token(&mail.body, "verify-email");
    // notifications skip an unconfirmed address
    let (org, _sub, proj) = org_project(&admin).await;
    admin.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "newbie", "access_level": "developer" })).await;
    let iss = admin.ok_post(&format!("/api/v1/projects/{proj}/issues"), json!({ "title": "x" })).await;
    let nid = newbie.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    admin.put(&format!("/api/v1/projects/{proj}/issues/{}", iss["iid"]), json!({ "assignee_ids": [nid] })).await;
    wait_jobs(&srv.state).await;
    assert!(!srv.state.mailer.sent().iter().any(|m| m.to.contains(&"newbie@example.com".to_string()) && m.body.contains("assigned you")));
    // the link confirms it, for the account itself only: whoever else opens it
    // (the owner of an address someone registered) is told to sign in, and the
    // link still works afterwards
    let (code, body) = anon.post("/api/v1/auth/email/confirm", json!({ "token": token })).await;
    assert_eq!(code, 403, "{body}");
    assert!(body["message"].as_str().unwrap_or_default().contains("@newbie"), "{body}");
    let other = srv.user("other", false).await;
    assert_eq!(other.post("/api/v1/auth/email/confirm", json!({ "token": token })).await.0, 403);
    assert_eq!(newbie.ok_get("/api/v1/user").await["email_verified"], false);
    assert_eq!(newbie.ok_post("/api/v1/auth/email/confirm", json!({ "token": token })).await["email"], "newbie@example.com");
    assert_eq!(newbie.ok_get("/api/v1/user").await["email_verified"], true);
    assert_eq!(newbie.post("/api/v1/auth/email/confirm", json!({ "token": token })).await.0, 400);

    // changing the address: the old one stays until the new one is confirmed
    let (code, body) = newbie.put("/api/v1/user", json!({ "name": "Newbie", "email": "moved@example.com" })).await;
    assert_eq!(code, 403, "a new address needs the password: {body}");
    let (code, body) = newbie
        .put("/api/v1/user", json!({ "name": "Newbie", "email": "moved@example.com", "current_password": "newbie-password-1" }))
        .await;
    assert_eq!(code, 200, "{body}");
    let me = newbie.ok_get("/api/v1/user").await;
    assert_eq!(me["email"], "newbie@example.com");
    assert_eq!(me["unconfirmed_email"], "moved@example.com");
    wait_jobs(&srv.state).await;
    let token = link_token(&last_mail_to(&srv, "moved@example.com").expect("a link to the new address").body, "verify-email");
    newbie.ok_post("/api/v1/auth/email/confirm", json!({ "token": token })).await;
    let me = newbie.ok_get("/api/v1/user").await;
    assert_eq!(me["email"], "moved@example.com");
    assert!(me["unconfirmed_email"].is_null());
    assert_eq!(me["email_verified"], true);
    // nothing left to confirm
    assert_eq!(newbie.post("/api/v1/user/email/resend", json!({})).await.0, 400);
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 200]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}

async fn upload(c: &Client, path: &str, body: Vec<u8>) -> u16 {
    let mut r = c.http.put(format!("{}{path}", c.base)).body(body);
    if let Some(k) = &c.cookie {
        r = r.header("cookie", k);
    }
    if let Some(t) = &c.csrf {
        r = r.header("x-csrf-token", t);
    }
    r.send().await.unwrap().status().as_u16()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn avatars(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, _rep, proj, _work) = setup(&srv).await;
    let anon = reqwest::Client::new();
    // nobody has one yet: 404 (and the same for an unknown name)
    assert_eq!(anon.get(format!("{}/api/v1/users/dave/avatar", srv.base)).send().await.unwrap().status(), 404);
    assert_eq!(anon.get(format!("{}/api/v1/users/nobody/avatar", srv.base)).send().await.unwrap().status(), 404);
    // not an image
    assert_eq!(upload(&dev, "/api/v1/user/avatar", b"<svg/>".to_vec()).await, 400);
    assert_eq!(upload(&dev, "/api/v1/user/avatar", png(600, 300)).await, 204);
    let me = dev.ok_get("/api/v1/user").await;
    let url = me["avatar_url"].as_str().unwrap().to_string();
    assert!(url.starts_with("/api/v1/users/dave/avatar?v="), "{url}");
    let r = anon.get(format!("{}{url}", srv.base)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    let etag = r.headers()["etag"].to_str().unwrap().to_string();
    let img = image::load_from_memory(&r.bytes().await.unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (256, 256));
    // revalidation
    let r = anon.get(format!("{}{url}", srv.base)).header("if-none-match", &etag).send().await.unwrap();
    assert_eq!(r.status(), 304);
    // removed
    let (code, _) = dev.delete("/api/v1/user/avatar").await;
    assert_eq!(code, 204);
    assert!(dev.ok_get("/api/v1/user").await["avatar_url"].is_null());
    assert_eq!(anon.get(format!("{}/api/v1/users/dave/avatar", srv.base)).send().await.unwrap().status(), 404);

    // projects: Maintainers set them; the project lists them
    assert_eq!(upload(&dev, &format!("/api/v1/projects/{proj}/avatar"), png(64, 64)).await, 403);
    assert_eq!(upload(&owner, &format!("/api/v1/projects/{proj}/avatar"), png(64, 64)).await, 204);
    let p = dev.ok_get(&format!("/api/v1/projects/{proj}")).await;
    let purl = p["avatar_url"].as_str().unwrap().to_string();
    assert_eq!(dev.get_bytes(&purl).await.0, 200);
    // a private project's avatar is not for strangers
    assert_eq!(anon.get(format!("{}{purl}", srv.base)).send().await.unwrap().status(), 404);
}

/// An account registered with someone else's address: when they take it back
/// with a reset link, the tokens and keys set up before go with the sessions.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_reclaimed_address_keeps_no_way_in(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    sqlx::query("UPDATE instance_settings SET signup_enabled = true").execute(&srv.state.db).await.unwrap();
    let anon = Client::new(&srv.base);
    let (code, body) = anon
        .post("/api/v1/auth/register", json!({ "username": "squatter", "email": "alice@example.com", "password": "squatter-pass-1" }))
        .await;
    assert_eq!(code, 200, "{body}");
    let mut squatter = Client::new(&srv.base);
    squatter.login("squatter", "squatter-pass-1").await;
    let pat =
        squatter.ok_post("/api/v1/user/tokens", json!({ "name": "keep", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let with_pat = Client::with_token(&srv.base, &pat);
    assert_eq!(with_pat.get("/api/v1/user").await.0, 200);

    // the address's owner resets the password: the token stops working
    assert_eq!(anon.post("/api/v1/auth/password/forgot", json!({ "login": "alice@example.com" })).await.0, 200);
    wait_jobs(&srv.state).await;
    let token = link_token(&last_mail_to(&srv, "alice@example.com").expect("a reset email").body, "reset-password");
    let (code, body) = anon.post("/api/v1/auth/password/reset", json!({ "token": token, "password": "alice-owns-this-1" })).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(with_pat.get("/api/v1/user").await.0, 401);
}

/// A changed password signs out the account's other sessions, not this one.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_new_password_ends_other_sessions(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let here = srv.user("mover", false).await;
    let mut elsewhere = Client::new(&srv.base);
    elsewhere.login("mover", &password("mover")).await;
    let (code, body) = here.put("/api/v1/user/password", json!({ "current": password("mover"), "new": "a-brand-new-password-1" })).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(here.get("/api/v1/user").await.0, 200);
    assert_eq!(elsewhere.get("/api/v1/user").await.0, 401);
}

/// An OIDC callback only finishes in the browser that started the sign-in.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn oidc_callbacks_need_the_starting_browser(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| {
        let p = serde_json::from_value(
            json!({ "display_name": "Corp", "issuer": "https://idp.invalid", "client_id": "x", "client_secret": "y" }),
        );
        c.oidc.insert("corp".into(), p.unwrap());
    })
    .await;
    let anon = Client::new(&srv.base);
    let (code, body) = anon.get("/api/v1/auth/oidc/corp/callback?code=attackers-code&state=attackers-state").await;
    assert_eq!(code, 400, "{body}");
    assert!(body.to_string().contains("not started in this browser"), "{body}");
    // the browser that started it gets past that check (to the state lookup)
    let r = anon
        .req(reqwest::Method::GET, "/api/v1/auth/oidc/corp/callback?code=c&state=its-state")
        .header("cookie", "forgust_oidc=its-state")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let text = r.text().await.unwrap();
    assert!(text.contains("unknown or expired login state"), "{text}");
}
