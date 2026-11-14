//! M7: a real v1.

mod common;

use std::path::Path;

use common::*;
use serde_json::json;

const REPO: &str = "acme/platform/api";

/// Owner, a Developer (dave), a Reporter (rita), a pushed `main`.
async fn setup(srv: &TestServer) -> (Client, Client, Client, i64, std::path::PathBuf) {
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let rep = srv.user("rita", false).await;
    let (org, _sub, proj) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let work = srv.work_dir("m7");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    (owner, dev, rep, proj, work)
}

async fn push_branch(srv: &TestServer, work: &Path, branch: &str, file: &str) {
    must(git(work, &["checkout", "-q", "-B", branch, "main"]).await);
    commit_file(work, file, "x\n", branch).await;
    must(git(work, &["push", "-q", "-f", &srv.http_url("dave", REPO), branch]).await);
    wait_jobs(&srv.state).await;
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn source_branch_deletion_is_chosen_when_opening(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, _rep, proj, work) = setup(&srv).await;
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    let branches = || async { owner.ok_get(&format!("/api/v1/projects/{proj}/repository/branches")).await.to_string() };

    // chosen at creation (the web form ticks it; the API default is off, as GitLab's)
    push_branch(&srv, &work, "goes", "goes.txt").await;
    let m = dev.ok_post(&mrs, json!({ "source_branch": "goes", "title": "Goes", "remove_source_branch": true })).await;
    assert_eq!(m["remove_source_branch"], true);
    let st = owner.ok_get(&format!("{mrs}/1/merge_status")).await;
    owner.ok_post(&format!("{mrs}/1/merge"), json!({ "sha": st["head_sha"] })).await;
    assert!(!branches().await.contains("\"goes\""));

    // not chosen: kept, unless the merge says otherwise
    push_branch(&srv, &work, "stays", "stays.txt").await;
    let m = dev.ok_post(&mrs, json!({ "source_branch": "stays", "title": "Stays" })).await;
    assert_eq!(m["remove_source_branch"], false);
    let st = owner.ok_get(&format!("{mrs}/2/merge_status")).await;
    owner.ok_post(&format!("{mrs}/2/merge"), json!({ "sha": st["head_sha"] })).await;
    assert!(branches().await.contains("\"stays\""));

    // changed later, and seen by /api/v4
    push_branch(&srv, &work, "later", "later.txt").await;
    dev.ok_post(&mrs, json!({ "source_branch": "later", "title": "Later", "remove_source_branch": false })).await;
    assert_eq!(dev.put(&format!("{mrs}/3"), json!({ "remove_source_branch": true })).await.0, 200);
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/api/v4/projects/{proj}/merge_requests/3", srv.base))
        .header("PRIVATE-TOKEN", &tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v4["force_remove_source_branch"], true);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn description_templates(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, work) = setup(&srv).await;
    let tpl = |kind: &str| format!("/api/v1/projects/{proj}/templates/{kind}");
    assert_eq!(dev.ok_get(&tpl("merge_request")).await, json!([]));
    must(git(&work, &["checkout", "-q", "main"]).await);
    commit_file(&work, ".gitlab/merge_request_templates/Feature.md", "## What\n\n## Why\n", "tpl").await;
    commit_file(&work, ".gitlab/merge_request_templates/Default.md", "Closes #\n", "tpl").await;
    commit_file(&work, ".gitlab/merge_request_templates/notes.txt", "not a template", "tpl").await;
    commit_file(&work, ".github/ISSUE_TEMPLATE/bug.md", "Steps:\n", "tpl").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    let list = dev.ok_get(&tpl("merge_request")).await;
    let names: Vec<&str> = list.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["Default", "Feature"]);
    assert_eq!(list[1]["content"], "## What\n\n## Why\n");
    // issue templates are for anyone who can open issues
    assert_eq!(rep.ok_get(&tpl("issue")).await[0]["name"], "bug");
    assert_eq!(owner.get(&tpl("nope")).await.0, 400);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn editing_in_the_browser(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, _work) = setup(&srv).await;
    let repo = format!("/api/v1/projects/{proj}/repository");
    let raw = |r: &str, path: &str| format!("{repo}/raw?ref={r}&path={path}");
    let main = owner.ok_get(&format!("{repo}/branches")).await.as_array().unwrap().iter().find(|b| b["name"] == "main").unwrap()["commit"]
        ["id"]
        .as_str()
        .unwrap()
        .to_string();

    // a branch, then a commit on it touching several files
    assert_eq!(rep.post(&format!("{repo}/branches"), json!({ "branch": "docs", "ref": "main" })).await.0, 403);
    dev.ok_post(&format!("{repo}/branches"), json!({ "branch": "docs", "ref": "main" })).await;
    assert_eq!(dev.post(&format!("{repo}/branches"), json!({ "branch": "docs", "ref": "main" })).await.0, 409);
    assert_eq!(dev.post(&format!("{repo}/branches"), json!({ "branch": "bad..name", "ref": "main" })).await.0, 400);
    let c = dev
        .ok_post(
            &format!("{repo}/commits"),
            json!({ "branch": "docs", "commit_message": "Docs", "expected_head": main, "actions": [
                { "action": "update", "file_path": "README.md", "content": "# Updated\n" },
                { "action": "create", "file_path": "docs/guide.md", "content": "Guide\n" },
                { "action": "create", "file_path": "bin.dat", "content": "AAEC", "encoding": "base64" },
            ] }),
        )
        .await;
    assert_eq!(c["branch"], "docs");
    assert_eq!(c["created_branch"], false);
    assert_eq!(dev.get_bytes(&raw("docs", "README.md")).await.1, b"# Updated\n");
    assert_eq!(dev.get_bytes(&raw("docs", "bin.dat")).await.1, vec![0u8, 1, 2]);
    let info = dev.ok_get(&format!("{repo}/commits/{}", c["id"].as_str().unwrap())).await;
    assert_eq!(info["subject"], "Docs");
    assert_eq!(info["author"]["name"], "dave");

    // the editor's view is stale: refused
    let stale = dev
        .post(
            &format!("{repo}/commits"),
            json!({ "branch": "docs", "commit_message": "x", "expected_head": main, "actions": [{ "action": "update", "file_path": "README.md", "content": "y" }] }),
        )
        .await;
    assert_eq!(stale.0, 409, "{}", stale.1);
    // actions that do not fit the tree
    let commits = format!("{repo}/commits");
    let bad = |a: serde_json::Value| dev.post(&commits, json!({ "branch": "docs", "commit_message": "x", "actions": [a] }));
    assert_eq!(bad(json!({ "action": "create", "file_path": "README.md", "content": "x" })).await.0, 409);
    assert_eq!(bad(json!({ "action": "update", "file_path": "nope.md", "content": "x" })).await.0, 409);
    assert_eq!(bad(json!({ "action": "update", "file_path": "../etc/passwd", "content": "x" })).await.0, 400);
    assert_eq!(bad(json!({ "action": "update", "file_path": "README.md", "content": "# Updated\n" })).await.0, 400); // nothing changed

    // a move and a delete, onto a new branch started from docs
    let c = dev
        .ok_post(
            &format!("{repo}/commits"),
            json!({ "branch": "tidy", "start_branch": "docs", "commit_message": "Tidy", "actions": [
                { "action": "move", "file_path": "docs/handbook.md", "previous_path": "docs/guide.md" },
                { "action": "delete", "file_path": "bin.dat" },
            ] }),
        )
        .await;
    assert_eq!(c["created_branch"], true);
    assert_eq!(dev.get_bytes(&raw("tidy", "docs/handbook.md")).await.1, b"Guide\n");
    assert_eq!(dev.get_bytes(&raw("tidy", "bin.dat")).await.0, 404);
    // "a new branch" onto a name that exists: refused, unless replacing it
    let again = json!({ "branch": "tidy", "start_branch": "main", "commit_message": "Again", "actions": [{ "action": "create", "file_path": "again.md", "content": "a" }] });
    let (code, body) = dev.post(&commits, again.clone()).await;
    assert_eq!(code, 400, "{body}");
    let mut forced = again;
    forced["force"] = json!(true);
    dev.ok_post(&commits, forced).await;
    assert_eq!(dev.get_bytes(&raw("tidy", "docs/handbook.md")).await.0, 404); // started over from main
    assert_eq!(dev.get_bytes(&raw("tidy", "again.md")).await.1, b"a");
    wait_jobs(&srv.state).await;

    // main is protected (Maintainers push): a Developer commits elsewhere
    owner
        .ok_post(
            &format!("/api/v1/projects/{proj}/protected_branches"),
            json!({ "name": "main", "push_access_level": "maintainer", "merge_access_level": "maintainer" }),
        )
        .await;
    let (code, body) = dev
        .post(
            &format!("{repo}/commits"),
            json!({ "branch": "main", "commit_message": "x", "actions": [{ "action": "create", "file_path": "z.md", "content": "z" }] }),
        )
        .await;
    assert_eq!(code, 403, "{body}");
    // deleting: not the default branch; others go
    assert_eq!(owner.delete(&format!("{repo}/branches/main")).await.0, 400);
    assert_eq!(dev.delete(&format!("{repo}/branches/tidy")).await.0, 204);
    assert!(!owner.ok_get(&format!("{repo}/branches")).await.to_string().contains("\"tidy\""));

    // /api/v4: GitLab's commit API (Renovate's platformCommit)
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/projects/{proj}/repository/commits", srv.base))
        .header("PRIVATE-TOKEN", &tok)
        .json(&json!({ "branch": "renovate/x", "start_branch": "main", "commit_message": "Update x", "actions": [{ "action": "create", "file_path": "x.lock", "content": "1" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["title"], "Update x");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn tags_uploads_and_releases(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, work) = setup(&srv).await;
    let repo = format!("/api/v1/projects/{proj}/repository");
    let rels = format!("/api/v1/projects/{proj}/releases");

    // tags: Developers create them, lightweight or annotated
    assert_eq!(rep.post(&format!("{repo}/tags"), json!({ "tag_name": "v1.0", "ref": "main" })).await.0, 403);
    let t = dev.ok_post(&format!("{repo}/tags"), json!({ "tag_name": "v1.0", "ref": "main" })).await;
    assert_eq!(t["annotated"], false);
    let t = dev.ok_post(&format!("{repo}/tags"), json!({ "tag_name": "v1.1", "ref": "main", "message": "One point one" })).await;
    assert_eq!(t["annotated"], true);
    assert_ne!(t["target"], t["commit"]);
    assert_eq!(dev.post(&format!("{repo}/tags"), json!({ "tag_name": "v1.1", "ref": "main" })).await.0, 409);
    // an annotated tag is deleted by its own value, not its commit's
    assert_eq!(dev.delete(&format!("{repo}/tags/v1.1")).await.0, 204);
    assert!(!dev.ok_get(&format!("{repo}/tags")).await.to_string().contains("v1.1"));

    // uploads: who can see the project may add files; served with safe headers
    let (code, up) = rep.upload(&format!("/api/v1/projects/{proj}/uploads"), "../shot.png", b"\x89PNG fake").await;
    assert_eq!(code, 201, "{up}");
    assert_eq!(up["alt"], "shot.png");
    assert!(up["markdown"].as_str().unwrap().starts_with("![shot.png](/-/project/"), "{up}");
    let png = up["full_path"].as_str().unwrap().to_string();
    let r = rep.req(reqwest::Method::GET, &png).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    assert!(r.headers()["content-disposition"].to_str().unwrap().starts_with("inline"));
    let (_, bin) = dev.upload(&format!("/api/v1/projects/{proj}/uploads"), "app.html", b"<script>alert(1)</script>").await;
    let file = bin["full_path"].as_str().unwrap().to_string();
    let r = dev.req(reqwest::Method::GET, &file).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "application/octet-stream");
    assert!(r.headers()["content-disposition"].to_str().unwrap().starts_with("attachment"));
    // a private project's files are not for strangers; the name must match
    assert!(matches!(Client::new(&srv.base).get_bytes(&file).await.0, 401 | 404));
    assert_eq!(dev.get_bytes(&file.replace("app.html", "other.html")).await.0, 404);

    // a release on a new tag (with `/`), with an uploaded file and an outside link
    let body = json!({
        "tag_name": "release/v2.0", "ref": "main", "tag_message": "Two", "name": "Two point oh", "description": "Fixes #1",
        "links": [
            { "name": "app", "url": file, "direct_asset_path": "bin/app", "link_type": "package" },
            { "name": "docs", "url": "https://example.com/docs" },
        ],
    });
    assert_eq!(rep.post(&rels, body.clone()).await.0, 403);
    let r = dev.ok_post(&rels, body.clone()).await;
    assert_eq!(r["name"], "Two point oh");
    assert_eq!(r["author_username"], "dave");
    assert_eq!(r["links"].as_array().unwrap().len(), 2);
    assert!(r["links"][0]["href"].as_str().unwrap().starts_with("http"), "{r}");
    assert!(r["commit"]["id"].is_string());
    assert_eq!(dev.post(&rels, body).await.0, 409);
    assert_eq!(dev.post(&rels, json!({ "tag_name": "nope" })).await.0, 400); // no tag, no ref
    let bad = json!({ "tag_name": "v1.0", "links": [{ "name": "x", "url": "javascript:alert(1)" }] });
    assert_eq!(dev.post(&rels, bad).await.0, 400);
    // on an existing tag; listed newest first; addressed by the encoded tag
    dev.ok_post(&rels, json!({ "tag_name": "v1.0", "released_at": "2020-01-01T00:00:00Z" })).await;
    let list = rep.ok_get(&rels).await;
    let tags: Vec<&str> = list.as_array().unwrap().iter().map(|r| r["tag_name"].as_str().unwrap()).collect();
    assert_eq!(tags, vec!["release/v2.0", "v1.0"]);
    let enc = "release%2Fv2.0";
    // a Guest sees releases but not code: no commit, no source archives
    let guest = srv.user("gus", false).await;
    owner.ok_post(&format!("/api/v1/projects/{proj}/members"), json!({ "username": "gus", "access_level": "guest" })).await;
    let seen = guest.ok_get(&format!("{rels}/{enc}")).await;
    assert!(seen["commit"].is_null() && seen["sources"] == json!([]), "{seen}");
    assert!(rep.ok_get(&format!("{rels}/{enc}")).await["commit"].is_object());
    assert_eq!(rep.ok_get(&format!("{rels}/{enc}")).await["description"], "Fixes #1");
    assert_eq!(dev.put(&format!("{rels}/{enc}"), json!({ "description": "Notes" })).await.0, 200);
    assert!(rep.ok_get(&format!("{repo}/tags")).await.to_string().contains("Two point oh"));
    // the asset's permanent URL redirects to the file
    let r = rep.req(reqwest::Method::GET, &format!("/acme/platform/api/-/releases/{enc}/downloads/bin/app")).send().await.unwrap();
    assert_eq!(r.status(), 302);
    assert!(r.headers()["location"].to_str().unwrap().ends_with(&file));

    // removing the link keeps the file (it may be used elsewhere)
    let app = "app";
    let shown = dev.ok_get(&format!("{rels}/{enc}")).await;
    let link_id = shown["links"].as_array().unwrap().iter().find(|l| l["name"] == app).unwrap()["id"].as_i64().unwrap();
    assert_eq!(dev.delete(&format!("{rels}/{enc}/links/{link_id}")).await.0, 204);
    assert_eq!(dev.get_bytes(&file).await.0, 200);

    // a tag deleted by a push takes its release with it
    must(git(&work, &["fetch", "-q", &srv.http_url("dave", REPO), "+refs/tags/*:refs/tags/*"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), ":refs/tags/v1.0"]).await);
    wait_jobs(&srv.state).await;
    assert_eq!(rep.get(&format!("{rels}/v1.0")).await.0, 404);

    // /api/v4, as glab: a token, then a CI job token (JOB-TOKEN)
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4 = Client::with_token(&srv.base, &tok);
    let (code, up) = v4.upload(&format!("/api/v4/projects/{proj}/uploads"), "tool.tar.gz", b"tarball").await;
    assert_eq!(code, 201, "{up}");
    let url = format!("{}{}", srv.base, up["full_path"].as_str().unwrap());
    let created = v4
        .ok_post(
            &format!("/api/v4/projects/{proj}/releases"),
            json!({ "tag_name": "v3.0", "ref": "main", "description": "Three", "assets": { "links": [{ "name": "tool", "url": url, "direct_asset_path": "/tool.tar.gz" }] } }),
        )
        .await;
    assert_eq!(
        created["assets"]["links"][0]["direct_asset_url"],
        format!("{}/acme/platform/api/-/releases/v3.0/downloads/tool.tar.gz", srv.state.cfg.server.external_url.trim_end_matches('/'))
    );
    assert_eq!(created["assets"]["sources"].as_array().unwrap().len(), 2);
    assert_eq!(v4.ok_get(&format!("/api/v4/projects/{proj}/repository/tags/v3.0")).await["release"]["tag_name"], "v3.0");
    assert_eq!(v4.ok_get(&format!("/api/v4/projects/{proj}/releases/v3.0/assets/links")).await.as_array().unwrap().len(), 1);
    let dave = dev.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let rita = rep.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let job = |user: i64| {
        let state = srv.state.clone();
        async move {
            let t = format!("fgcbt-test{user}{}", rand_suffix());
            forgust_db::tokens::create_job_token(
                &state.db,
                &forgust_server::crypto::token_hash(&t),
                proj,
                user,
                chrono::Utc::now() + chrono::Duration::hours(1),
            )
            .await
            .unwrap();
            t
        }
    };
    let jt = job(dave).await;
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/projects/{proj}/releases", srv.base))
        .header("JOB-TOKEN", &jt)
        .json(&json!({ "tag_name": "v4.0", "ref": "main" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let jt = job(rita).await; // a Reporter's job cannot
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/projects/{proj}/releases", srv.base))
        .header("JOB-TOKEN", &jt)
        .json(&json!({ "tag_name": "v5.0", "ref": "main" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // GitLab answers a deleted release with its last state
    let gone = reqwest::Client::new()
        .delete(format!("{}/api/v4/projects/{proj}/releases/v3.0", srv.base))
        .header("PRIVATE-TOKEN", &tok)
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 200);

    // deleting the project leaves no file behind in the blob store
    let key = format!("uploads/{proj}/{}", png.split('/').nth(5).unwrap());
    assert!(srv.state.blobs.size(&key).await.unwrap().is_some(), "{key}");
    assert!(matches!(owner.delete(&format!("/api/v1/projects/{proj}")).await.0, 200 | 204));
    forgust_server::services::uploads::drain_deletions(&srv.state).await.unwrap();
    assert!(srv.state.blobs.size(&key).await.unwrap().is_none());
}

fn rand_suffix() -> String {
    format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn milestones_and_due_dates(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, work) = setup(&srv).await;
    let ms = format!("/api/v1/projects/{proj}/milestones");
    let issues = format!("/api/v1/projects/{proj}/issues");

    assert_eq!(rep.post(&ms, json!({ "title": "v1" })).await.0, 403);
    let v1 = dev.ok_post(&ms, json!({ "title": "v1", "due_date": "2030-01-31", "description": "First" })).await;
    assert_eq!(v1["state"], "active");
    assert_eq!(v1["iid"], 1);
    assert_eq!(dev.post(&ms, json!({ "title": "v1" })).await.0, 409);
    assert_eq!(dev.post(&ms, json!({ "title": "bad", "due_date": "31/01/2030" })).await.0, 400);
    let v1id = v1["id"].as_i64().unwrap();

    // on issues: Developers set it, with a note; the author may set a due date
    let a = rep.ok_post(&issues, json!({ "title": "A" })).await;
    assert_eq!(rep.put(&format!("{issues}/1"), json!({ "milestone_id": v1id })).await.0, 403);
    let a2 = rep.ok_post(&issues, json!({ "title": "B" })).await;
    let (code, b) = rep.put(&format!("{issues}/2"), json!({ "due_date": "2030-01-15" })).await;
    assert_eq!(code, 200, "{b}");
    assert_eq!(b["due_date"], "2030-01-15");
    assert_eq!(a["iid"], 1);
    assert_eq!(a2["iid"], 2);
    let (_, a) = dev.put(&format!("{issues}/1"), json!({ "milestone_id": v1id })).await;
    assert_eq!(a["milestone_title"], "v1");
    let notes = dev.ok_get(&format!("{issues}/1/notes")).await;
    assert!(notes.to_string().contains("changed milestone to %\\\"v1\\\""), "{notes}");
    // a milestone of another project is refused
    assert_eq!(dev.put(&format!("{issues}/2"), json!({ "milestone_id": 999_999 })).await.0, 400);
    // filters
    let on = dev.ok_get(&format!("{issues}?milestone=v1")).await;
    assert_eq!(on["issues"].as_array().unwrap().len(), 1);
    let none = dev.ok_get(&format!("{issues}?milestone=None")).await;
    assert_eq!(none["issues"][0]["iid"], 2);
    assert_eq!(dev.ok_get(&format!("{issues}?milestone=nope")).await["issues"], json!([]));
    // progress
    dev.put(&format!("{issues}/1"), json!({ "state_event": "close" })).await;
    let shown = rep.ok_get(&format!("{ms}/{v1id}")).await;
    assert_eq!((shown["open_issues"].as_i64(), shown["closed_issues"].as_i64()), (Some(0), Some(1)));

    // on merge requests
    must(git(&work, &["checkout", "-q", "-B", "topic", "main"]).await);
    std::fs::write(work.join("t.txt"), "t\n").unwrap();
    must(git(&work, &["add", "t.txt"]).await);
    must(git(&work, &["commit", "-q", "-m", "t"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "topic"]).await);
    wait_jobs(&srv.state).await;
    let m = dev
        .ok_post(
            &format!("/api/v1/projects/{proj}/merge_requests"),
            json!({ "source_branch": "topic", "title": "T", "milestone_id": v1id }),
        )
        .await;
    assert_eq!(m["milestone_title"], "v1");
    assert_eq!(dev.ok_get(&format!("/api/v1/projects/{proj}/merge_requests?milestone=v1")).await.as_array().unwrap().len(), 1);
    // changes show in the timeline, as on issues
    let mr = format!("/api/v1/projects/{proj}/merge_requests/{}", m["iid"]);
    assert_eq!(dev.put(&mr, json!({ "milestone_id": 0 })).await.0, 200);
    let notes = dev.ok_get(&format!("{mr}/notes")).await.to_string();
    assert!(notes.contains("removed milestone %\\\"v1\\\""), "{notes}");

    // /api/v4: milestones, and issues carrying them; null clears
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4 = Client::with_token(&srv.base, &tok);
    let list = v4.ok_get(&format!("/api/v4/projects/{proj}/milestones?title=v1")).await;
    assert_eq!(list[0]["due_date"], "2030-01-31");
    let i = v4.ok_get(&format!("/api/v4/projects/{proj}/issues/1")).await;
    assert_eq!(i["milestone"]["title"], "v1");
    let (code, i) = v4.put(&format!("/api/v4/projects/{proj}/issues/2"), json!({ "due_date": null, "milestone_id": v1id })).await;
    assert_eq!(code, 200, "{i}");
    assert!(i["due_date"].is_null());
    assert_eq!(i["milestone"]["id"], v1id);
    let (_, i) = v4.put(&format!("/api/v4/projects/{proj}/issues/2"), json!({ "milestone_id": null })).await;
    assert!(i["milestone"].is_null());
    let closed = v4.put(&format!("/api/v4/projects/{proj}/milestones/{v1id}"), json!({ "state_event": "close" })).await.1;
    assert_eq!(closed["state"], "closed");

    // refused before anything is created: no stray issue
    let count = || async { dev.ok_get(&format!("{issues}?state=all")).await["issues"].as_array().unwrap().len() };
    let n = count().await;
    assert_eq!(rep.post(&issues, json!({ "title": "X", "milestone_id": v1id })).await.0, 403);
    assert_eq!(rep.post(&issues, json!({ "title": "X", "due_date": "soon" })).await.0, 400);
    assert_eq!(count().await, n);

    // deleting it takes it off everything
    assert_eq!(dev.delete(&format!("{ms}/{v1id}")).await.0, 204);
    assert!(dev.ok_get(&format!("{issues}/1")).await["milestone_id"].is_null());
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn emoji_reactions(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, _work) = setup(&srv).await;
    let issue = format!("/api/v1/projects/{proj}/issues/1");
    rep.ok_post(&format!("/api/v1/projects/{proj}/issues"), json!({ "title": "A" })).await;
    let note = rep.ok_post(&format!("{issue}/notes"), json!({ "body": "Me too" })).await;
    let note_id = note["id"].as_i64().unwrap();

    // toggled on, then off; counted per emoji with who
    assert_eq!(dev.ok_post(&format!("{issue}/reactions"), json!({ "name": "thumbsup" })).await["awarded"], true);
    assert_eq!(rep.ok_post(&format!("{issue}/reactions"), json!({ "name": "thumbsup" })).await["awarded"], true);
    assert_eq!(rep.ok_post(&format!("{issue}/reactions"), json!({ "name": "rocket", "note_id": note_id })).await["awarded"], true);
    let r = dev.ok_get(&format!("{issue}/reactions")).await;
    assert_eq!(r["target"][0]["name"], "thumbsup");
    assert_eq!(r["target"][0]["count"], 2);
    assert_eq!(r["target"][0]["me"], true);
    assert_eq!(r["notes"][note_id.to_string()][0]["name"], "rocket");
    assert_eq!(r["notes"][note_id.to_string()][0]["me"], false);
    assert_eq!(dev.ok_post(&format!("{issue}/reactions"), json!({ "name": "thumbsup" })).await["awarded"], false);
    assert_eq!(dev.ok_get(&format!("{issue}/reactions")).await["target"][0]["count"], 1);
    // only the eight; a comment of another issue is not this one's
    assert_eq!(dev.post(&format!("{issue}/reactions"), json!({ "name": "poop" })).await.0, 400);
    assert_eq!(dev.post(&format!("{issue}/reactions"), json!({ "name": "eyes", "note_id": 999_999 })).await.0, 404);
    // anonymous people see, but do not react (private project: not even see)
    assert!(matches!(Client::new(&srv.base).get(&format!("{issue}/reactions")).await.0, 401 | 404));

    // /api/v4 award_emoji, as GitLab
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4 = Client::with_token(&srv.base, &tok);
    let base = format!("/api/v4/projects/{proj}/issues/1/award_emoji");
    let a = v4.ok_post(&base, json!({ "name": "heart" })).await;
    assert_eq!(a["awardable_type"], "Issue");
    assert_eq!(a["user"]["username"], "olivia");
    assert_eq!(v4.post(&base, json!({ "name": "heart" })).await.0, 400);
    let list = v4.ok_get(&base).await;
    assert_eq!(list.as_array().unwrap().len(), 2, "{list}"); // rita's 👍 and this ❤
    let theirs = list.as_array().unwrap().iter().find(|x| x["name"] == "thumbsup").unwrap()["id"].as_i64().unwrap();
    assert_eq!(v4.delete(&format!("{base}/{theirs}")).await.0, 403);
    assert_eq!(v4.delete(&format!("{base}/{}", a["id"])).await.0, 204);
    // the merge request routes are there too (none open here)
    assert_eq!(v4.get(&format!("/api/v4/projects/{proj}/merge_requests/1/award_emoji")).await.0, 404);
}

/// Emails sent so far to `who` whose subject or body contains `text`.
fn mails_to(srv: &TestServer, who: &str, text: &str) -> usize {
    let addr = format!("{who}@example.com");
    srv.state.mailer.sent().iter().filter(|m| m.to.contains(&addr) && (m.subject.contains(text) || m.body.contains(text))).count()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn notification_levels_and_watching(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, _work) = setup(&srv).await;
    let stranger = srv.user("sam", false).await;
    let settings = format!("/api/v1/projects/{proj}/notification_settings");
    let issues = format!("/api/v1/projects/{proj}/issues");

    // dave watches; sam cannot see the (private) project, whatever he chooses
    assert_eq!(dev.put(&settings, json!({ "level": "watch" })).await.0, 200);
    assert_eq!(dev.ok_get(&settings).await["effective"], "watch");
    assert_eq!(stranger.put(&settings, json!({ "level": "watch" })).await.0, 404);
    assert_eq!(rep.put(&settings, json!({ "level": "loud" })).await.0, 400);

    // a new issue reaches watchers only
    owner.ok_post(&issues, json!({ "title": "First" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(mails_to(&srv, "dave", "opened issue #1"), 1);
    assert_eq!(mails_to(&srv, "rita", "opened issue #1"), 0);
    let mail = srv.state.mailer.sent().into_iter().find(|m| m.body.contains("opened issue #1")).unwrap();
    assert!(mail.body.contains("/profile/notifications"), "{}", mail.body);

    // rita takes part: closing it reaches her (participating) and dave (watch), not olivia (the actor)
    rep.ok_post(&format!("{issues}/1/notes"), json!({ "body": "Seen it" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(mails_to(&srv, "olivia", "rita commented"), 1, "the author takes part");
    owner.put(&format!("{issues}/1"), json!({ "state_event": "close" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(mails_to(&srv, "rita", "closed issue #1"), 1);
    assert_eq!(mails_to(&srv, "dave", "closed issue #1"), 1);
    assert_eq!(mails_to(&srv, "olivia", "closed issue #1"), 0);

    // mention only: activity is quiet, a mention is not
    rep.put(&settings, json!({ "level": "mention" })).await;
    owner.put(&format!("{issues}/1"), json!({ "state_event": "reopen" })).await;
    owner.ok_post(&format!("{issues}/1/notes"), json!({ "body": "@rita can you check?" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(mails_to(&srv, "rita", "reopened issue #1"), 0);
    assert_eq!(mails_to(&srv, "rita", "mentioned you"), 1);
    // disabled: nothing at all
    rep.put(&settings, json!({ "level": "disabled" })).await;
    owner.ok_post(&format!("{issues}/1/notes"), json!({ "body": "@rita again" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(mails_to(&srv, "rita", "mentioned you"), 1);

    // a default of watch covers projects one is a member of
    assert_eq!(rep.put(&settings, json!({ "level": "global" })).await.1["effective"], "participating");
    assert_eq!(rep.put("/api/v1/user/notification_settings", json!({ "level": "watch" })).await.0, 200);
    stranger.put("/api/v1/user/notification_settings", json!({ "level": "watch" })).await;
    owner.ok_post(&issues, json!({ "title": "Second" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(mails_to(&srv, "rita", "opened issue #2"), 1);
    assert_eq!(mails_to(&srv, "sam", "opened issue #2"), 0);

    // someone removed from the project stops hearing about it, watch or not
    let members = owner.ok_get(&format!("/api/v1/projects/{proj}/members")).await.to_string();
    assert!(!members.contains("\"dave\""), "dave is a group member");
    let org = owner.ok_get("/api/v1/groups/acme").await["id"].as_i64().unwrap();
    let dave = dev.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    assert!(matches!(owner.delete(&format!("/api/v1/groups/{org}/members/{dave}")).await.0, 200 | 204));
    owner.ok_post(&issues, json!({ "title": "Third" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(mails_to(&srv, "dave", "opened issue #3"), 0);

    // one message per person: their reason in a header and the footer, threads, unsubscribe
    let sent = srv.state.mailer.sent();
    assert!(sent.iter().all(|m| m.to.len() == 1), "one recipient each");
    let opened = sent.iter().find(|m| m.body.contains("opened issue #2") && m.to[0] == "rita@example.com").unwrap();
    assert!(opened.body.contains("You get this because you watch acme/platform/api."), "{}", opened.body);
    let header = |m: &forgust_server::services::notify::Email, k: &str| m.headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.clone());
    assert_eq!(header(opened, "X-Forgust-Notification-Reason").as_deref(), Some("watching"));
    let thread = opened.message_id.clone().unwrap();
    assert!(thread.starts_with("<issue-"), "{thread}");
    let closed = sent.iter().find(|m| m.body.contains("closed issue #1")).unwrap();
    assert!(closed.references.as_deref().unwrap().starts_with("<issue-") && closed.message_id != closed.references);
    let unsub = header(opened, "List-Unsubscribe").unwrap();
    let path = unsub.trim_matches(['<', '>']).split_once("/api/v1/").map(|(_, p)| format!("/api/v1/{p}")).unwrap();
    let anon = Client::new(&srv.base);
    // looking changes nothing (mail scanners follow links); a forged link is refused
    assert_eq!(anon.ok_get(&path).await["level"], "watch");
    assert_eq!(anon.get(&format!("{}x", path)).await.0, 404);
    assert_eq!(rep.ok_get(&settings).await["effective"], "watch");
    let r = anon.http.post(format!("{}{path}", srv.base)).body("List-Unsubscribe=One-Click").send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(rep.ok_get(&settings).await["effective"], "disabled");
    owner.ok_post(&issues, json!({ "title": "Fourth" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(mails_to(&srv, "rita", "opened issue #4"), 0);

    // /api/v4, as GitLab
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4 = Client::with_token(&srv.base, &tok);
    assert_eq!(v4.ok_get("/api/v4/notification_settings").await["level"], "participating");
    assert_eq!(v4.put(&format!("/api/v4/projects/{proj}/notification_settings"), json!({ "level": "watch" })).await.1["level"], "watch");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn inbox_and_a_switch_per_kind_and_channel(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, work) = setup(&srv).await;
    let inbox = "/api/v1/user/notifications";
    let channels = "/api/v1/user/notification_channels";
    let of = |v: &serde_json::Value, kind: &str| {
        v["items"].as_array().unwrap().iter().filter(|n| n["kind"] == kind).cloned().collect::<Vec<_>>()
    };

    // everything goes to the app and by email, but the email of a pipeline that passes again
    let all = dev.ok_get(channels).await;
    assert_eq!(all.as_array().unwrap().len(), 7, "{all}");
    assert!(
        all.as_array()
            .unwrap()
            .iter()
            .all(|c| c["web"] == true && c["screen"] == true && (c["email"] == true) == (c["kind"] != "pipeline_fixed")),
        "{all}"
    );
    assert_eq!(dev.ok_put(channels, json!({ "kind": "activity", "channel": "screen", "enabled": false })).await[2]["screen"], false);
    assert!(matches!(dev.put(channels, json!({ "kind": "loud", "channel": "web", "enabled": false })).await.0, 400 | 422));
    assert!(matches!(dev.put(channels, json!({ "kind": "review", "channel": "pager", "enabled": false })).await.0, 400 | 422));
    // every kind the server knows is one the database takes
    for c in all.as_array().unwrap() {
        dev.ok_put(channels, json!({ "kind": c["kind"], "channel": "web", "enabled": true })).await;
    }

    // an approval reaches the author: the bell and an email
    push_branch(&srv, &work, "feature", "a.txt").await;
    let mrs = format!("/api/v1/projects/{proj}/merge_requests");
    dev.ok_post(&mrs, json!({ "source_branch": "feature", "target_branch": "main", "title": "Add a" })).await;
    rep.ok_post(&format!("{mrs}/1/approve"), json!({})).await;
    wait_jobs(&srv.state).await;
    let mine = dev.ok_get(inbox).await;
    let reviews = of(&mine, "review");
    assert_eq!(reviews.len(), 1, "{mine}");
    assert_eq!(mine["unread"], 1, "{mine}");
    let n = &reviews[0];
    assert_eq!((n["summary"].as_str(), n["title"].as_str(), n["actor_username"].as_str()), (Some("approved"), Some("Add a"), Some("rita")));
    assert_eq!((n["target_type"].as_str(), n["target_iid"].as_i64(), n["read"].as_bool()), (Some("merge_request"), Some(1), Some(false)));
    assert_eq!(mails_to(&srv, "dave", "rita approved merge request !1"), 1);
    // approving again says nothing new
    rep.ok_post(&format!("{mrs}/1/approve"), json!({})).await;
    wait_jobs(&srv.state).await;
    assert_eq!(of(&dev.ok_get(inbox).await, "review").len(), 1);
    // not who approved, and nobody reads another's inbox
    assert_eq!(rep.ok_get(inbox).await["unread"], 0);
    let id = n["id"].as_i64().unwrap();
    assert_eq!(rep.post(&format!("{inbox}/{id}/read"), json!({})).await.0, 404);

    // read it; then all
    assert_eq!(dev.ok_post(&format!("{inbox}/{id}/read"), json!({})).await["unread"], 0);
    assert_eq!(dev.post(&format!("{inbox}/{id}/read"), json!({})).await.0, 200, "reading it again is not an error");
    assert_eq!(of(&dev.ok_get(inbox).await, "review")[0]["read"], true);

    // email off for reviews: the bell only; then the bell off too
    let set = dev.ok_put(channels, json!({ "kind": "review", "channel": "email", "enabled": false })).await;
    assert!(set.as_array().unwrap().iter().any(|c| c["kind"] == "review" && c["web"] == true && c["email"] == false), "{set}");
    rep.ok_post(&format!("{mrs}/1/request_changes"), json!({})).await;
    wait_jobs(&srv.state).await;
    assert_eq!(of(&dev.ok_get(inbox).await, "review").len(), 2);
    assert_eq!(mails_to(&srv, "dave", "rita requested changes"), 0);
    dev.ok_put(channels, json!({ "kind": "review", "channel": "web", "enabled": false })).await;
    rep.ok_post(&format!("{mrs}/1/approve"), json!({})).await;
    wait_jobs(&srv.state).await;
    assert_eq!(of(&dev.ok_get(inbox).await, "review").len(), 2);
    assert_eq!(mails_to(&srv, "dave", "rita approved merge request !1"), 1);

    // the other kinds are untouched: a comment is activity, a mention a mention
    owner.ok_post(&format!("{mrs}/1/notes"), json!({ "body": "Looks fine @dave" })).await;
    wait_jobs(&srv.state).await;
    let mine = dev.ok_get(inbox).await;
    assert_eq!(of(&mine, "mention")[0]["summary"], "mentioned you", "{mine}");
    assert_eq!(mails_to(&srv, "dave", "mentioned you"), 1);
    assert_eq!(dev.ok_post(&format!("{inbox}/read"), json!({})).await["unread"], 0);

    // a pipeline that fails tells who started it and the author of its merge request
    std::fs::write(work.join(".gitlab-ci.yml"), "job:\n  script: x\n  secrets:\n    DB: {vault: db/pw}\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-qm", "ci"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "feature"]).await);
    wait_jobs(&srv.state).await;
    for who in [&owner, &dev] {
        let got = who.ok_get(inbox).await;
        let failed = of(&got, "pipeline_failed");
        // the branch's pipeline and the merge request's: a broken config fails both
        assert_eq!(failed.len(), 2, "{got}");
        assert_eq!((failed[0]["target_type"].as_str(), failed[0]["summary"].as_str()), (Some("pipeline"), Some("failed")), "{got}");
        assert!(failed[0]["title"].as_str().unwrap().ends_with("on feature"), "{got}");
    }
    assert_eq!(mails_to(&srv, "olivia", "failed on feature"), 2);
    assert!(of(&rep.ok_get(inbox).await, "pipeline_failed").is_empty(), "rita has nothing to do with it");

    // a level of disabled for the project: nothing, in the app either
    dev.put(&format!("/api/v1/projects/{proj}/notification_settings"), json!({ "level": "disabled" })).await;
    owner.ok_post(&format!("{mrs}/1/notes"), json!({ "body": "@dave again" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(of(&dev.ok_get(inbox).await, "mention").len(), 1);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn instance_search_sees_only_what_it_may(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, proj, work) = setup(&srv).await;
    let stranger = srv.user("sam", false).await;
    let guest = srv.user("gus", false).await;
    owner.ok_post(&format!("/api/v1/projects/{proj}/members"), json!({ "username": "gus", "access_level": "guest" })).await;
    // private project: an issue and an MR with a telling title
    owner.ok_post(&format!("/api/v1/projects/{proj}/issues"), json!({ "title": "Secret launch plan" })).await;
    must(git(&work, &["checkout", "-q", "-B", "launch", "main"]).await);
    std::fs::write(work.join("l.txt"), "l\n").unwrap();
    must(git(&work, &["add", "l.txt"]).await);
    must(git(&work, &["commit", "-q", "-m", "l"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "launch"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "launch", "title": "Secret launch code" }))
        .await;
    // a public project with an open issue
    let pubg =
        owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await["id"].as_i64().unwrap();
    let open =
        owner.ok_post("/api/v1/projects", json!({ "name": "Site", "path": "site", "namespace_id": pubg, "visibility": "public" })).await;
    owner.ok_post(&format!("/api/v1/projects/{}/issues", open["id"]), json!({ "title": "Public launch page" })).await;
    owner.ok_post(&format!("/api/v1/projects/{}/issues", open["id"]), json!({ "title": "100% coverage_goal" })).await;

    // members see both; a Reporter the MR too
    let r = rep.ok_get("/api/v1/search?q=launch").await;
    assert_eq!(r["counts"]["issues"], 2, "{r}");
    assert_eq!(r["counts"]["merge_requests"], 1);
    assert_eq!(r["merge_requests"][0]["title"], "Secret launch code");
    // a Guest sees the private issue, not the code (no MR, not even counted)
    let g = guest.ok_get("/api/v1/search?q=launch").await;
    assert_eq!(g["counts"]["issues"], 2);
    assert_eq!(g["counts"]["merge_requests"], 0, "{g}");
    assert_eq!(g["merge_requests"], json!([]));
    // a stranger and anonymous people: the public project only
    for c in [&stranger, &Client::new(&srv.base)] {
        let s = c.ok_get("/api/v1/search?q=launch").await;
        assert_eq!(s["counts"]["issues"], 1, "{s}");
        assert_eq!(s["issues"][0]["title"], "Public launch page");
        assert_eq!(s["counts"]["merge_requests"], 0);
        assert!(!s.to_string().contains("Secret"));
        assert_eq!(s["counts"]["projects"], 0);
    }
    assert_eq!(stranger.ok_get("/api/v1/search?q=site&scope=projects").await["projects"][0]["full_path"], "open/site");
    // LIKE characters match themselves
    assert_eq!(stranger.ok_get("/api/v1/search?q=100%25").await["counts"]["issues"], 1);
    assert_eq!(stranger.ok_get("/api/v1/search?q=e_g").await["counts"]["issues"], 1);
    assert_eq!(stranger.ok_get("/api/v1/search?q=%25").await["counts"]["issues"], 1);
    assert_eq!(stranger.ok_get("/api/v1/search?q=_").await["counts"]["issues"], 1);
    // paging one scope, states
    let p = rep.ok_get("/api/v1/search?q=launch&scope=issues&state=opened&per_page=1&page=2").await;
    assert_eq!(p["issues"].as_array().unwrap().len(), 1);
    assert_eq!(p["projects"], json!([]));
    assert_eq!(rep.get("/api/v1/search?q=").await.0, 400);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn protected_tags(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, _rep, proj, work) = setup(&srv).await;
    let repo = format!("/api/v1/projects/{proj}/repository");
    let rels = format!("/api/v1/projects/{proj}/releases");
    assert_eq!(dev.post(&format!("/api/v1/projects/{proj}/protected_tags"), json!({ "name": "v*" })).await.0, 403);
    let list = owner
        .ok_post(&format!("/api/v1/projects/{proj}/protected_tags"), json!({ "name": "v*", "create_access_level": "maintainer" }))
        .await;
    assert_eq!(list[0]["create_access_level"], "maintainer");

    // creating: Maintainers only for v*, anyone who pushes for the rest
    let (code, body) = dev.post(&format!("{repo}/tags"), json!({ "tag_name": "v1.0", "ref": "main" })).await;
    assert_eq!(code, 403, "{body}");
    dev.ok_post(&format!("{repo}/tags"), json!({ "tag_name": "nightly", "ref": "main" })).await;
    owner.ok_post(&format!("{repo}/tags"), json!({ "tag_name": "v1.0", "ref": "main" })).await;
    // a release on a new protected tag creates the tag: refused for a Developer
    assert_eq!(dev.post(&rels, json!({ "tag_name": "v2.0", "ref": "main" })).await.0, 403);
    // on the existing one it is fine (no tag is written)
    dev.ok_post(&rels, json!({ "tag_name": "v1.0", "description": "First" })).await;

    // pushes: no creating (Developer), no moving or deleting (anyone)
    let url = |u: &str| srv.http_url(u, REPO);
    must(git(&work, &["tag", "v9"]).await);
    let out = git(&work, &["push", &url("dave"), "v9"]).await;
    assert!(!out.ok && out.stderr.contains("protected tag v9"), "{}", out.stderr);
    must(git(&work, &["fetch", "-q", &url("olivia"), "+refs/tags/*:refs/tags/*"]).await);
    let out = git(&work, &["push", &url("olivia"), ":refs/tags/v1.0"]).await;
    assert!(!out.ok && out.stderr.contains("cannot delete the protected tag"), "{}", out.stderr);
    must(git(&work, &["commit", "-q", "--allow-empty", "-m", "moved"]).await);
    must(git(&work, &["tag", "-f", "v1.0"]).await);
    let out = git(&work, &["push", "-f", &url("olivia"), "v1.0"]).await;
    assert!(!out.ok && out.stderr.contains("cannot update the protected tag"), "{}", out.stderr);
    // unprotected tags behave as before
    must(git(&work, &["tag", "other"]).await);
    must(git(&work, &["push", "-q", &url("dave"), "other"]).await);

    // deleting in Forgust: Maintainers only, and the release goes with it
    assert_eq!(dev.delete(&format!("{repo}/tags/v1.0")).await.0, 403);
    assert_eq!(owner.delete(&format!("{repo}/tags/v1.0")).await.0, 204);
    wait_jobs(&srv.state).await;
    assert_eq!(owner.get(&format!("{rels}/v1.0")).await.0, 404);

    // /api/v4, as GitLab
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4 = Client::with_token(&srv.base, &tok);
    let l = v4.ok_get(&format!("/api/v4/projects/{proj}/protected_tags")).await;
    assert_eq!(l[0]["create_access_levels"][0]["access_level"], 40);
    assert_eq!(v4.delete(&format!("/api/v4/projects/{proj}/protected_tags/v*")).await.0, 204);
    dev.ok_post(&format!("{repo}/tags"), json!({ "tag_name": "v3.0", "ref": "main" })).await;
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn upload_limits_and_storage(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| {
        c.limits.max_upload_bytes = 2 << 20;
        c.limits.max_upload_bytes_non_member = 1 << 20;
        c.limits.uploads_per_hour = 3;
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let visitor = srv.user("vic", false).await;
    let g =
        owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await["id"].as_i64().unwrap();
    let proj =
        owner.ok_post("/api/v1/projects", json!({ "name": "Site", "path": "site", "namespace_id": g, "visibility": "public" })).await["id"]
            .as_i64()
            .unwrap();
    let up = format!("/api/v1/projects/{proj}/uploads");
    let mb = |n: usize| vec![7u8; n << 20];

    // a visitor of a public project gets the smaller size; a member the full one
    assert_eq!(visitor.upload(&up, "big.bin", &[mb(1), vec![1]].concat()).await.0, 413);
    assert_eq!(visitor.upload(&up, "small.bin", &mb(0)).await.0, 201);
    assert_eq!(owner.upload(&up, "big.bin", &mb(1)).await.0, 201);
    assert_eq!(owner.upload(&up, "huge.bin", &[mb(2), vec![1]].concat()).await.0, 413);
    // so many files an hour, then 429
    assert_eq!(owner.upload(&up, "two.txt", b"2").await.0, 201);
    assert_eq!(owner.upload(&up, "three.txt", b"3").await.0, 201);
    let (code, body) = owner.upload(&up, "four.txt", b"4").await;
    assert_eq!(code, 429, "{body}");

    // what the project stores, for its Maintainers
    let s = owner.ok_get(&format!("/api/v1/projects/{proj}/storage")).await;
    assert_eq!(s["uploads_count"], 4);
    assert_eq!(s["uploads_bytes"], (1 << 20) + 2);
    assert_eq!(visitor.get(&format!("/api/v1/projects/{proj}/storage")).await.0, 403);
}

/// Pushes write branches and tags only, a tag cannot take a branch's name (or
/// the reverse), and what Forgust reads ignores refs/replace and prefers branches.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn only_branches_and_tags_are_pushed(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, _rep, proj, work) = setup(&srv).await;
    let url = srv.http_url("dave", REPO);
    let main = must(git(&work, &["rev-parse", "main"]).await).trim().to_string();
    let readme = must(git(&work, &["rev-parse", "main:README.md"]).await).trim().to_string();

    // an evil README on a side branch, then refs/replace/<main's README> pointing at it
    must(git(&work, &["checkout", "-q", "-b", "side"]).await);
    commit_file(&work, "README.md", "evil\n", "evil").await;
    let evil = must(git(&work, &["rev-parse", "side:README.md"]).await).trim().to_string();
    must(git(&work, &["push", "-q", &url, "side"]).await);
    let out = git(&work, &["push", &url, &format!("{evil}:refs/replace/{readme}")]).await;
    assert!(!out.ok && out.stderr.contains("only branches and tags"), "{}", out.stderr);
    let out = git(&work, &["push", &url, "side:refs/notes/commits"]).await;
    assert!(!out.ok && out.stderr.contains("only branches and tags"), "{}", out.stderr);

    // a tag named like a branch, by API or push, and a branch named like a tag
    let repo = format!("/api/v1/projects/{proj}/repository");
    let (code, body) = dev.post(&format!("{repo}/tags"), json!({ "tag_name": "main", "ref": "side" })).await;
    assert_eq!(code, 403, "{body}");
    let out = git(&work, &["push", &url, "side:refs/tags/main"]).await;
    assert!(!out.ok && out.stderr.contains("already uses that name"), "{}", out.stderr);
    dev.ok_post(&format!("{repo}/tags"), json!({ "tag_name": "nightly", "ref": "side" })).await;
    let (code, body) = dev.post(&format!("{repo}/branches"), json!({ "branch": "nightly", "ref": "main" })).await;
    assert_eq!(code, 403, "{body}");

    // refs written behind Forgust's back (older versions allowed both) change nothing it reads
    srv.state.storage.update_ref(proj, &format!("refs/replace/{readme}"), &evil, None).await.unwrap();
    srv.state.storage.update_ref(proj, "refs/tags/main", must(git(&work, &["rev-parse", "side"]).await).trim(), None).await.unwrap();
    let commit = owner.ok_get(&format!("/api/v4/projects/{proj}/repository/commits/main")).await;
    assert_eq!(commit["id"], main.as_str());
    let (code, raw) = owner.get_bytes(&format!("/api/v4/projects/{proj}/repository/files/README.md/raw?ref=main")).await;
    assert_eq!(code, 200);
    assert!(String::from_utf8_lossy(&raw).starts_with("# Demo"), "{}", String::from_utf8_lossy(&raw));
    // blame runs a fresh git process (the long-lived readers loaded refs before the plant)
    let blame = owner.ok_get(&format!("/api/v1/projects/{proj}/repository/blame?ref=main&path=README.md")).await;
    let lines: Vec<&str> =
        blame["hunks"].as_array().unwrap().iter().flat_map(|h| h["lines"].as_array().unwrap()).map(|l| l.as_str().unwrap()).collect();
    assert_eq!(lines.first(), Some(&"# Demo"), "{lines:?}");
    // and a reader opened after the plant (gix, then cat-file), as the CI config is read
    use forgust_storage::Storage as _;
    let fresh = forgust_server::app::storage_for(&srv.state.cfg);
    let blob = fresh.blob(proj, &main, "README.md", 1 << 20).await.unwrap();
    assert!(String::from_utf8_lossy(&blob.data).starts_with("# Demo"), "{}", String::from_utf8_lossy(&blob.data));
    let by_id = fresh.blob_by_id(proj, &readme, 1 << 20).await.unwrap();
    assert!(String::from_utf8_lossy(&by_id).starts_with("# Demo"));
}

/// Internal projects are not for external users or bots, in search and
/// contributions too; a Guest does not learn merge request titles.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn internal_and_guest_visibility(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, _rep, proj, work) = setup(&srv).await;
    let ext = srv.user("ext", false).await;
    let plain = srv.user("plain", false).await;
    let guest = srv.user("gina", false).await;
    sqlx::query("UPDATE users SET is_external = true WHERE username = 'ext'").execute(&srv.state.db).await.unwrap();
    // (straight in the table: the groups above are private)
    sqlx::query("UPDATE projects SET visibility = 10 WHERE id = $1").bind(proj).execute(&srv.state.db).await.unwrap();
    owner.ok_post(&format!("/api/v1/projects/{proj}/issues"), json!({ "title": "Quarterly secret plan" })).await;
    let found = |c: &Client| {
        let c = c.clone();
        async move { c.ok_get("/api/v1/search?q=secret%20plan&scope=issues").await["counts"]["issues"].as_i64().unwrap() }
    };
    assert_eq!(found(&plain).await, 1);
    assert_eq!(found(&ext).await, 0);
    let contrib = ext.ok_get("/api/v1/users/olivia/contributions").await;
    assert_eq!(contrib["activity"].as_array().unwrap().len(), 0, "{contrib}");

    // back to private, with a Guest: `!1` in an issue does not show the MR's title to them
    sqlx::query("UPDATE projects SET visibility = 0 WHERE id = $1").bind(proj).execute(&srv.state.db).await.unwrap();
    owner.ok_post(&format!("/api/v1/projects/{proj}/members"), json!({ "username": "gina", "access_level": "guest" })).await;
    push_branch(&srv, &work, "topic", "t.txt").await;
    dev.ok_post(&format!("/api/v1/projects/{proj}/merge_requests"), json!({ "source_branch": "topic", "title": "Hidden acquisition" }))
        .await;
    let issue =
        owner.ok_post(&format!("/api/v1/projects/{proj}/issues"), json!({ "title": "See", "description": "see !1 and @gina" })).await;
    let path = format!("/api/v1/projects/{proj}/issues/{}", issue["iid"]);
    assert!(dev.ok_get(&path).await["description_html"].as_str().unwrap().contains("Hidden acquisition"));
    assert!(!guest.ok_get(&path).await["description_html"].as_str().unwrap().contains("Hidden acquisition"));
}

/// Over git HTTP an anonymous caller cannot tell a private project from a missing one.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn git_http_hides_private_paths(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (_owner, _dev, _rep, _proj, _work) = setup(&srv).await;
    let status = |path: &str| {
        let url = format!("{}/{path}.git/info/refs?service=git-upload-pack", srv.base);
        async move { reqwest::get(url).await.unwrap().status().as_u16() }
    };
    assert_eq!(status(REPO).await, 401);
    assert_eq!(status("acme/platform/nope").await, 401);
    assert_eq!(status("nobody/nothing").await, 401);
}
