//! M5: importing from GitLab, against a mock of its API. The mock's clone URL is a
//! project on the test server itself, which also keeps `refs/merge-requests/*/head`
//! like GitLab does.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use common::*;
use serde_json::{Value, json};

/// A fake `/api/v4`: JSON per path (`X-Total` = array length); strings are served raw.
async fn mock_gitlab(routes: HashMap<String, Value>) -> String {
    mock_gitlab_slow(routes, 0).await
}

/// The same, answering each request after `delay_ms` (a real server's latency).
async fn mock_gitlab_slow(routes: HashMap<String, Value>, delay_ms: u64) -> String {
    mock_gitlab_full(routes, delay_ms, false).await
}

/// `anonymous`: a public GitLab seen without a token (any token is refused, so a
/// token sent by mistake shows up as a failure).
async fn mock_gitlab_full(routes: HashMap<String, Value>, delay_ms: u64, anonymous: bool) -> String {
    mock_gitlab_counted(routes, delay_ms, anonymous).await.0
}

/// Requests in flight at the fake source: now, and the most at once.
#[derive(Default)]
struct InFlight {
    now: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

/// Paths requested so far.
type Hits = Arc<std::sync::Mutex<Vec<String>>>;

fn hits_on(hits: &Hits, pred: impl Fn(&str) -> bool) -> usize {
    hits.lock().unwrap().iter().filter(|p| pred(p)).count()
}

async fn mock_gitlab_counted(routes: HashMap<String, Value>, delay_ms: u64, anonymous: bool) -> (String, Hits, Arc<InFlight>) {
    use std::sync::atomic::Ordering::SeqCst;
    let routes = Arc::new(routes);
    let hits: Hits = Default::default();
    let seen = hits.clone();
    let in_flight: Arc<InFlight> = Default::default();
    let flight = in_flight.clone();
    let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let routes = routes.clone();
        let seen = seen.clone();
        let flight = flight.clone();
        async move {
            seen.lock().unwrap().push(req.uri().path().to_string());
            let n = flight.now.fetch_add(1, SeqCst) + 1;
            flight.peak.fetch_max(n, SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            flight.now.fetch_sub(1, SeqCst);
            let token = req.headers().get("private-token").and_then(|v| v.to_str().ok());
            // any token but "nope" (the import's token is a Forgust one, for git)
            let refused = if anonymous { token.is_some() } else { matches!(token, None | Some("nope")) };
            if refused {
                return (StatusCode::UNAUTHORIZED, r#"{"message":"401 Unauthorized"}"#).into_response();
            }
            match routes.get(req.uri().path()) {
                // a page gitlab.com shows only with a token
                Some(Value::String(raw)) if raw == "__401__" => {
                    (StatusCode::UNAUTHORIZED, r#"{"message":"401 Unauthorized"}"#).into_response()
                }
                // gitlab.com's hourly limit on anonymous user lookups
                Some(Value::String(raw)) if raw == "__429__" => (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "3600"), ("ratelimit-name", "throttle_unauthenticated_api")],
                    r#"{"message":"Retry later"}"#,
                )
                    .into_response(),
                Some(Value::String(raw)) => raw.clone().into_response(),
                Some(v) => {
                    let total = v.as_array().map(|a| a.len()).unwrap_or(1);
                    ([("x-total", total.to_string()), ("content-type", "application/json".into())], v.to_string()).into_response()
                }
                None => (StatusCode::NOT_FOUND, r#"{"message":"404 Not found"}"#).into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), hits, in_flight)
}

fn person(id: i64, username: &str, name: &str) -> Value {
    json!({ "id": id, "username": username, "name": name })
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn import_a_gitlab_project(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    srv.user("jane", false).await; // jane@example.com, matched by email

    // the "GitLab" repository: main, an open MR (topic) and a closed one whose branch is gone
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Acme", "path": "acme" })).await;
    let src = owner.ok_post("/api/v1/projects", json!({ "name": "Origin", "path": "origin", "namespace_id": org["id"] })).await;
    let src_id = src["id"].as_i64().unwrap();
    let work = srv.work_dir("origin");
    seed_repo(&work).await;
    let remote = srv.http_url("olivia", "acme/origin");
    must(git(&work, &["push", "-q", &remote, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "a.txt", "one\ntwo\n", "add a").await;
    must(git(&work, &["push", "-q", &remote, "topic"]).await);
    must(git(&work, &["checkout", "-q", "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "gone"]).await);
    commit_file(&work, "b.txt", "b\n", "add b").await;
    must(git(&work, &["push", "-q", &remote, "gone"]).await);
    wait_jobs(&srv.state).await;
    let mr_path = format!("/api/v1/projects/{src_id}/merge_requests");
    owner.ok_post(&mr_path, json!({ "source_branch": "topic", "title": "Topic" })).await;
    owner.ok_post(&mr_path, json!({ "source_branch": "gone", "title": "Gone" })).await;
    owner.ok_post(&format!("{mr_path}/2/close"), json!({})).await;
    must(git(&work, &["push", "-q", &remote, ":gone"]).await);
    wait_jobs(&srv.state).await;
    // like gitlab.com, the source no longer advertises the closed MR's ref; its
    // commits are still there, served by id
    srv.state.storage.delete_ref(src_id, "refs/merge-requests/2/head", None).await.unwrap();
    let v1 = owner.ok_get(&format!("{mr_path}/1")).await["versions"][0].clone();
    let v2 = owner.ok_get(&format!("{mr_path}/2")).await["versions"][0].clone();
    let token = owner.ok_post("/api/v1/user/tokens", json!({ "name": "import", "scopes": ["read_repository"] })).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    // the GitLab API
    let jane = person(7, "jane", "Jane Doe");
    let ghost = person(8, "ghosty", "Ghost Y");
    let project = json!({
        "id": 42, "path_with_namespace": "team/origin", "name": "Origin", "path": "origin", "description": "From GitLab",
        "visibility": "internal", "default_branch": "main", "http_url_to_repo": format!("{}/acme/origin.git", srv.base),
        "web_url": "https://gitlab.example/team/origin", "archived": false, "empty_repo": false, "issues_enabled": true,
        "statistics": { "repository_size": 12345, "lfs_objects_size": 0 },
        "shared_with_groups": [
            { "group_full_path": "team/qa", "group_access_level": 20 },
            { "group_full_path": "partners", "group_access_level": 30 }
        ]
    });
    let mr = |iid: i64, title: &str, state: &str, branch: &str, v: &Value| {
        json!({
            "id": 900 + iid, "iid": iid, "title": title, "description": "", "author": jane, "state": state, "draft": false,
            "source_branch": branch, "target_branch": "main", "labels": ["bug"], "created_at": format!("2025-01-0{iid}T10:00:00Z"),
            "updated_at": "2025-02-01T10:00:00Z", "merged_at": null, "closed_at": null, "sha": v["head_sha"],
            "source_project_id": 42, "target_project_id": 42,
            "diff_refs": { "base_sha": v["base_sha"], "start_sha": v["start_sha"], "head_sha": v["head_sha"] }
        })
    };
    let (mr1, mr2) = (mr(1, "Topic", "opened", "topic", &v1), mr(2, "Gone", "closed", "gone", &v2));
    let routes: HashMap<String, Value> = [
        ("/api/v4/user", json!({ "id": 1, "username": "importer", "name": "Importer" })),
        ("/api/v4/projects", json!([project])),
        ("/api/v4/projects/42", project.clone()),
        ("/api/v4/users/7", json!({ "id": 7, "username": "jane", "name": "Jane Doe", "public_email": "Jane@Example.com" })),
        ("/api/v4/projects/42/labels", json!([{ "name": "bug", "color": "#D73A4A", "description": "Broken" }, { "name": "ui", "color": "#abc" }])),
        (
            "/api/v4/projects/42/milestones",
            json!([{ "id": 1, "iid": 1, "title": "v1", "description": "First", "state": "active", "start_date": null, "due_date": "2024-07-01" }]),
        ),
        (
            "/api/v4/projects/42/issues",
            json!([
                { "id": 503, "iid": 3, "title": "It crashes", "description": "Steps", "author": jane, "state": "opened", "labels": ["bug"],
                  "created_at": "2024-05-01T08:00:00Z", "updated_at": "2024-05-02T08:00:00Z", "due_date": "2024-05-20",
                  "milestone": { "id": 1, "iid": 1, "title": "v1", "state": "active", "due_date": "2024-07-01" } },
                { "id": 505, "iid": 5, "title": "Old idea", "description": "", "author": ghost, "state": "closed", "labels": ["wontfix"],
                  "milestone": { "id": 77, "iid": 3, "group_id": 5, "title": "Q3 (group)", "state": "closed", "due_date": null },
                  "created_at": "2024-06-01T08:00:00Z", "updated_at": "2024-06-03T08:00:00Z", "closed_at": "2024-06-03T08:00:00Z" }
            ]),
        ),
        (
            "/api/v4/projects/42/issues/3/notes",
            json!([
                { "id": 1, "body": "Me too", "author": ghost, "created_at": "2024-05-01T09:00:00Z", "system": false },
                { "id": 2, "body": "added ~bug label", "author": jane, "created_at": "2024-05-01T09:30:00Z", "system": true }
            ]),
        ),
        ("/api/v4/projects/42/issues/5/notes", json!([])),
        ("/api/v4/projects/42/merge_requests", json!([mr1, mr2])),
        ("/api/v4/projects/42/merge_requests/1", mr1.clone()),
        ("/api/v4/projects/42/merge_requests/2", mr2.clone()),
        (
            "/api/v4/projects/42/merge_requests/1/discussions",
            json!([
                { "id": "d1", "notes": [
                    { "id": 11, "type": "DiffNote", "body": "Why two?", "author": ghost, "created_at": "2025-01-02T10:00:00Z", "resolved": true,
                      "position": { "base_sha": v1["base_sha"], "start_sha": v1["start_sha"], "head_sha": v1["head_sha"], "old_path": "a.txt",
                                    "new_path": "a.txt", "old_line": null, "new_line": 2, "position_type": "text" } },
                    { "id": 12, "body": "Because.", "author": jane, "created_at": "2025-01-02T11:00:00Z", "resolved": true }
                ] },
                { "id": "d2", "notes": [{ "id": 13, "body": "added 1 commit", "author": jane, "created_at": "2025-01-03T10:00:00Z", "system": true }] },
                { "id": "d3", "notes": [{ "id": 14, "body": "Looks good", "author": jane, "created_at": "2025-01-04T10:00:00Z" }] }
            ]),
        ),
        ("/api/v4/projects/42/merge_requests/2/discussions", json!([])),
        ("/api/v4/projects/42/variables", json!([
            { "key": "DEPLOY_TOKEN", "value": "s3cret-value", "variable_type": "env_var", "protected": true, "masked": true, "environment_scope": "*" },
            { "key": "REGION", "value": "eu", "environment_scope": "production" }
        ])),
        ("/api/v4/projects/42/protected_branches", json!([
            { "name": "main", "push_access_levels": [{ "access_level": 40 }], "merge_access_levels": [{ "access_level": 30 }], "allow_force_push": false }
        ])),
        ("/api/v4/groups/team/members", json!([{ "id": 7, "username": "jane", "name": "Jane Doe", "access_level": 20 }])),
        ("/api/v4/groups/team", json!({ "id": 5, "full_path": "team", "shared_with_groups": [] })),
        ("/api/v4/projects/42/members/all", json!([
            { "id": 7, "username": "jane", "name": "Jane Doe", "access_level": 30 },
            { "id": 8, "username": "ghosty", "name": "Ghost Y", "access_level": 40 }
        ])),
        ("/api/v4/projects/42/repository/files/%2Egitlab%2Dci%2Eyml/raw", json!("deploy:\n  script: [./deploy]\n  environment: production\n  secrets:\n    DB: {vault: db/pw}\n")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let gitlab = mock_gitlab(routes).await;

    // a target organization, with the group the project is shared with already there
    let target = owner.ok_post("/api/v1/groups", json!({ "name": "Imported", "path": "imported", "visibility": "internal" })).await;
    let apps = owner
        .ok_post("/api/v1/groups", json!({ "name": "apps", "path": "apps", "parent_id": target["id"], "visibility": "internal" }))
        .await;
    let qa = owner.ok_post("/api/v1/groups", json!({ "name": "QA", "path": "qa", "parent_id": apps["id"] })).await;

    // a wrong token is refused up front; the right one lists the project
    let bad = owner.post("/api/v1/imports/connect", json!({ "source": "gitlab", "url": gitlab, "token": "nope" })).await;
    assert_eq!(bad.0, 400, "{}", bad.1);
    let listed = owner.ok_post("/api/v1/imports/connect", json!({ "source": "gitlab", "url": gitlab, "token": token })).await;
    assert_eq!(listed["username"], "importer");
    assert_eq!(listed["projects"][0]["full_path"], "team/origin");

    // dry run
    let started = owner
        .ok_post(
            "/api/v1/imports",
            json!({ "source": "gitlab", "url": gitlab, "token": token, "target_namespace_id": target["id"],
                    "projects": [{ "source_id": "42", "source_path": "team/origin", "target_path": "apps/origin" }] }),
        )
        .await;
    let id = started["id"].as_i64().unwrap();
    assert_eq!(started["status"], "planning");
    wait_jobs(&srv.state).await;
    let planned = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    assert_eq!(planned["status"], "planned", "{planned}");
    let report = &planned["projects"][0]["report"];
    assert_eq!(report["issues"], 2);
    assert_eq!(report["merge_requests"], 2);
    assert!(report["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("secrets")), "{report}");
    assert!(planned["projects"][0]["project_id"].is_null(), "a dry run creates nothing");

    // import for real
    let run = owner.ok_post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    assert_eq!(run["status"], "running");
    wait_jobs(&srv.state).await;
    let done = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    assert_eq!(done["status"], "finished", "{done}");
    assert_eq!(done["has_token"], false, "the token is erased at the end");
    let row = &done["projects"][0];
    assert_eq!(row["status"], "finished", "{row}");
    let pid = row["project_id"].as_i64().unwrap();
    let p = owner.ok_get(&format!("/api/v1/projects/{pid}")).await;
    assert_eq!(p["full_path"], "imported/apps/origin");
    assert_eq!(p["visibility"], "internal");
    assert_eq!(p["description"], "From GitLab");

    // repository: branches (the deleted one stays gone), and no leftover import refs
    let branches = owner.ok_get(&format!("/api/v1/projects/{pid}/repository/branches")).await;
    let names: Vec<&str> = branches.as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"main") && names.contains(&"topic") && !names.contains(&"gone"), "{names:?}");
    assert!(srv.state.storage.refs(pid, "refs/import/").await.unwrap().is_empty());

    // issues keep their numbers, dates and authors
    let issues = owner.ok_get(&format!("/api/v1/projects/{pid}/issues?state=all")).await;
    let list = issues["issues"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    let i3 = list.iter().find(|i| i["iid"] == 3).unwrap();
    assert_eq!(i3["author_username"], "jane", "matched by email");
    assert_eq!(i3["created_at"], "2024-05-01T08:00:00Z");
    assert_eq!(i3["labels"][0]["name"], "bug");
    assert_eq!(i3["labels"][0]["color"], "#d73a4a");
    let i5 = list.iter().find(|i| i["iid"] == 5).unwrap();
    assert_eq!(i5["state"], "closed");
    assert!(i5["author_id"].is_null());
    assert_eq!(i5["imported_author"], "Ghost Y (@ghosty)");
    assert_eq!(i5["labels"][0]["name"], "wontfix", "labels used but not listed are created");
    // milestones: the project's, and a group one an issue uses, made the project's;
    // dates kept, the import's updated_at too
    assert_eq!(i3["milestone_title"], "v1");
    assert_eq!(i3["due_date"], "2024-05-20");
    assert_eq!(i3["updated_at"], "2024-05-02T08:00:00Z");
    assert_eq!(i5["milestone_title"], "Q3 (group)");
    let ms = owner.ok_get(&format!("/api/v1/projects/{pid}/milestones?state=all")).await;
    let first = ms.as_array().unwrap().iter().find(|m| m["title"] == "v1").unwrap();
    assert_eq!(first["due_date"], "2024-07-01");
    assert_eq!(first["open_issues"], 1);
    assert_eq!(ms.as_array().unwrap().iter().find(|m| m["title"] == "Q3 (group)").unwrap()["state"], "closed");
    let notes = owner.ok_get(&format!("/api/v1/projects/{pid}/issues/3/notes")).await;
    assert_eq!(notes.as_array().unwrap().len(), 1, "system notes are left behind");
    assert_eq!(notes[0]["imported_author"], "Ghost Y (@ghosty)");
    let next = owner.ok_post(&format!("/api/v1/projects/{pid}/issues"), json!({ "title": "New here" })).await;
    assert_eq!(next["iid"], 6, "new numbers follow the imported ones");

    // merge requests: versions on the original commits, threads with positions
    let mrs = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests?state=all")).await;
    assert_eq!(mrs.as_array().unwrap().len(), 2);
    let m1 = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/1")).await;
    assert_eq!(m1["state"], "opened");
    assert_eq!(m1["author_username"], "jane");
    assert_eq!(m1["versions"][0]["head_sha"], v1["head_sha"]);
    assert_eq!(m1["versions"][0]["base_sha"], v1["base_sha"]);
    assert_eq!(m1["labels"][0]["name"], "bug");
    let m2 = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/2")).await;
    assert_eq!(m2["state"], "closed");
    assert_eq!(m2["versions"][0]["head_sha"], v2["head_sha"], "the head comes from refs/merge-requests/2/head");
    assert_eq!(srv.state.storage.resolve(pid, "refs/merge-requests/2/head").await.unwrap().as_deref(), v2["head_sha"].as_str());
    let mnotes = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/1/notes")).await;
    let mnotes = mnotes.as_array().unwrap();
    assert_eq!(mnotes.len(), 3, "{mnotes:?}");
    let root = mnotes.iter().find(|n| n["body"] == "Why two?").unwrap();
    assert_eq!(root["new_line"], 2);
    assert_eq!(root["position"]["new_line"], 2, "the thread sits on its line");
    assert!(!root["resolved_at"].is_null());
    assert_eq!(root["imported_author"], "Ghost Y (@ghosty)");
    let reply = mnotes.iter().find(|n| n["body"] == "Because.").unwrap();
    assert_eq!(reply["parent_id"], root["id"]);
    // the imported open MR is a real one: a duplicate for its branches is refused
    assert_eq!(
        owner.post(&format!("/api/v1/projects/{pid}/merge_requests"), json!({ "source_branch": "topic", "title": "Again" })).await.0,
        409
    );

    // owner-only parts: variables, protected branches, members by email
    let vars = owner.ok_get(&format!("/api/v1/projects/{pid}/variables")).await;
    let vars = vars.as_array().unwrap();
    let deploy = vars.iter().find(|v| v["key"] == "DEPLOY_TOKEN").unwrap();
    assert_eq!(deploy["masked"], true);
    assert_eq!(deploy["protected"], true);
    let pbs = owner.ok_get(&format!("/api/v1/projects/{pid}/protected_branches")).await;
    let main = pbs.as_array().unwrap().iter().find(|b| b["name"] == "main").unwrap();
    assert_eq!(
        (main["push_access_level"].as_str(), main["merge_access_level"].as_str()),
        (Some("maintainer"), Some("developer")),
        "{main}"
    );
    let members = owner.ok_get(&format!("/api/v1/projects/{pid}/members")).await;
    let jane_m = members.as_array().unwrap().iter().find(|m| m["username"] == "jane").unwrap();
    assert_eq!(jane_m["access_level"], "developer");
    // group members land on the group the source group became; sharing follows
    let gm = owner.ok_get(&format!("/api/v1/groups/{}/members", apps["id"])).await;
    let jane_g = gm.as_array().unwrap().iter().find(|m| m["username"] == "jane").expect("jane is a member of imported/apps");
    assert_eq!(jane_g["access_level"], "reporter");
    let shares = owner.ok_get(&format!("/api/v1/projects/{pid}/shares")).await;
    let qa_share = shares.as_array().unwrap().iter().find(|s| s["group_id"] == qa["id"]).expect("shared with imported/apps/qa");
    assert_eq!(qa_share["max_access_level"], "reporter");
    let warnings = done["projects"][0]["report"]["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("partners")), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("@ghosty")), "unmatched members are reported: {warnings:?}");
    assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("environment scope")), "{warnings:?}");
    assert!(done["users"].as_array().unwrap().iter().any(|u| u["username"] == "jane" && u["local_username"] == "jane"));

    // a second run (as after a crash) imports nothing twice
    sqlx::query("UPDATE imports SET status = 'running', token_enc = $2 WHERE id = $1")
        .bind(id)
        .bind(srv.state.secrets.encrypt(token.as_bytes()).unwrap())
        .execute(&srv.state.db)
        .await
        .unwrap();
    let row_id = row["id"].as_i64().unwrap();
    forgust_db::imports::set_project_status(&srv.state.db, row_id, "pending", None).await.unwrap();
    let mut conn = srv.state.db.acquire().await.unwrap();
    srv.state
        .jobs
        .enqueue(&mut conn, forgust_server::import::ImportJob { import_project_id: row_id, plan: false }.request(id))
        .await
        .unwrap();
    drop(conn);
    wait_jobs(&srv.state).await;
    let again = owner.ok_get(&format!("/api/v1/projects/{pid}/issues?state=all")).await;
    assert_eq!(again["issues"].as_array().unwrap().len(), 3, "2 imported + the new one");
    let mrs = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests?state=all")).await;
    assert_eq!(mrs.as_array().unwrap().len(), 2, "nothing imported twice");

    // a run again never overwrites what the project has (no forced update past
    // push rules and protected branches): main moved here stays; a branch the
    // source has and the project lost comes back
    let mine = owner
        .ok_post(
            &format!("/api/v1/projects/{pid}/repository/commits"),
            json!({ "branch": "main", "commit_message": "local", "actions": [{ "action": "create", "file_path": "local.txt", "content": "x\n" }] }),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(owner.delete(&format!("/api/v1/projects/{pid}/repository/branches/topic")).await.0 < 300);
    // through the retry endpoint (a failed row, the token given again)
    sqlx::query("UPDATE import_projects SET status = 'failed' WHERE id = $1").bind(row_id).execute(&srv.state.db).await.unwrap();
    owner.ok_post(&format!("/api/v1/imports/{id}/retry"), json!({ "token": token })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(srv.state.storage.resolve(pid, "refs/heads/main").await.unwrap().as_deref(), Some(mine.as_str()), "main is not overwritten");
    let after = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    assert!(srv.state.storage.resolve(pid, "refs/heads/topic").await.unwrap().is_some(), "a missing branch is created: {after}");
    assert!(srv.state.storage.refs(pid, "refs/import/").await.unwrap().is_empty(), "nothing left aside");

    // an importer who left the group may not run it again
    sqlx::query("UPDATE import_projects SET status = 'failed' WHERE id = $1").bind(row_id).execute(&srv.state.db).await.unwrap();
    let ivy = srv.user("ivy", false).await;
    owner.ok_post(&format!("/api/v1/groups/{}/members", target["id"]), json!({ "username": "ivy", "access_level": "owner" })).await;
    let olivia = owner.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    assert!(ivy.delete(&format!("/api/v1/groups/{}/members/{olivia}", target["id"])).await.0 < 300);
    let (s, body) = owner.post(&format!("/api/v1/imports/{id}/retry"), json!({ "token": token })).await;
    assert!(s == 403 || s == 404, "{s} {body}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn import_refuses_internal_addresses_by_default(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let r = owner.post("/api/v1/imports/connect", json!({ "source": "gitlab", "url": "http://127.0.0.1:9/", "token": "x" })).await;
    assert_eq!(r.0, 400);
    assert!(r.1["message"].as_str().unwrap_or_default().contains("internal address"), "{}", r.1);
    let r = owner.post("/api/v1/imports/connect", json!({ "source": "gitlab", "url": "file:///etc/passwd", "token": "x" })).await;
    assert_eq!(r.0, 400);
}

/// A fake GitHub Enterprise `/api/v3`: JSON per path, `?page=N` pages chained
/// with `Link` headers; Bearer token required.
/// `anonymous`: a public repository seen without a token (a token is refused).
async fn mock_github(routes: HashMap<String, Value>, anonymous: bool) -> (String, Hits) {
    let routes = Arc::new(routes);
    let hits: Hits = Default::default();
    let seen = hits.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let routes = routes.clone();
        let seen = seen.clone();
        async move {
            seen.lock().unwrap().push(req.uri().path().to_string());
            let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok());
            let ok = if anonymous { auth.is_none() } else { auth.is_some_and(|v| v.starts_with("Bearer ") && v != "Bearer nope") };
            if !ok {
                return (StatusCode::UNAUTHORIZED, r#"{"message":"Bad credentials"}"#).into_response();
            }
            let path = req.uri().path().to_string();
            let page: usize = req
                .uri()
                .query()
                .unwrap_or("")
                .split('&')
                .find_map(|kv| kv.strip_prefix("page="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let key = if page > 1 { format!("{path}?page={page}") } else { path.clone() };
            match routes.get(&key) {
                Some(v) => {
                    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
                    if routes.contains_key(&format!("{path}?page={}", page + 1)) {
                        headers.push(("link".into(), format!("<http://{addr}{path}?page={}&per_page=100>; rel=\"next\"", page + 1)));
                    }
                    let mut r = v.to_string().into_response();
                    for (k, val) in headers {
                        r.headers_mut().insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), val.parse().unwrap());
                    }
                    r
                }
                None => (StatusCode::NOT_FOUND, r#"{"message":"Not Found"}"#).into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), hits)
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn import_a_github_repository(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    srv.user("jane", false).await;

    // the "GitHub" repository, with a pull request head kept under refs/pull/
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Acme", "path": "acme" })).await;
    let origin = owner.ok_post("/api/v1/projects", json!({ "name": "Origin", "path": "origin", "namespace_id": org["id"] })).await;
    let work = srv.work_dir("gh-origin");
    seed_repo(&work).await;
    let remote = srv.http_url("olivia", "acme/origin");
    must(git(&work, &["push", "-q", &remote, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "a.txt", "one\ntwo\n", "add a").await;
    must(git(&work, &["push", "-q", &remote, "topic"]).await);
    let topic = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    must(git(&work, &["checkout", "-q", "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "gone"]).await);
    commit_file(&work, "b.txt", "b\n", "add b").await;
    let gone = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    // Forgust takes branches and tags only: the objects go up under a tag, the ref is set directly
    must(git(&work, &["push", "-q", &remote, &format!("{gone}:refs/tags/pull-4")]).await);
    wait_jobs(&srv.state).await;
    srv.state.storage.update_ref(origin["id"].as_i64().unwrap(), "refs/pull/4/head", &gone, None).await.unwrap();
    srv.state.storage.delete_ref(origin["id"].as_i64().unwrap(), "refs/tags/pull-4", None).await.unwrap();
    wait_jobs(&srv.state).await;
    let token = owner.ok_post("/api/v1/user/tokens", json!({ "name": "import", "scopes": ["read_repository"] })).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let octo = json!({ "id": 9, "login": "octocat" });
    let jane = json!({ "id": 7, "login": "jane" });
    let repo = json!({
        "full_name": "octo/app", "name": "app", "description": "An app", "private": true, "visibility": "private", "default_branch": "main",
        "clone_url": format!("{}/acme/origin.git", srv.base), "html_url": "https://github.example/octo/app", "archived": false,
        "fork": false, "size": 12, "has_issues": true
    });
    let routes: HashMap<String, Value> = [
        ("/api/v3/user", json!({ "id": 1, "login": "importer" })),
        ("/api/v3/user/repos", json!([repo])),
        ("/api/v3/repos/octo/app", repo.clone()),
        ("/api/v3/users/jane", json!({ "id": 7, "login": "jane", "name": "Jane Doe", "email": "jane@example.com" })),
        ("/api/v3/users/octocat", json!({ "id": 9, "login": "octocat", "name": "The Octocat" })),
        ("/api/v3/repos/octo/app/labels", json!([{ "name": "bug", "color": "d73a4a", "description": "Broken" }])),
        (
            "/api/v3/repos/octo/app/issues",
            json!([
                { "id": 101, "number": 1, "title": "Crash", "body": "It crashes", "user": jane, "state": "open", "labels": [{ "name": "bug" }],
                  "milestone": { "number": 1, "title": "Launch", "state": "open", "due_on": "2024-03-01T08:00:00Z" },
                  "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-02T00:00:00Z" },
                { "id": 102, "number": 2, "title": "A pull request", "user": jane, "state": "open", "pull_request": {},
                  "created_at": "2024-01-03T00:00:00Z", "updated_at": "2024-01-03T00:00:00Z" }
            ]),
        ),
        (
            "/api/v3/repos/octo/app/issues?page=2",
            json!([{ "id": 103, "number": 3, "title": "Old", "body": null, "user": octo, "state": "closed", "labels": [],
                     "created_at": "2024-01-04T00:00:00Z", "updated_at": "2024-01-05T00:00:00Z", "closed_at": "2024-01-05T00:00:00Z" }]),
        ),
        // every comment of the repository at once, issues and pull requests mixed
        (
            "/api/v3/repos/octo/app/issues/comments",
            json!([
                { "id": 201, "issue_url": "https://api.github.example/repos/octo/app/issues/1", "body": "Same here", "user": octo,
                  "created_at": "2024-01-01T05:00:00Z" },
                { "id": 401, "issue_url": "https://api.github.example/repos/octo/app/issues/2", "body": "Thanks!", "user": octo,
                  "created_at": "2024-01-03T02:00:00Z" }
            ]),
        ),
        (
            "/api/v3/repos/octo/app/pulls",
            json!([
                { "id": 302, "number": 2, "title": "Topic", "body": "Adds a", "user": jane, "state": "open", "draft": false, "labels": [{ "name": "bug" }],
                  "created_at": "2024-01-03T00:00:00Z", "updated_at": "2024-01-03T01:00:00Z",
                  "head": { "ref": "topic", "sha": topic, "repo": { "full_name": "octo/app" } }, "base": { "ref": "main", "sha": "x" } },
                { "id": 304, "number": 4, "title": "From a fork", "body": "", "user": octo, "state": "closed", "labels": [],
                  "created_at": "2024-01-06T00:00:00Z", "updated_at": "2024-01-07T00:00:00Z", "merged_at": "2024-01-07T00:00:00Z",
                  "merge_commit_sha": gone, "head": { "ref": "gone", "sha": gone, "repo": null }, "base": { "ref": "main", "sha": "x" } }
            ]),
        ),
        ("/api/v3/repos/octo/app/pulls/2/reviews", json!([{ "id": 501, "body": "", "state": "APPROVED", "user": octo, "submitted_at": "2024-01-03T03:00:00Z" }])),
        (
            "/api/v3/repos/octo/app/pulls/comments",
            json!([
                { "id": 601, "pull_request_url": "https://api.github.example/repos/octo/app/pulls/2", "body": "Why two lines?", "user": octo, "created_at": "2024-01-03T01:30:00Z", "path": "a.txt", "line": 2, "side": "RIGHT",
                  "commit_id": topic, "original_commit_id": topic, "original_line": 2 },
                { "id": 602, "pull_request_url": "https://api.github.example/repos/octo/app/pulls/2", "body": "Because.", "user": jane, "created_at": "2024-01-03T01:40:00Z", "path": "a.txt", "line": 2, "side": "RIGHT",
                  "commit_id": topic, "in_reply_to_id": 601 }
            ]),
        ),
        ("/api/v3/repos/octo/app/pulls/4/reviews", json!([])),
        ("/api/v3/repos/octo/app/branches", json!([{ "name": "main" }])),
        ("/api/v3/repos/octo/app/collaborators", json!([{ "id": 7, "login": "jane", "permissions": { "push": true } }])),
        ("/api/v3/search/issues", json!({ "total_count": 2 })),
        ("/api/v3/repos/octo/app/contents/.github/workflows", json!([{ "name": "ci.yml" }])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let (github, hits) = mock_github(routes, false).await;
    let target = owner.ok_post("/api/v1/groups", json!({ "name": "Imported", "path": "imported" })).await;

    let listed = owner.ok_post("/api/v1/imports/connect", json!({ "source": "github", "url": github, "token": token })).await;
    assert_eq!(listed["projects"][0]["full_path"], "octo/app");
    let started = owner
        .ok_post(
            "/api/v1/imports",
            json!({ "source": "github", "url": github, "token": token, "target_namespace_id": target["id"],
                    "projects": [{ "source_id": "octo/app", "source_path": "octo/app", "target_path": "app" }] }),
        )
        .await;
    let id = started["id"].as_i64().unwrap();
    wait_jobs(&srv.state).await;
    let planned = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    assert_eq!(planned["status"], "planned", "{planned}");
    let warnings = planned["projects"][0]["report"]["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str().unwrap().contains("Actions workflow")), "{warnings:?}");
    owner.ok_post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    wait_jobs(&srv.state).await;
    let done = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    assert_eq!(done["projects"][0]["status"], "finished", "{done}");
    let pid = done["projects"][0]["project_id"].as_i64().unwrap();

    // issues: pull requests left out of the list, both pages read
    let issues = owner.ok_get(&format!("/api/v1/projects/{pid}/issues?state=all")).await;
    let iids: Vec<i64> = issues["issues"].as_array().unwrap().iter().map(|i| i["iid"].as_i64().unwrap()).collect();
    assert_eq!(iids.len(), 2, "{iids:?}");
    assert!(iids.contains(&1) && iids.contains(&3));
    let i1 = owner.ok_get(&format!("/api/v1/projects/{pid}/issues/1")).await;
    assert_eq!(i1["author_username"], "jane");
    assert_eq!(i1["labels"][0]["color"], "#d73a4a");
    assert_eq!(i1["milestone_title"], "Launch");

    // pull requests: the open one on its branch, the merged fork one from refs/pull/4/head
    let p2 = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/2")).await;
    assert_eq!(p2["state"], "opened");
    assert_eq!(p2["versions"][0]["head_sha"], topic);
    let p4 = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/4")).await;
    assert_eq!(p4["state"], "merged");
    assert_eq!(p4["source_branch"], "pull/4");
    assert_eq!(p4["versions"][0]["head_sha"], gone);
    let notes = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/2/notes")).await;
    let notes = notes.as_array().unwrap();
    let bodies: Vec<&str> = notes.iter().map(|n| n["body"].as_str().unwrap()).collect();
    assert!(bodies.contains(&"Thanks!") && bodies.contains(&"**Approved**"), "{bodies:?}");
    let root = notes.iter().find(|n| n["body"] == "Why two lines?").unwrap();
    assert_eq!(root["new_line"], 2);
    assert_eq!(root["position"]["new_line"], 2);
    let reply = notes.iter().find(|n| n["body"] == "Because.").unwrap();
    assert_eq!(reply["parent_id"], root["id"]);

    // collaborators become members by email; protected branches come along
    let members = owner.ok_get(&format!("/api/v1/projects/{pid}/members")).await;
    assert!(members.as_array().unwrap().iter().any(|m| m["username"] == "jane" && m["access_level"] == "developer"));
    let report = &done["projects"][0]["report"]["warnings"];
    assert!(report.as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("fork")), "{report}");
    // comments come from the two repository-wide lists, not a request per item
    let per_item = hits_on(&hits, |p| {
        let rest = p.strip_prefix("/api/v3/repos/octo/app/").unwrap_or("");
        (rest.starts_with("issues/") || rest.starts_with("pulls/")) && rest.ends_with("/comments") && rest.split('/').count() == 3
    });
    assert_eq!(per_item, 0);
    assert_eq!(hits_on(&hits, |p| p == "/api/v3/repos/octo/app/issues/comments"), 1, "read once for issues and pull requests");
}

/// A public GitHub repository without a token: GitHub allows 60 requests an hour,
/// so nothing is asked per pull request or per person, and the dry run says how
/// long the quota will make it.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn import_a_public_github_repository_without_a_token(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await;
    owner.ok_post("/api/v1/projects", json!({ "name": "Pub", "path": "pub", "namespace_id": org["id"], "visibility": "public" })).await;
    let work = srv.work_dir("gh-pub");
    seed_repo(&work).await;
    let remote = srv.http_url("olivia", "open/pub");
    must(git(&work, &["push", "-q", &remote, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "a.txt", "one\n", "add a").await;
    must(git(&work, &["push", "-q", &remote, "topic"]).await);
    let topic = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    wait_jobs(&srv.state).await;

    let someone = json!({ "id": 3, "login": "someone" });
    let repo = json!({
        "full_name": "someone/pub", "name": "pub", "description": "", "private": false, "visibility": "public", "default_branch": "main",
        "clone_url": format!("{}/open/pub.git", srv.base), "html_url": "https://github.example/someone/pub", "archived": false,
        "fork": false, "size": 1, "has_issues": true
    });
    let routes: HashMap<String, Value> = [
        ("/api/v3/repos/someone/pub", repo.clone()),
        ("/api/v3/rate_limit", json!({ "resources": { "core": { "limit": 60, "remaining": 2, "reset": 0 } } })),
        ("/api/v3/search/issues", json!({ "total_count": 1 })),
        ("/api/v3/repos/someone/pub/labels", json!([])),
        ("/api/v3/repos/someone/pub/branches", json!([])),
        (
            "/api/v3/repos/someone/pub/issues",
            json!([{ "id": 11, "number": 1, "title": "Public issue", "user": someone, "state": "open", "labels": [],
                     "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z" }]),
        ),
        (
            "/api/v3/repos/someone/pub/issues/comments",
            json!([
                { "id": 21, "issue_url": "https://api.github.example/repos/someone/pub/issues/1", "body": "Me too", "user": someone,
                  "created_at": "2024-01-01T01:00:00Z" },
                { "id": 22, "issue_url": "https://api.github.example/repos/someone/pub/issues/2", "body": "Nice", "user": someone,
                  "created_at": "2024-01-02T01:00:00Z" }
            ]),
        ),
        (
            "/api/v3/repos/someone/pub/pulls",
            json!([{ "id": 12, "number": 2, "title": "Public PR", "body": "", "user": someone, "state": "open", "draft": false, "labels": [],
                     "created_at": "2024-01-02T00:00:00Z", "updated_at": "2024-01-02T00:00:00Z",
                     "head": { "ref": "topic", "sha": topic, "repo": { "full_name": "someone/pub" } }, "base": { "ref": "main", "sha": "x" } }]),
        ),
        ("/api/v3/repos/someone/pub/pulls/comments", json!([])),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let (github, hits) = mock_github(routes, true).await;
    let found = owner
        .ok_post("/api/v1/imports/lookup", json!({ "source": "github", "url": github, "path": format!("{github}/someone/pub") }))
        .await;
    assert_eq!(found["id"], "someone/pub");

    let target = owner.ok_post("/api/v1/groups", json!({ "name": "Imported", "path": "imported" })).await;
    let started = owner
        .ok_post(
            "/api/v1/imports",
            json!({ "source": "github", "url": github, "target_namespace_id": target["id"],
                    "projects": [{ "source_id": "someone/pub", "source_path": "someone/pub", "target_path": "pub" }] }),
        )
        .await;
    let id = started["id"].as_i64().unwrap();
    wait_jobs(&srv.state).await;
    let planned = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    let warnings = planned["projects"][0]["report"]["warnings"].as_array().unwrap().clone();
    let says = |start: &str| warnings.iter().any(|w| w.as_str().unwrap().starts_with(start));
    assert!(says("Without a token GitHub allows 60 requests an hour (2 left now)"), "{warnings:?}");
    assert!(says("Without a token, review summaries"), "{warnings:?}");

    owner.ok_post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    wait_jobs(&srv.state).await;
    let done = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    assert_eq!(done["projects"][0]["status"], "finished", "{done}");
    let pid = done["projects"][0]["project_id"].as_i64().unwrap();
    let issue = owner.ok_get(&format!("/api/v1/projects/{pid}/issues/1/notes")).await;
    assert_eq!(issue[0]["body"], "Me too", "{issue}");
    let pr = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/2/notes")).await;
    assert!(pr.as_array().unwrap().iter().any(|n| n["body"] == "Nice"), "{pr}");
    assert_eq!(hits_on(&hits, |p| p.contains("/users/")), 0, "no email lookups");
    assert_eq!(hits_on(&hits, |p| p.ends_with("/reviews")), 0, "no review summaries");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn import_copies_lfs_objects(pool: sqlx::PgPool) {
    if !has("git-lfs") {
        eprintln!("skipping: git-lfs not on PATH");
        return;
    }
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Acme", "path": "acme" })).await;
    owner.ok_post("/api/v1/projects", json!({ "name": "Assets", "path": "assets", "namespace_id": org["id"] })).await;
    let work = srv.work_dir("lfs-origin");
    seed_repo(&work).await;
    must(git(&work, &["lfs", "install", "--local"]).await);
    must(git(&work, &["lfs", "track", "*.bin"]).await);
    let data: Vec<u8> = (0..150_000u32).map(|i| (i * 13 % 251) as u8).collect();
    std::fs::write(work.join("model.bin"), &data).unwrap();
    must(git(&work, &["add", ".gitattributes", "model.bin"]).await);
    must(git(&work, &["commit", "-q", "-m", "add model"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/assets"), "main"]).await);
    wait_jobs(&srv.state).await;
    let token = owner.ok_post("/api/v1/user/tokens", json!({ "name": "import", "scopes": ["read_repository"] })).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    // a GitLab that only knows the project: the lists it refuses become warnings
    let project = json!({
        "id": 5, "path_with_namespace": "team/assets", "name": "Assets", "path": "assets", "visibility": "private", "default_branch": "main",
        "http_url_to_repo": format!("{}/acme/assets.git", srv.base), "web_url": "https://gitlab.example/team/assets", "issues_enabled": true
    });
    let routes: HashMap<String, Value> =
        [("/api/v4/user", json!({ "id": 1, "username": "importer", "name": "Importer" })), ("/api/v4/projects/5", project)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
    let gitlab = mock_gitlab(routes).await;
    let target = owner.ok_post("/api/v1/groups", json!({ "name": "Imported", "path": "imported" })).await;
    let started = owner
        .ok_post(
            "/api/v1/imports",
            json!({ "source": "gitlab", "url": gitlab, "token": token, "target_namespace_id": target["id"],
                    "projects": [{ "source_id": "5", "source_path": "team/assets", "target_path": "assets" }] }),
        )
        .await;
    let id = started["id"].as_i64().unwrap();
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    wait_jobs(&srv.state).await;
    let done = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    let row = &done["projects"][0];
    assert_eq!(row["status"], "finished", "{done}");
    assert_eq!(row["progress"]["lfs"]["done"], 1, "{row}");
    let warnings = row["report"]["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str().unwrap().starts_with("Labels could not be read")), "{warnings:?}");
    let pid = row["project_id"].as_i64().unwrap();

    // the object is here, served by raw
    let blob = owner.ok_get(&format!("/api/v1/projects/{pid}/repository/blob?ref=main&path=model.bin")).await;
    assert_eq!(blob["lfs"]["stored"], true, "{blob}");
    let (s, raw) = owner.get_bytes(&format!("/api/v1/projects/{pid}/repository/raw?ref=main&path=model.bin")).await;
    assert_eq!(s, 200);
    assert_eq!(raw, data);
}

/// 100 merge requests from a source answering in 50 ms: the import overlaps the
/// requests instead of making them one after the other.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn import_many_merge_requests_quickly(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Acme", "path": "acme" })).await;
    owner.ok_post("/api/v1/projects", json!({ "name": "Origin", "path": "origin", "namespace_id": org["id"] })).await;
    let work = srv.work_dir("many");
    seed_repo(&work).await;
    let remote = srv.http_url("olivia", "acme/origin");
    must(git(&work, &["push", "-q", &remote, "main"]).await);
    let base = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "a.txt", "a\n", "add a").await;
    must(git(&work, &["push", "-q", &remote, "topic"]).await);
    let head = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    // merged: main now has the head, so git alone gives an empty diff
    must(git(&work, &["checkout", "-q", "main"]).await);
    must(git(&work, &["merge", "-q", "--no-ff", "-m", "Merge topic", "topic"]).await);
    must(git(&work, &["push", "-q", &remote, "main"]).await);
    let merge = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    wait_jobs(&srv.state).await;
    let token = owner.ok_post("/api/v1/user/tokens", json!({ "name": "import", "scopes": ["read_repository"] })).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    const N: i64 = 100;
    let jane = person(7, "jane", "Jane Doe");
    // the last one was fast-forwarded (no merge commit): only the source knows its base
    let mr = |iid: i64| {
        json!({ "id": 9000 + iid, "iid": iid, "title": format!("Change {iid}"), "description": "", "author": jane, "state": "merged",
                "source_branch": "topic", "target_branch": "main", "labels": [], "created_at": "2025-01-01T10:00:00Z",
                "updated_at": "2025-01-02T10:00:00Z", "merged_at": "2025-01-02T10:00:00Z", "sha": head, "source_project_id": 1, "target_project_id": 1,
                "merge_commit_sha": if iid == N { Value::Null } else { json!(merge) },
                "diff_refs": { "base_sha": base, "start_sha": base, "head_sha": head } })
    };
    // like GitLab, the list has no diff_refs; the single MR does
    let listed = |iid: i64| {
        let mut m = mr(iid);
        m.as_object_mut().unwrap().remove("diff_refs");
        m
    };
    let mut routes: HashMap<String, Value> = [
        ("/api/v4/user".to_string(), json!({ "id": 1, "username": "importer", "name": "Importer" })),
        ("/api/v4/users/7".to_string(), json!({ "id": 7, "username": "jane", "name": "Jane Doe" })),
        (
            "/api/v4/projects/1".to_string(),
            json!({ "id": 1, "path_with_namespace": "team/origin", "name": "Origin", "path": "origin", "visibility": "private",
                    "default_branch": "main", "http_url_to_repo": format!("{}/acme/origin.git", srv.base), "web_url": "https://gitlab.example/team/origin" }),
        ),
        ("/api/v4/projects/1/labels".to_string(), json!([])),
        ("/api/v4/projects/1/issues".to_string(), json!([])),
        ("/api/v4/projects/1/merge_requests".to_string(), Value::Array((1..=N).map(listed).collect())),
    ]
    .into_iter()
    .collect();
    for iid in 1..=N {
        routes.insert(format!("/api/v4/projects/1/merge_requests/{iid}"), mr(iid));
        routes.insert(
            format!("/api/v4/projects/1/merge_requests/{iid}/discussions"),
            json!([{ "id": "d", "notes": [{ "id": iid, "body": "LGTM", "author": jane, "created_at": "2025-01-01T11:00:00Z" }] }]),
        );
    }
    let (gitlab, hits, in_flight) = mock_gitlab_counted(routes, 50, false).await;
    let target = owner.ok_post("/api/v1/groups", json!({ "name": "Imported", "path": "imported" })).await;
    let started = owner
        .ok_post(
            "/api/v1/imports",
            json!({ "source": "gitlab", "url": gitlab, "token": token, "target_namespace_id": target["id"],
                    "options": { "variables": false, "protected_branches": false, "members": false },
                    "projects": [{ "source_id": "1", "source_path": "team/origin", "target_path": "origin" }] }),
        )
        .await;
    let id = started["id"].as_i64().unwrap();
    wait_jobs(&srv.state).await;
    let t0 = std::time::Instant::now();
    owner.ok_post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    loop {
        let d = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
        if !matches!(d["status"].as_str(), Some("running")) {
            assert_eq!(d["projects"][0]["status"], "finished", "{d}");
            break;
        }
        assert!(t0.elapsed().as_secs() < 120, "too slow");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let took = t0.elapsed();
    eprintln!("imported {N} merge requests in {took:?}");
    let pid = owner.ok_get(&format!("/api/v1/imports/{id}")).await["projects"][0]["project_id"].as_i64().unwrap();
    let mrs = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests?state=all&per_page=100")).await;
    assert_eq!(mrs.as_array().unwrap().len() as i64, N);
    // the diff is the change itself, worked out from the merge commit or asked for
    for iid in [1, N] {
        let m = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests/{iid}")).await;
        assert_eq!(m["versions"][0]["base_sha"], base, "!{iid}");
        assert_eq!(m["versions"][0]["head_sha"], head, "!{iid}");
    }
    let singles = hits_on(&hits, |p| p.strip_prefix("/api/v4/projects/1/merge_requests/").is_some_and(|r| !r.contains('/')));
    assert_eq!(singles, 1, "only the fast-forwarded MR needs its own request");
    // the source is asked several things at once (one after the other, the
    // waiting alone is 100 x 50 ms); wall time only as a loose bound, since a
    // busy machine (the whole suite in parallel) slows the git work down
    let peak = in_flight.peak.load(std::sync::atomic::Ordering::SeqCst);
    assert!(peak >= 4, "at most {peak} request(s) in flight");
    assert!(took.as_secs_f64() < 30.0, "{N} merge requests took {took:?}");
}

/// A public project the importer has no token for, added by its URL.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn import_a_public_project_without_a_token(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await;
    owner.ok_post("/api/v1/projects", json!({ "name": "Pub", "path": "pub", "namespace_id": org["id"], "visibility": "public" })).await;
    let work = srv.work_dir("pub");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "open/pub"), "main"]).await);
    let head = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    wait_jobs(&srv.state).await;

    let project = json!({
        "id": 77, "path_with_namespace": "someone/pub", "name": "Pub", "path": "pub", "visibility": "public", "default_branch": "main",
        "http_url_to_repo": format!("{}/open/pub.git", srv.base), "web_url": "https://gitlab.example/someone/pub", "issues_enabled": true
    });
    // more issues than fit in one window: once the comments are refused, the
    // importer stops asking
    const ISSUES: i64 = 40;
    let issues: Vec<Value> = (1..=ISSUES)
        .map(|i| {
            json!({ "id": i, "iid": i, "title": if i == 1 { "Public issue".to_string() } else { format!("Issue {i}") },
                    "author": person(3, "someone", "Someone"), "state": "opened",
                    "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z" })
        })
        .collect();
    let mut routes: HashMap<String, Value> = [
        ("/api/v4/projects/someone%2Fpub", project.clone()),
        ("/api/v4/projects/77", project.clone()),
        // a group's listing also has projects shared with it, from elsewhere
        ("/api/v4/groups/someone/projects", {
            let mut shared = project.clone();
            shared["id"] = json!(78);
            shared["path_with_namespace"] = json!("other/shared");
            shared["path"] = json!("shared");
            json!([project, shared])
        }),
        ("/api/v4/projects/77/labels", json!("__401__")),
        ("/api/v4/projects/77/issues", Value::Array(issues)),
        ("/api/v4/projects/77/merge_requests", json!([{ "id": 501, "iid": 1, "title": "Public MR", "author": person(3, "someone", "Someone"),
            "state": "opened", "source_branch": "feature", "target_branch": "main", "sha": head, "source_project_id": 77, "target_project_id": 77,
            "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z" }])),
        ("/api/v4/projects/77/merge_requests/1/discussions", json!("__401__")),
        // looking people up is rate-limited for an hour: the import goes on without
        ("/api/v4/users/3", json!("__429__")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for i in 1..=ISSUES {
        routes.insert(format!("/api/v4/projects/77/issues/{i}/notes"), json!("__401__"));
    }
    let (gitlab, hits, _) = mock_gitlab_counted(routes, 0, true).await;

    // no token: nothing to list without a group, but a project can be added by its URL
    let listed = owner.ok_post("/api/v1/imports/connect", json!({ "source": "gitlab", "url": gitlab })).await;
    assert_eq!(listed["projects"], json!([]));
    // a group's public projects, without those only shared with it
    let listed = owner.ok_post("/api/v1/imports/connect", json!({ "source": "gitlab", "url": gitlab, "scope": "someone" })).await;
    let paths: Vec<&str> = listed["projects"].as_array().unwrap().iter().map(|p| p["full_path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["someone/pub"]);
    let found = owner
        .ok_post(
            "/api/v1/imports/lookup",
            json!({ "source": "gitlab", "url": gitlab, "path": format!("{gitlab}/someone/pub/-/tree/main") }),
        )
        .await;
    assert_eq!(found["id"], "77");
    let other_host =
        owner.post("/api/v1/imports/lookup", json!({ "source": "gitlab", "url": gitlab, "path": "https://elsewhere.example/a/b" })).await;
    assert_eq!(other_host.0, 400);
    let missing = owner.post("/api/v1/imports/lookup", json!({ "source": "gitlab", "url": gitlab, "path": "someone/private" })).await;
    assert_eq!(missing.0, 400);
    assert!(missing.1["message"].as_str().unwrap().contains("private"), "{}", missing.1);

    let target = owner.ok_post("/api/v1/groups", json!({ "name": "Imported", "path": "imported" })).await;
    let started = owner
        .ok_post(
            "/api/v1/imports",
            json!({ "source": "gitlab", "url": gitlab, "target_namespace_id": target["id"],
                    "projects": [{ "source_id": "77", "source_path": "someone/pub", "target_path": "pub" }] }),
        )
        .await;
    let id = started["id"].as_i64().unwrap();
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    wait_jobs(&srv.state).await;
    let done = owner.ok_get(&format!("/api/v1/imports/{id}")).await;
    assert_eq!(done["projects"][0]["status"], "finished", "{done}");
    let pid = done["projects"][0]["project_id"].as_i64().unwrap();
    let branches = owner.ok_get(&format!("/api/v1/projects/{pid}/repository/branches")).await;
    assert_eq!(branches[0]["name"], "main", "fetched without credentials");
    let issues = owner.ok_get(&format!("/api/v1/projects/{pid}/issues?state=all&per_page=100")).await;
    let issues = issues["issues"].as_array().unwrap();
    assert_eq!(issues.len() as i64, ISSUES, "imported even though their comments are hidden");
    assert!(issues.iter().any(|i| i["title"] == "Public issue"));
    let warnings = done["projects"][0]["report"]["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w.as_str().unwrap().starts_with("Comments could not be read")), "{warnings:?}");
    assert!(warnings.iter().any(|w| w.as_str().unwrap().starts_with("Labels could not be read")), "{warnings:?}");
    let mrs = owner.ok_get(&format!("/api/v1/projects/{pid}/merge_requests?state=all")).await;
    assert_eq!(mrs[0]["title"], "Public MR", "{mrs}");
    let notes = hits_on(&hits, |p| p.ends_with("/notes"));
    assert!(notes <= 32, "asked for comments {notes} times after they were refused");
    assert_eq!(hits_on(&hits, |p| p.ends_with("/discussions")), 0);
    assert_eq!(hits_on(&hits, |p| p.starts_with("/api/v4/users/")), 1, "people looked up until the first rate limit");
}
