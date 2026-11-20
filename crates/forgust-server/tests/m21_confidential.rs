//! Confidential issues and internal notes: every read path, as each kind of
//! viewer. A confidential issue is for the project's Reporters and up, its
//! author and its assignees; an internal note for Reporters and up.

mod common;

use common::*;
use serde_json::{Value, json};

const REPO: &str = "open/site";

fn bodies(notes: &Value) -> Vec<String> {
    notes.as_array().unwrap().iter().map(|n| n["body"].as_str().unwrap().to_string()).collect()
}

/// What a viewer may see.
struct Who {
    name: &'static str,
    client: Client,
    signed_in: bool,
    /// The confidential issue #2 (Reporter+, its author, its assignee).
    secret: bool,
    /// Internal notes, and the confidential #3 nobody is assigned to.
    internal: bool,
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn confidential_issues_and_internal_notes(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.webhooks_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let root = srv.user("root", true).await;
    let rita = srv.user("rita", false).await;
    let gus = srv.user("gus", false).await;
    let alice = srv.user("alice", false).await;
    let adam = srv.user("adam", false).await;
    let nora = srv.user("nora", false).await;
    let anon = Client::new(&srv.base);

    // a public project: anyone sees it and its issues, merge requests included
    let g = owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await;
    let proj =
        owner.ok_post("/api/v1/projects", json!({ "name": "Site", "path": "site", "namespace_id": g["id"], "visibility": "public" })).await;
    let proj = proj["id"].as_i64().unwrap();
    let base = format!("/api/v1/projects/{proj}");
    let v4 = format!("/api/v4/projects/{proj}");
    for (name, level) in [("rita", "reporter"), ("gus", "guest"), ("alice", "guest"), ("adam", "guest")] {
        owner.ok_post(&format!("{base}/members"), json!({ "username": name, "access_level": level })).await;
    }
    let work = srv.work_dir("site");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "a.txt", "a\n", "a").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "topic"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("{base}/merge_requests"), json!({ "source_branch": "topic", "title": "Topic" })).await;

    // hooks: one for the everyday events, one that asked for the confidential ones
    let hooks = owner.ok_post(&format!("{base}/hooks"), json!({ "url": "http://127.0.0.1:9/a", "events": ["issue", "note"] })).await;
    let plain = hooks[0]["id"].as_i64().unwrap();
    let hooks = owner
        .ok_post(&format!("{base}/hooks"), json!({ "url": "http://127.0.0.1:9/b", "events": ["confidential_issue", "confidential_note"] }))
        .await;
    let secret_hook = hooks.as_array().unwrap().iter().find(|h| h["id"] != plain).unwrap()["id"].as_i64().unwrap();

    owner.ok_post(&format!("{base}/labels"), json!({ "name": "bug" })).await;
    let v1 = owner.ok_post(&format!("{base}/milestones"), json!({ "title": "v1" })).await;

    // #1 public; #2 confidential, opened by a Guest, assigned to another Guest
    owner.ok_post(&format!("{base}/issues"), json!({ "title": "Login page" })).await;
    owner.ok_post(&format!("{base}/issues/1/notes"), json!({ "body": "public note" })).await;
    let i2 = alice
        .ok_post(
            &format!("{base}/issues"),
            json!({ "title": "Token leak", "description": "found by @alice, see #1", "confidential": true }),
        )
        .await;
    assert_eq!((i2["iid"].as_i64(), i2["confidential"].as_bool()), (Some(2), Some(true)), "{i2}");
    let adam_id = srv.user_id("adam").await;
    // assignees need Reporter: Adam is one when assigned, then a Guest again
    owner.ok_post(&format!("{base}/members"), json!({ "username": "adam", "access_level": "reporter" })).await;
    owner.ok_put(&format!("{base}/issues/2"), json!({ "labels": ["bug"], "milestone_id": v1["id"], "assignee_ids": [adam_id] })).await;
    owner.delete(&format!("{base}/members/{adam_id}")).await;
    owner.ok_post(&format!("{base}/members"), json!({ "username": "adam", "access_level": "guest" })).await;

    // #3 public at first: Gus takes part and is mentioned, then it turns confidential
    owner.ok_post(&format!("{base}/issues"), json!({ "title": "Was public" })).await;
    gus.ok_post(&format!("{base}/issues/3/notes"), json!({ "body": "me too" })).await;
    owner.ok_post(&format!("{base}/issues/3/notes"), json!({ "body": "ping @gus" })).await;
    wait_jobs(&srv.state).await;
    let gus_todos = gus.ok_get("/api/v1/user/todos").await;
    assert!(gus_todos.as_array().unwrap().iter().any(|t| t["target_iid"] == 3), "{gus_todos}");
    let inbox = gus.ok_get("/api/v1/user/notifications").await;
    assert!(inbox["items"].as_array().unwrap().iter().any(|n| n["target_iid"] == 3), "{inbox}");
    assert_eq!(owner.ok_put(&format!("{base}/issues/3"), json!({ "confidential": true })).await["confidential"], true);

    // internal notes (Reporter); a public note mentioning people on #2
    let r = rita.ok_post(&format!("{base}/issues/2/notes"), json!({ "body": "internal on secret", "internal": true })).await;
    assert!(r["id"].is_i64());
    let internal_note =
        rita.ok_post(&format!("{base}/issues/1/notes"), json!({ "body": "internal on public @gus", "internal": true })).await;
    let internal_note = internal_note["id"].as_i64().unwrap();
    owner.ok_post(&format!("{base}/issues/2/notes"), json!({ "body": "ping @gus @rita @adam" })).await;
    owner.ok_post(&format!("{base}/issues"), json!({ "title": "Overview", "description": "See #2 and #1" })).await;
    owner.ok_post(&format!("{base}/issues/1/reactions"), json!({ "name": "thumbsup", "note_id": internal_note })).await;
    rita.ok_post(&format!("{base}/merge_requests/1/notes"), json!({ "body": "internal on mr", "internal": true })).await;
    owner.ok_post(&format!("{base}/merge_requests/1/notes"), json!({ "body": "public on mr" })).await;
    wait_jobs(&srv.state).await;

    let viewers = vec![
        Who { name: "owner", client: owner.clone(), signed_in: true, secret: true, internal: true },
        Who { name: "admin", client: root.clone(), signed_in: true, secret: true, internal: true },
        Who { name: "reporter", client: rita.clone(), signed_in: true, secret: true, internal: true },
        Who { name: "guest", client: gus.clone(), signed_in: true, secret: false, internal: false },
        Who { name: "author (guest)", client: alice.clone(), signed_in: true, secret: true, internal: false },
        Who { name: "assignee (guest)", client: adam.clone(), signed_in: true, secret: true, internal: false },
        Who { name: "non-member", client: nora.clone(), signed_in: true, secret: false, internal: false },
        Who { name: "anonymous", client: anon.clone(), signed_in: false, secret: false, internal: false },
    ];

    for w in &viewers {
        let (c, who) = (&w.client, w.name);
        let has = |list: &Value, iid: i64| list.as_array().unwrap().iter().any(|i| i["iid"] == iid);

        // v1 list and its counts
        let list = c.ok_get(&format!("{base}/issues?state=all")).await;
        assert_eq!(has(&list["issues"], 2), w.secret, "{who}: list");
        assert_eq!(has(&list["issues"], 3), w.internal, "{who}: list #3");
        assert_eq!(list["opened"], 2 + w.secret as i64 + w.internal as i64, "{who}: counts {list}");
        let only = c.ok_get(&format!("{base}/issues?state=all&confidential=true")).await;
        assert_eq!(only["issues"].as_array().unwrap().len(), w.secret as usize + w.internal as usize, "{who}: confidential filter");

        // the issue page and its notes
        let (s, i) = c.get(&format!("{base}/issues/2")).await;
        assert_eq!(s, if w.secret { 200 } else { 404 }, "{who}: show {i}");
        if w.secret {
            assert_eq!(i["confidential"], true);
            assert_eq!(i["can_comment_internal"], w.internal, "{who}");
            let n = bodies(&c.ok_get(&format!("{base}/issues/2/notes")).await);
            assert_eq!(n.contains(&"internal on secret".to_string()), w.internal, "{who}: {n:?}");
            assert!(n.contains(&"ping @gus @rita @adam".to_string()), "{who}: {n:?}");
        } else {
            assert_eq!(c.get(&format!("{base}/issues/2/notes")).await.0, 404, "{who}: notes");
            assert_eq!(c.get(&format!("{base}/issues/2/reactions")).await.0, 404, "{who}: reactions");
        }
        let n1 = c.ok_get(&format!("{base}/issues/1/notes")).await;
        let b1 = bodies(&n1);
        assert_eq!(b1.contains(&"internal on public @gus".to_string()), w.internal, "{who}: {b1:?}");
        assert_eq!(b1.contains(&"mentioned in issue #2".to_string()), w.internal, "{who}: the cross-reference is internal {b1:?}");
        assert!(b1.contains(&"mentioned in issue #4".to_string()) && b1.contains(&"public note".to_string()), "{who}: {b1:?}");
        let listed = c.ok_get(&format!("{base}/issues?state=all")).await;
        let row = listed["issues"].as_array().unwrap().iter().find(|i| i["iid"] == 1).unwrap().clone();
        assert_eq!(row["notes_count"], 1, "{who}: only the public comment is counted");

        // reactions on an internal note
        let re = c.ok_get(&format!("{base}/issues/1/reactions")).await;
        assert_eq!(re["notes"].get(internal_note.to_string()).is_some(), w.internal, "{who}: {re}");

        // labels and milestones count what the viewer sees
        let labels = c.ok_get(&format!("{base}/labels")).await;
        assert_eq!(labels[0]["open_issues"], w.secret as i64, "{who}: label count");
        let ms = c.ok_get(&format!("{base}/milestones?state=all")).await;
        assert_eq!(ms[0]["open_issues"], w.secret as i64, "{who}: milestone count");

        // a reference: the title only for those who see the issue
        let overview = c.ok_get(&format!("{base}/issues/4")).await;
        let html = overview["description_html"].as_str().unwrap();
        assert_eq!(html.contains("Token leak"), w.secret, "{who}: {html}");
        assert!(html.contains("Login page"), "{who}: {html}");

        // search
        let found = c.ok_get("/api/v1/search?q=Token").await;
        assert_eq!(has(&found["issues"], 2), w.secret, "{who}: search {found}");
        assert_eq!(found["counts"]["issues"], w.secret as i64, "{who}: search counts");

        // /api/v4
        let l4 = c.ok_get(&format!("{v4}/issues")).await;
        assert_eq!(has(&l4, 2), w.secret, "{who}: v4 list");
        let c4 = c.ok_get(&format!("{v4}/issues?confidential=true")).await;
        assert_eq!(has(&c4, 2), w.secret, "{who}: v4 confidential filter");
        assert!(c4.as_array().unwrap().iter().all(|i| i["confidential"] == true));
        let (s, i4) = c.get(&format!("{v4}/issues/2")).await;
        assert_eq!(s, if w.secret { 200 } else { 404 }, "{who}: v4 show");
        if w.secret {
            assert_eq!(i4["confidential"], true);
        }
        let n4 = c.ok_get(&format!("{v4}/issues/1/notes")).await;
        let internal4 = n4.as_array().unwrap().iter().filter(|n| n["internal"] == true).count();
        assert_eq!(internal4, if w.internal { 2 } else { 0 }, "{who}: v4 notes {n4}");

        // merge request notes, v1 and v4
        let mn = bodies(&c.ok_get(&format!("{base}/merge_requests/1/notes")).await);
        assert_eq!(mn.contains(&"internal on mr".to_string()), w.internal, "{who}: {mn:?}");
        assert!(mn.contains(&"public on mr".to_string()));
        let mn4 = bodies(&c.ok_get(&format!("{v4}/merge_requests/1/notes")).await);
        assert_eq!(mn4.contains(&"internal on mr".to_string()), w.internal, "{who}: v4 {mn4:?}");
        let d4 = c.ok_get(&format!("{v4}/merge_requests/1/discussions")).await;
        assert_eq!(d4.to_string().contains("internal on mr"), w.internal, "{who}: discussions");

        if !w.signed_in {
            continue;
        }
        // activity on people's profiles (for signed-in people)
        let act = c.ok_get("/api/v1/users/alice/contributions").await;
        let opened = act["activity"].as_array().unwrap().iter().any(|a| a["action"] == "opened_issue" && a["iid"] == 2);
        assert_eq!(opened, w.secret, "{who}: {act}");
        let act = c.ok_get("/api/v1/users/rita/contributions").await;
        let commented =
            act["activity"].as_array().unwrap().iter().filter(|a| a["action"].as_str().unwrap().starts_with("commented")).count();
        assert_eq!(commented, if w.internal { 3 } else { 0 }, "{who}: internal comments in rita's activity {act}");

        // MCP, with a read token (an administrator's token without `admin_mode` is not admin)
        let token =
            c.ok_post("/api/v1/user/tokens", json!({ "name": "mcp", "scopes": ["read_api"] })).await["token"].as_str().unwrap().to_string();
        let mcp = Mcp::new(&srv, Some(&token));
        let mcp_secret = w.secret && who != "admin";
        let mcp_internal = w.internal && who != "admin";
        let (err, text) = mcp.call("list_issues", json!({ "project_id": REPO, "state": "all" })).await;
        assert!(!err, "{who}: {text}");
        assert_eq!(text.contains("Token leak"), mcp_secret, "{who}: mcp list {text}");
        let (err, text) = mcp.call("get_issue", json!({ "project_id": REPO, "issue_iid": 2 })).await;
        assert_eq!(err, !mcp_secret, "{who}: mcp get {text}");
        let (_, text) = mcp.call("get_issue", json!({ "project_id": REPO, "issue_iid": 1, "include": ["notes"] })).await;
        assert_eq!(text.contains("internal on public"), mcp_internal, "{who}: mcp notes {text}");

        // to-dos and the inbox
        let todos = c.ok_get("/api/v1/user/todos").await;
        let names_2 = todos.as_array().unwrap().iter().any(|t| t["target_iid"] == 2 && t["target_type"] == "issue");
        let inbox = c.ok_get("/api/v1/user/notifications").await;
        for n in inbox["items"].as_array().unwrap() {
            if n["target_type"] == "issue" && n["target_iid"] == 2 {
                assert!(w.secret, "{who}: told about #2: {n}");
            }
            if n["target_type"] == "issue" && n["target_iid"] == 3 {
                assert!(w.internal, "{who}: told about #3: {n}");
            }
        }
        match who {
            "reporter" | "assignee (guest)" => assert!(names_2, "{who}: mentioned on #2 {todos}"),
            "guest" => {
                assert!(!names_2, "guest: mentioned on a confidential issue they may not see {todos}");
                assert!(!todos.as_array().unwrap().iter().any(|t| t["target_iid"] == 3), "the to-do of #3 went with its sight {todos}");
                assert!(!todos.as_array().unwrap().iter().any(|t| t["target_iid"] == 1), "no to-do from an internal note {todos}");
                assert_eq!(c.ok_get("/api/v1/user/todos").await.as_array().unwrap().len(), 0);
            }
            "author (guest)" => assert!(inbox["items"].as_array().unwrap().iter().any(|n| n["target_iid"] == 2), "{inbox}"),
            _ => {}
        }
    }

    // emails: none about #2 to Gus, the mention to Rita
    let sent = srv.state.mailer.sent();
    assert!(!sent.iter().any(|e| e.to.contains(&"gus@example.com".to_string()) && e.subject.contains("#2")), "{sent:?}");
    assert!(!sent.iter().any(|e| e.to.contains(&"gus@example.com".to_string()) && e.body.contains("internal on public")));
    assert!(sent.iter().any(|e| e.to.contains(&"rita@example.com".to_string()) && e.subject.contains("#2")), "{sent:?}");
    assert!(sent.iter().any(|e| e.to.contains(&"adam@example.com".to_string()) && e.subject.contains("#2")), "{sent:?}");

    // webhooks: the everyday hook never carries confidential content
    let deliveries = |hook: i64| {
        let db = srv.state.db.clone();
        async move {
            sqlx::query_as::<_, (String, String)>("SELECT event, payload::text FROM webhook_deliveries WHERE webhook_id = $1")
                .bind(hook)
                .fetch_all(&db)
                .await
                .unwrap()
        }
    };
    let a = deliveries(plain).await;
    assert!(a.iter().any(|(e, _)| e == "issue") && a.iter().any(|(e, _)| e == "note"), "{a:?}");
    for (e, body) in &a {
        assert!(e == "issue" || e == "note", "{e}");
        // (#3 was public when it was opened: that event is the everyday hook's)
        for secret in ["Token leak", "internal on", "ping @gus @rita", "\"confidential\": true"] {
            assert!(!body.contains(secret), "{e} carried {secret}: {body}");
        }
    }
    let b = deliveries(secret_hook).await;
    assert!(b.iter().any(|(e, body)| e == "confidential_issue" && body.contains("Token leak")), "{b:?}");
    assert!(b.iter().any(|(e, body)| e == "confidential_note" && body.contains("internal on mr")), "{b:?}");
    assert!(b.iter().any(|(e, body)| e == "confidential_note" && body.contains("internal on public")), "{b:?}");
    assert!(b.iter().all(|(e, _)| e.starts_with("confidential_")), "{b:?}");

    // who may write what
    assert_eq!(gus.post(&format!("{base}/issues/1/notes"), json!({ "body": "x", "internal": true })).await.0, 403);
    assert_eq!(alice.post(&format!("{base}/issues/2/notes"), json!({ "body": "x", "internal": true })).await.0, 403);
    assert_eq!(nora.post(&format!("{base}/merge_requests/1/notes"), json!({ "body": "x", "internal": true })).await.0, 403);
    assert_eq!(gus.put(&format!("{base}/issues/1"), json!({ "confidential": true })).await.0, 403, "a plain Guest");
    assert_eq!(nora.put(&format!("{base}/issues/1"), json!({ "confidential": true })).await.0, 403);
    assert_eq!(gus.put(&format!("{base}/issues/2"), json!({ "confidential": false })).await.0, 404, "unseen");
    assert_eq!(gus.put(&format!("{base}/issues/2"), json!({ "title": "x" })).await.0, 404);
    assert_eq!(gus.post(&format!("{base}/issues/2/notes"), json!({ "body": "x" })).await.0, 404);
    assert_eq!(gus.post(&format!("{base}/issues/2/reactions"), json!({ "name": "thumbsup" })).await.0, 404);
    let r = gus.post(&format!("{base}/issues/1/reactions"), json!({ "name": "thumbsup", "note_id": internal_note })).await;
    assert_eq!(r.0, 404, "an internal note is not known to a Guest");
    // the assignee (a Guest) may switch it, and their edit is recorded
    let open = adam.put(&format!("{base}/issues/2"), json!({ "confidential": false })).await;
    assert_eq!((open.0, open.1["confidential"].as_bool()), (200, Some(false)), "{}", open.1);
    assert_eq!(gus.get(&format!("{base}/issues/2")).await.0, 200, "public now");
    let n = bodies(&gus.ok_get(&format!("{base}/issues/2/notes")).await);
    assert!(n.contains(&"made the issue visible to everyone".to_string()), "{n:?}");
    assert!(!n.contains(&"internal on secret".to_string()), "internal notes stay internal {n:?}");
    assert_eq!(adam.put(&format!("{base}/issues/2"), json!({ "confidential": true })).await.0, 200);
    assert_eq!(gus.get(&format!("{base}/issues/2")).await.0, 404);

    // /api/v4 takes `confidential` on create and update, and `internal` on notes
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "v4", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let api = Client::with_token(&srv.base, &tok);
    let (s, made) = api.post(&format!("{v4}/issues"), json!({ "title": "Report", "confidential": true })).await;
    assert_eq!((s, made["confidential"].as_bool()), (201, Some(true)), "{made}");
    let iid = made["iid"].as_i64().unwrap();
    assert_eq!(gus.get(&format!("{v4}/issues/{iid}")).await.0, 404);
    let (s, note) = api.post(&format!("{v4}/issues/1/notes"), json!({ "body": "v4 internal", "internal": true })).await;
    assert_eq!((s, note["internal"].as_bool()), (201, Some(true)), "{note}");
    let (s, note) = api.post(&format!("{v4}/merge_requests/1/notes"), json!({ "body": "v4 mr internal", "confidential": true })).await;
    assert_eq!((s, note["internal"].as_bool()), (201, Some(true)), "GitLab 14's name {note}");
    assert!(!bodies(&gus.ok_get(&format!("{v4}/merge_requests/1/notes")).await).contains(&"v4 mr internal".to_string()));
    let upd = api.put(&format!("{v4}/issues/{iid}"), json!({ "confidential": false })).await;
    assert_eq!((upd.0, upd.1["confidential"].as_bool()), (200, Some(false)), "{}", upd.1);
    assert_eq!(gus.get(&format!("{v4}/issues/{iid}")).await.0, 200);

    // a public mention after an internal one still leaves a public "mentioned in" note
    let mentioned = "mentioned in merge request !1".to_string();
    rita.ok_post(&format!("{base}/merge_requests/1/notes"), json!({ "body": "see #1", "internal": true })).await;
    assert!(!bodies(&gus.ok_get(&format!("{base}/issues/1/notes")).await).contains(&mentioned));
    owner.ok_post(&format!("{base}/merge_requests/1/notes"), json!({ "body": "see #1" })).await;
    let n = bodies(&gus.ok_get(&format!("{base}/issues/1/notes")).await);
    assert!(n.contains(&mentioned), "{n:?}");

    // an issue closed by a merge tells the `issue` hook
    owner.ok_put(&format!("{base}/merge_requests/1"), json!({ "description": "Closes #1" })).await;
    let st = owner.ok_get(&format!("{base}/merge_requests/1/merge_status")).await;
    owner.ok_post(&format!("{base}/merge_requests/1/merge"), json!({ "method": "merge", "sha": st["head_sha"] })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(owner.ok_get(&format!("{base}/issues/1")).await["state"], "closed");
    let a = deliveries(plain).await;
    assert!(a.iter().any(|(e, body)| e == "issue" && body.contains("Login page") && body.contains("\"close\"")), "{a:?}");
}

