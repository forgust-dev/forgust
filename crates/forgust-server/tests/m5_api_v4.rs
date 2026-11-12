//! M5: the GitLab-compatible `/api/v4` subset, walked the way Renovate and glab do.

mod common;

use base64::Engine;
use common::*;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

/// A Bearer-token client (Renovate's header), with access to response headers.
struct V4 {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl V4 {
    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, reqwest::header::HeaderMap, Value) {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{}/api/v4{path}", self.base) };
        let mut r = self.http.request(method, url).bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let text = resp.text().await.unwrap();
        (status, headers, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
    async fn get(&self, path: &str) -> (u16, reqwest::header::HeaderMap, Value) {
        self.send(reqwest::Method::GET, path, None).await
    }
    async fn ok(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let (s, _, v) = self.send(method.clone(), path, body).await;
        assert!((200..300).contains(&s), "{method} {path} -> {s} {v}");
        v
    }
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn renovate_and_glab_calls(pool: sqlx::PgPool) {
    use reqwest::Method as M;
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, proj) = org_project(&owner).await;
    owner.ok_post("/api/v1/projects", json!({ "name": "Web", "path": "web", "namespace_id": _sub })).await;
    let work = srv.work_dir("v4");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "renovate/serde-1.x"]).await);
    std::fs::write(work.join("deps.txt"), "serde = 1.0.200\n").unwrap();
    must(git(&work, &["add", "deps.txt"]).await);
    must(git(&work, &["commit", "-q", "-m", "Update serde"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "renovate/serde-1.x"]).await);
    wait_jobs(&srv.state).await;
    let token = owner.ok_post("/api/v1/user/tokens", json!({ "name": "renovate", "scopes": ["api", "write_repository"] })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let api = V4 { base: srv.base.clone(), token: token.clone(), http: reqwest::Client::new() };
    let pid = "acme%2Fplatform%2Fapi";

    // no token: 401; version and user
    let anon = reqwest::get(format!("{}/api/v4/user", srv.base)).await.unwrap();
    assert_eq!(anon.status(), 401);
    let v = api.ok(M::GET, "/version", None).await;
    assert!(v["version"].as_str().unwrap().starts_with("17."));
    let me = api.ok(M::GET, "/user", None).await;
    assert_eq!(me["username"], "olivia");
    assert_eq!(me["email"], "olivia@example.com");
    // glab sends PRIVATE-TOKEN
    let r = reqwest::Client::new().get(format!("{}/api/v4/user", srv.base)).header("PRIVATE-TOKEN", &token).send().await.unwrap();
    assert_eq!(r.status(), 200);

    // discovery: pages chained with Link headers on the external URL
    let (s, h, page1) = api.get("/projects?membership=true&min_access_level=30&per_page=1&archived=false").await;
    assert_eq!(s, 200);
    assert_eq!(page1.as_array().unwrap().len(), 1);
    let link = h.get("link").unwrap().to_str().unwrap().to_string();
    assert!(link.contains("rel=\"next\""), "{link}");
    assert_eq!(h.get("x-next-page").unwrap(), "2");
    let next = link.split(',').find(|l| l.contains("rel=\"next\"")).unwrap();
    let next = next.trim().trim_start_matches('<').split('>').next().unwrap();
    assert!(next.starts_with(&srv.base), "absolute, on the server's address: {next}");
    let (_, h2, page2) = api.get(next).await;
    assert_eq!(page2.as_array().unwrap().len(), 1);
    assert!(!h2.get("link").unwrap().to_str().unwrap().contains("rel=\"next\""));
    let paths: Vec<String> = [&page1, &page2].iter().map(|p| p[0]["path_with_namespace"].as_str().unwrap().to_string()).collect();
    assert!(paths.contains(&REPO.to_string()));
    let in_group = api.ok(M::GET, "/groups/acme/projects?include_subgroups=true&with_shared=false", None).await;
    assert_eq!(in_group.as_array().unwrap().len(), 2);

    // the repository, by encoded path or id
    let p = api.ok(M::GET, &format!("/projects/{pid}"), None).await;
    assert_eq!(p["id"], proj);
    assert_eq!(p["default_branch"], "main");
    assert_eq!(p["empty_repo"], false);
    assert_eq!(p["merge_method"], "merge");
    assert!(p["http_url_to_repo"].as_str().unwrap().ends_with("/acme/platform/api.git"));
    assert_eq!(api.ok(M::GET, &format!("/projects/{proj}"), None).await["path_with_namespace"], REPO);
    assert_eq!(api.get("/projects/acme%2Fnope").await.0, 404);

    // files: base64 content, or raw
    let f = api.ok(M::GET, &format!("/projects/{pid}/repository/files/README.md?ref=main"), None).await;
    let text = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(f["content"].as_str().unwrap()).unwrap()).unwrap();
    assert!(text.contains("# "), "{text}");
    let (s, _, raw) = api.get(&format!("/projects/{pid}/repository/files/README.md/raw?ref=main")).await;
    assert_eq!(s, 200);
    assert_eq!(raw, Value::String(text));
    assert_eq!(api.get(&format!("/projects/{pid}/repository/files/nope.txt?ref=main")).await.0, 404);

    // branches, one with a slash in its name
    let b = api.ok(M::GET, &format!("/projects/{pid}/repository/branches/renovate%2Fserde-1.x"), None).await;
    let head = b["commit"]["id"].as_str().unwrap().to_string();
    assert_eq!(b["protected"], false);
    let list = api.ok(M::GET, &format!("/projects/{pid}/repository/branches?search=renovate"), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    // commit statuses: Renovate's own, next to (absent) CI jobs
    api.ok(
        M::POST,
        &format!("/projects/{pid}/statuses/{head}"),
        Some(json!({ "state": "pending", "context": "renovate/stability-days", "description": "Waiting" })),
    )
    .await;
    api.ok(M::POST, &format!("/projects/{pid}/statuses/{head}"), Some(json!({ "state": "success", "context": "renovate/stability-days" })))
        .await;
    let sts = api.ok(M::GET, &format!("/projects/{pid}/repository/commits/{head}/statuses"), None).await;
    assert_eq!(sts.as_array().unwrap().len(), 1, "one status per context");
    assert_eq!(sts[0]["status"], "success");
    assert_eq!(sts[0]["name"], "renovate/stability-days");
    assert_eq!(api.send(M::POST, &format!("/projects/{pid}/statuses/{head}"), Some(json!({ "state": "bogus" }))).await.0, 400);
    let c = api.ok(M::GET, &format!("/projects/{pid}/repository/commits/{head}"), None).await;
    assert_eq!(c["title"], "Update serde");

    // merge requests: create (labels made on the fly), find by branch, update, comment, merge
    let (s, _, mr) = api
        .send(
            M::POST,
            &format!("/projects/{pid}/merge_requests"),
            Some(json!({ "source_branch": "renovate/serde-1.x", "target_branch": "main", "title": "Update serde", "description": "Bumps serde",
                         "labels": "dependencies,rust", "remove_source_branch": true, "squash": true })),
        )
        .await;
    assert_eq!(s, 201, "{mr}");
    assert_eq!(mr["iid"], 1);
    let mut labels: Vec<&str> = mr["labels"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
    labels.sort();
    assert_eq!(labels, vec!["dependencies", "rust"]);
    let dup = api
        .send(
            M::POST,
            &format!("/projects/{pid}/merge_requests"),
            Some(json!({ "source_branch": "renovate/serde-1.x", "target_branch": "main", "title": "x" })),
        )
        .await;
    assert_eq!(dup.0, 409);
    let found = api.ok(M::GET, &format!("/projects/{pid}/merge_requests?source_branch=renovate/serde-1.x&state=opened"), None).await;
    assert_eq!(found[0]["iid"], 1);
    assert_eq!(found[0]["sha"], head);
    let by_label = api
        .ok(
            M::GET,
            &format!("/projects/{pid}/merge_requests?labels=dependencies&scope=created_by_me&per_page=100&order_by=updated_at&sort=desc"),
            None,
        )
        .await;
    assert_eq!(by_label.as_array().unwrap().len(), 1);
    let one = api.ok(M::GET, &format!("/projects/{pid}/merge_requests/1?include_diverged_commits_count=1"), None).await;
    assert_eq!(one["detailed_merge_status"], "mergeable", "{one}");
    assert_eq!(one["merge_status"], "can_be_merged");
    assert_eq!(one["references"]["full"], format!("{REPO}!1"));
    assert_eq!(one["diff_refs"]["head_sha"], head);
    let upd = api
        .ok(
            M::PUT,
            &format!("/projects/{pid}/merge_requests/1"),
            Some(json!({ "title": "Update serde to 1.0.200", "add_labels": "security", "remove_labels": "rust" })),
        )
        .await;
    assert_eq!(upd["title"], "Update serde to 1.0.200");
    let mut labels: Vec<&str> = upd["labels"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
    labels.sort();
    assert_eq!(labels, vec!["dependencies", "security"]);
    let note =
        api.ok(M::POST, &format!("/projects/{pid}/merge_requests/1/notes"), Some(json!({ "body": "### Release notes\n\nok" }))).await;
    let nid = note["id"].as_i64().unwrap();
    api.ok(M::PUT, &format!("/projects/{pid}/merge_requests/1/notes/{nid}"), Some(json!({ "body": "### Release notes\n\nupdated" }))).await;
    let notes = api.ok(M::GET, &format!("/projects/{pid}/merge_requests/1/notes?per_page=100"), None).await;
    assert!(notes.as_array().unwrap().iter().any(|n| n["body"] == "### Release notes\n\nupdated"));
    let disc = api.ok(M::POST, &format!("/projects/{pid}/merge_requests/1/discussions"), Some(json!({ "body": "From glab" }))).await;
    let did = disc["id"].as_str().unwrap().to_string();
    assert!(disc["notes"][0]["id"].is_i64());
    api.ok(M::POST, &format!("/projects/{pid}/merge_requests/1/discussions/{did}/notes"), Some(json!({ "body": "A reply" }))).await;
    let ds = api.ok(M::GET, &format!("/projects/{pid}/merge_requests/1/discussions"), None).await;
    let thread = ds.as_array().unwrap().iter().find(|d| d["id"] == did.as_str()).unwrap();
    assert_eq!(thread["notes"].as_array().unwrap().len(), 2);
    let (s, _, _) = api.send(M::DELETE, &format!("/projects/{pid}/merge_requests/1/notes/{nid}"), None).await;
    assert_eq!(s, 204);
    assert_eq!(api.ok(M::GET, &format!("/projects/{pid}/merge_requests/1/approval_rules"), None).await, json!([]));
    let wrong = api.send(M::PUT, &format!("/projects/{pid}/merge_requests/1/merge"), Some(json!({ "sha": "0".repeat(40) }))).await;
    assert_eq!(wrong.0, 409);
    let merged = api
        .ok(
            M::PUT,
            &format!("/projects/{pid}/merge_requests/1/merge"),
            Some(json!({ "should_remove_source_branch": true, "squash": true })),
        )
        .await;
    assert_eq!(merged["state"], "merged");
    assert_eq!(api.send(M::PUT, &format!("/projects/{pid}/merge_requests/1/merge"), Some(json!({}))).await.0, 405);
    assert_eq!(api.get(&format!("/projects/{pid}/repository/branches/renovate%2Fserde-1.x")).await.0, 404, "source branch removed");

    // issues: the dependency dashboard
    let (s, _, dash) = api
        .send(
            M::POST,
            &format!("/projects/{pid}/issues"),
            Some(json!({ "title": "Dependency Dashboard", "description": "- [ ] serde", "labels": "renovate", "confidential": false })),
        )
        .await;
    assert_eq!(s, 201, "{dash}");
    assert_eq!(dash["labels"], json!(["renovate"]));
    let open = api.ok(M::GET, &format!("/projects/{pid}/issues?per_page=100&state=opened&scope=created_by_me"), None).await;
    assert_eq!(open[0]["title"], "Dependency Dashboard");
    assert_eq!(open[0]["labels"], json!(["renovate"]));
    let iid = dash["iid"].as_i64().unwrap();
    let got = api.ok(M::GET, &format!("/projects/{pid}/issues/{iid}"), None).await;
    assert_eq!(got["description"], "- [ ] serde");
    api.ok(M::PUT, &format!("/projects/{pid}/issues/{iid}"), Some(json!({ "description": "- [x] serde", "labels": "renovate,deps" })))
        .await;
    api.ok(M::POST, &format!("/projects/{pid}/issues/{iid}/notes"), Some(json!({ "body": "noted" }))).await;
    let closed = api.ok(M::PUT, &format!("/projects/{pid}/issues/{iid}"), Some(json!({ "state_event": "close" }))).await;
    assert_eq!(closed["state"], "closed");
    assert_eq!(closed["labels"].as_array().unwrap().len(), 2);
    let notes = api.ok(M::GET, &format!("/projects/{pid}/issues/{iid}/notes"), None).await;
    assert!(notes.as_array().unwrap().iter().any(|n| n["body"] == "noted" && n["system"] == false));

    // users, members, pipelines
    let users = api.ok(M::GET, "/users?username=olivia", None).await;
    assert_eq!(users[0]["id"], me["id"]);
    assert_eq!(api.ok(M::GET, "/users/olivia/status", None).await["availability"], "not_set");
    assert_eq!(api.ok(M::GET, &format!("/projects/{pid}/members"), None).await, json!([]), "olivia is a member through the group");
    let members = api.ok(M::GET, &format!("/projects/{pid}/members/all"), None).await;
    assert!(members.as_array().unwrap().iter().any(|m| m["username"] == "olivia" && m["access_level"] == 50), "{members}");
    assert_eq!(api.ok(M::GET, &format!("/projects/{pid}/pipelines"), None).await, json!([]));
    assert_eq!(api.get(&format!("/projects/{pid}/pipelines/latest?ref=main")).await.0, 404);

    // the runner API still answers in the same tree
    let r =
        reqwest::Client::new().post(format!("{}/api/v4/runners/verify", srv.base)).json(&json!({ "token": "nope" })).send().await.unwrap();
    assert_eq!(r.status(), 403);
}

/// What glab and import checks read besides Renovate's calls: the
/// tree, the commit list, an MR's changes and commits, labels.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn glab_reads_code_and_merge_requests(pool: sqlx::PgPool) {
    use reqwest::Method as M;
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, _proj) = org_project(&owner).await;
    let work = srv.work_dir("v4b");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    std::fs::write(work.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}").unwrap();
    std::fs::write(work.join("NEW.md"), "new\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "Say hi"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "topic"]).await);
    wait_jobs(&srv.state).await;
    let token =
        owner.ok_post("/api/v1/user/tokens", json!({ "name": "glab", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let api = V4 { base: srv.base.clone(), token, http: reqwest::Client::new() };
    let pid = "acme%2Fplatform%2Fapi";

    // the tree: a directory, then everything; trees first
    let top = api.ok(M::GET, &format!("/projects/{pid}/repository/tree"), None).await;
    let names: Vec<(&str, &str)> =
        top.as_array().unwrap().iter().map(|e| (e["type"].as_str().unwrap(), e["path"].as_str().unwrap())).collect();
    assert_eq!(names, vec![("tree", "src"), ("blob", "README.md")]);
    assert_eq!(top[1]["mode"], "100644");
    let (_, h, all) = api.get(&format!("/projects/{pid}/repository/tree?recursive=true&ref=topic&per_page=2")).await;
    assert_eq!(h.get("x-total").unwrap(), "4", "src, NEW.md, README.md, src/main.rs: {all}");
    assert_eq!(api.get(&format!("/projects/{pid}/repository/tree?path=nope")).await.0, 404);

    // commits: newest first, by ref and path, paged
    let (_, h, list) = api.get(&format!("/projects/{pid}/repository/commits?ref_name=topic&per_page=2")).await;
    assert_eq!(
        list.as_array().unwrap().iter().map(|c| c["title"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["Say hi", "greet the world"]
    );
    assert_eq!(h.get("x-next-page").unwrap(), "2");
    let on_readme = api.ok(M::GET, &format!("/projects/{pid}/repository/commits?ref_name=topic&path=README.md"), None).await;
    assert_eq!(on_readme.as_array().unwrap().len(), 1);
    let future = api.ok(M::GET, &format!("/projects/{pid}/repository/commits?since=2999-01-01T00:00:00Z"), None).await;
    assert!(future.as_array().unwrap().is_empty());

    // an MR's changes (unified diffs), diffs (paged) and commits
    api.ok(
        M::POST,
        &format!("/projects/{pid}/merge_requests"),
        Some(json!({ "source_branch": "topic", "target_branch": "main", "title": "Hi" })),
    )
    .await;
    let ch = api.ok(M::GET, &format!("/projects/{pid}/merge_requests/1/changes"), None).await;
    assert_eq!((ch["iid"].as_i64(), ch["changes_count"].as_str()), (Some(1), Some("2")), "{ch}");
    let main_rs = ch["changes"].as_array().unwrap().iter().find(|c| c["new_path"] == "src/main.rs").unwrap();
    let diff = main_rs["diff"].as_str().unwrap();
    assert!(diff.starts_with("@@ -1,3 +1,3 @@"), "{diff}");
    assert!(diff.contains("\n-    println!(\"hello, world\");\n-}\n+    println!(\"hi\");\n"), "{diff}");
    assert!(diff.ends_with("\n+}\n\\ No newline at end of file\n"), "{diff}");
    let new_md = ch["changes"].as_array().unwrap().iter().find(|c| c["new_path"] == "NEW.md").unwrap();
    assert_eq!((new_md["new_file"].as_bool(), new_md["deleted_file"].as_bool()), (Some(true), Some(false)));
    let (_, h, diffs) = api.get(&format!("/projects/{pid}/merge_requests/1/diffs?per_page=1")).await;
    assert_eq!((diffs.as_array().unwrap().len(), h.get("x-total").unwrap().to_str().unwrap()), (1, "2"));
    let commits = api.ok(M::GET, &format!("/projects/{pid}/merge_requests/1/commits"), None).await;
    assert_eq!(commits.as_array().unwrap().iter().map(|c| c["title"].as_str().unwrap()).collect::<Vec<_>>(), vec!["Say hi"]);

    // labels: created as GitLab takes them, listed with their counts
    api.ok(M::POST, &format!("/projects/{pid}/labels"), Some(json!({ "name": "bug", "color": "#d9534f", "description": "Broken" }))).await;
    api.ok(M::PUT, &format!("/projects/{pid}/merge_requests/1"), Some(json!({ "labels": "bug" }))).await;
    let labels = api.ok(M::GET, &format!("/projects/{pid}/labels"), None).await;
    assert_eq!(labels[0]["name"], "bug");
    assert_eq!((labels[0]["open_merge_requests_count"].as_i64(), labels[0]["open_issues_count"].as_i64()), (Some(1), Some(0)));
}