/// What stays hidden once someone loses sight of internal notes, and what
/// threads and counts tell those who never had it.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn internal_notes_after_a_demotion(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let rita = srv.user("rita", false).await;
    let nora = srv.user("nora", false).await;
    let g = owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await;
    let proj =
        owner.ok_post("/api/v1/projects", json!({ "name": "Site", "path": "site", "namespace_id": g["id"], "visibility": "public" })).await;
    let base = format!("/api/v1/projects/{}", proj["id"].as_i64().unwrap());
    owner.ok_post(&format!("{base}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let work = srv.work_dir("site");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "a.txt", "a\n", "a").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "topic"]).await);
    wait_jobs(&srv.state).await;
    let mr = format!("{base}/merge_requests/1");
    owner.ok_post(&format!("{base}/merge_requests"), json!({ "source_branch": "topic", "title": "Topic" })).await;

    // a Reporter is mentioned internally on a public issue: a to-do with the text, an inbox entry
    owner.ok_post(&format!("{base}/issues"), json!({ "title": "Login page" })).await;
    owner.ok_post(&format!("{base}/issues/1/notes"), json!({ "body": "@rita the secret plan", "internal": true })).await;
    wait_jobs(&srv.state).await;
    let on_1 = |list: &Value| list.as_array().unwrap().iter().filter(|t| t["target_type"] == "issue" && t["target_iid"] == 1).count();
    let todos = rita.ok_get("/api/v1/user/todos").await;
    assert_eq!(on_1(&todos), 1, "{todos}");
    assert!(todos.to_string().contains("the secret plan"), "{todos}");
    assert_eq!(on_1(&rita.ok_get("/api/v1/user/notifications").await["items"]), 1);
    assert_eq!(rita.ok_get("/api/v1/user/work").await["todos_pending"], 1);

    // demoted to Guest: neither the text nor that it was written
    let rita_id = srv.user_id("rita").await;
    owner.delete(&format!("{base}/members/{rita_id}")).await;
    owner.ok_post(&format!("{base}/members"), json!({ "username": "rita", "access_level": "guest" })).await;
    let todos = rita.ok_get("/api/v1/user/todos").await;
    assert_eq!(on_1(&todos), 0, "{todos}");
    assert_eq!(rita.ok_get("/api/v1/user/work").await["todos_pending"], 0);
    let inbox = rita.ok_get("/api/v1/user/notifications").await;
    assert_eq!(on_1(&inbox["items"]), 0, "{inbox}");

    // a public mention after it is a to-do again, with its own text
    owner.ok_post(&format!("{base}/issues/1/notes"), json!({ "body": "@rita have a look" })).await;
    wait_jobs(&srv.state).await;
    let todos = rita.ok_get("/api/v1/user/todos").await;
    assert_eq!(on_1(&todos), 1, "{todos}");
    assert!(todos.to_string().contains("have a look") && !todos.to_string().contains("secret plan"), "{todos}");

    // unresolved threads: an internal one counts only for who may read it
    let line = json!({ "new_path": "a.txt", "new_line": 1 });
    let public = owner.ok_post(&format!("{mr}/notes"), json!({ "body": "public thread", "position": line })).await;
    owner.ok_post(&format!("{mr}/notes"), json!({ "body": "internal thread", "position": line, "internal": true })).await;
    assert_eq!(owner.ok_get(&format!("{mr}/merge_status")).await["unresolved_threads"], 2);
    assert_eq!(nora.ok_get(&format!("{mr}/merge_status")).await["unresolved_threads"], 1);

    // a reply is as internal as its thread: not internal in a public one
    let (s, e) = owner.post(&format!("{mr}/notes"), json!({ "body": "aside", "parent_id": public["id"], "internal": true })).await;
    assert_eq!(s, 400, "{e}");
    let reply = owner.ok_post(&format!("{mr}/notes"), json!({ "body": "fine", "parent_id": public["id"] })).await;
    assert_eq!(reply["internal"], false);
}
