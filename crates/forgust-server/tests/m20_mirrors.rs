//! Repository mirrors: a project pulled from another repository (read-only
//! here: every way to change its branches and tags is refused, the rest of the
//! project stays open), and a project pushed to another one after every change.
//! The "other forge" is a project on the test server itself.

mod common;

use common::*;
use serde_json::{Value, json};

async fn token(c: &Client, scopes: &[&str]) -> String {
    c.ok_post("/api/v1/user/tokens", json!({ "name": "mirror", "scopes": scopes })).await["token"].as_str().unwrap().to_string()
}

fn branches(list: &Value) -> Vec<String> {
    let mut v: Vec<String> = list.as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap().to_string()).collect();
    v.sort();
    v
}

async fn mirror(owner: &Client, p: i64) -> Value {
    owner.ok_get(&format!("/api/v1/projects/{p}/mirrors")).await[0].clone()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_pull_mirror_follows_its_source_and_is_read_only(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Acme", "path": "acme" })).await;
    let ns = org["id"].clone();
    owner.ok_post("/api/v1/projects", json!({ "name": "Upstream", "path": "upstream", "namespace_id": ns })).await;
    let copy =
        owner.ok_post("/api/v1/projects", json!({ "name": "Copy", "path": "copy", "namespace_id": ns })).await["id"].as_i64().unwrap();
    owner.ok_post(&format!("/api/v1/projects/{copy}/members"), json!({ "username": "dave", "access_level": "developer" })).await;

    // the source: main, topic and a tag
    let work = srv.work_dir("upstream");
    seed_repo(&work).await;
    let up = srv.http_url("olivia", "acme/upstream");
    must(git(&work, &["push", "-q", &up, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "t.txt", "t\n", "topic").await;
    must(git(&work, &["tag", "v1"]).await);
    must(git(&work, &["push", "-q", &up, "topic", "v1"]).await);
    must(git(&work, &["checkout", "-q", "main"]).await);
    // the copy has a branch of its own, which the mirror replaces
    let mine = srv.work_dir("mine");
    must(git(&mine, &["init", "-q", "-b", "local"]).await);
    commit_file(&mine, "l.txt", "l\n", "local").await;
    must(git(&mine, &["push", "-q", &srv.http_url("olivia", "acme/copy"), "local"]).await);
    wait_jobs(&srv.state).await;

    // only Maintainers manage mirrors; the address gets the SSRF check
    let base = format!("/api/v1/projects/{copy}");
    let read = token(&owner, &["read_repository"]).await;
    let pull = json!({ "direction": "pull", "url": format!("{}/acme/upstream.git", srv.base), "username": "olivia", "token": read });
    assert_eq!(dev.post(&format!("{base}/mirrors"), pull.clone()).await.0, 403);
    let creds =
        owner.post(&format!("{base}/mirrors"), json!({ "direction": "pull", "url": srv.http_url("olivia", "acme/upstream") })).await;
    assert_eq!(creds.0, 400, "credentials go in the token field, not the address: {}", creds.1);
    let listed = owner.ok_post(&format!("{base}/mirrors"), pull.clone()).await;
    assert_eq!(listed[0]["has_token"], true);
    assert!(!listed.to_string().contains(&read), "the token is never shown");
    assert_eq!(owner.post(&format!("{base}/mirrors"), pull).await.0, 409, "one pull mirror");
    wait_jobs(&srv.state).await;

    let m = mirror(&owner, copy).await;
    assert!(m["last_error"].is_null() && m["last_success_at"].is_string(), "{m}");
    assert_eq!(branches(&owner.ok_get(&format!("{base}/repository/branches")).await), ["main", "topic"], "the source's, not `local`");
    assert_eq!(owner.ok_get(&format!("{base}/repository/tags")).await[0]["name"], "v1");
    assert!(srv.state.storage.refs(copy, "refs/mirror/").await.unwrap().is_empty(), "nothing left in staging");

    // the source moves on: a new commit, a branch gone
    commit_file(&work, "n.txt", "n\n", "next").await;
    let head = must(git(&work, &["rev-parse", "HEAD"]).await).trim().to_string();
    must(git(&work, &["push", "-q", &up, "main", ":topic"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("{base}/mirrors/{}/update", m["id"]), json!({})).await;
    wait_jobs(&srv.state).await;
    let after = owner.ok_get(&format!("{base}/repository/branches")).await;
    assert_eq!(branches(&after), ["main"]);
    assert_eq!(after[0]["commit"]["id"].as_str().or(after[0]["sha"].as_str()), Some(head.as_str()), "{after}");

    // every write to its branches and tags: refused, whoever tries
    let writes: Vec<(&Client, String, Value)> = vec![
        (&dev, format!("{base}/repository/branches"), json!({ "branch": "new", "ref": "main" })),
        (&dev, format!("{base}/repository/tags"), json!({ "tag_name": "v2", "ref": "main" })),
        (&owner, format!("{base}/releases"), json!({ "tag_name": "v3", "ref": "main" })),
        (
            &dev,
            format!("{base}/repository/commits"),
            json!({ "branch": "main", "commit_message": "web edit", "actions": [{ "action": "create", "file_path": "w.txt", "content": "w" }] }),
        ),
    ];
    for (who, path, body) in writes {
        let (s, b) = who.post(&path, body).await;
        assert_eq!(s, 403, "POST {path} should be refused: {b}");
        assert!(b.to_string().contains("pull mirror"), "POST {path}: {b}");
    }
    for path in [format!("{base}/repository/branches/main"), format!("{base}/repository/tags/v1")] {
        let (s, b) = dev.delete(&path).await;
        assert_eq!(s, 403, "DELETE {path}: {b}");
    }
    commit_file(&mine, "late.txt", "late\n", "late").await;
    let pushed = git(&mine, &["push", "-q", &srv.http_url("olivia", "acme/copy"), "local:refs/heads/late"]).await;
    assert!(!pushed.ok && pushed.stderr.contains("pull mirror"), "{}", pushed.stderr);
    // the rest of the project is open: issues and comments
    let issue = dev.ok_post(&format!("{base}/issues"), json!({ "title": "Found a bug" })).await;
    dev.ok_post(&format!("{base}/issues/{}/notes", issue["iid"]), json!({ "body": "here" })).await;

    // removed: the repository is writable again
    let (s, _) = owner.delete(&format!("{base}/mirrors/{}", m["id"])).await;
    assert_eq!(s, 200);
    must(git(&mine, &["push", "-q", &srv.http_url("olivia", "acme/copy"), "local:refs/heads/late"]).await);
    for e in ["mirror_added", "mirror_removed"] {
        assert_eq!(srv.audit_events(e).await.len(), 1, "{e}");
    }
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_pull_mirror_blocks_merges(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let (_, _, p) = org_project(&owner).await;
    let work = srv.work_dir("m");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "t.txt", "t\n", "topic").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&srv.state).await;
    let base = format!("/api/v1/projects/{p}");
    owner.ok_post(&format!("{base}/merge_requests"), json!({ "source_branch": "topic", "title": "Topic" })).await;
    // pulled from itself: the branches stay as they are
    let read = token(&owner, &["read_repository"]).await;
    owner
        .ok_post(
            &format!("{base}/mirrors"),
            json!({ "direction": "pull", "url": format!("{}/acme/platform/api.git", srv.base), "username": "olivia", "token": read }),
        )
        .await;
    wait_jobs(&srv.state).await;
    let status = owner.ok_get(&format!("{base}/merge_requests/1/merge_status")).await;
    assert!(status["blockers"].to_string().contains("pull mirror"), "{status}");
    let (s, b) = owner.post(&format!("{base}/merge_requests/1/merge"), json!({})).await;
    assert!(s == 403 || s == 409, "{s} {b}");
    for path in ["rebase", "stack/rebase"] {
        let (s, b) = owner.post(&format!("{base}/merge_requests/1/{path}"), json!({})).await;
        assert!(s == 403 || s == 409, "{path}: {s} {b}");
    }
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_push_mirror_sends_every_change(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.features.imports_allow_local = true).await;
    let owner = srv.user("olivia", false).await;
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Acme", "path": "acme" })).await;
    let src =
        owner.ok_post("/api/v1/projects", json!({ "name": "Src", "path": "src", "namespace_id": org["id"] })).await["id"].as_i64().unwrap();
    let dst =
        owner.ok_post("/api/v1/projects", json!({ "name": "Dst", "path": "dst", "namespace_id": org["id"] })).await["id"].as_i64().unwrap();
    let write = token(&owner, &["write_repository"]).await;
    let base = format!("/api/v1/projects/{src}");
    let target = format!("{}/acme/dst.git", srv.base);
    owner.ok_post(&format!("{base}/mirrors"), json!({ "direction": "push", "url": target, "token": write })).await;

    let work = srv.work_dir("src");
    seed_repo(&work).await;
    let url = srv.http_url("olivia", "acme/src");
    must(git(&work, &["push", "-q", &url, "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "feature"]).await);
    commit_file(&work, "f.txt", "f\n", "feature").await;
    must(git(&work, &["push", "-q", &url, "feature"]).await);
    wait_jobs(&srv.state).await;
    assert_eq!(branches(&owner.ok_get(&format!("/api/v1/projects/{dst}/repository/branches")).await), ["feature", "main"]);
    assert!(mirror(&owner, src).await["last_error"].is_null());

    // a web commit and a deleted branch reach it too (pruned there)
    owner
        .ok_post(
            &format!("{base}/repository/commits"),
            json!({ "branch": "main", "commit_message": "web", "actions": [{ "action": "create", "file_path": "w.txt", "content": "w" }] }),
        )
        .await;
    owner.delete(&format!("{base}/repository/branches/feature")).await;
    wait_jobs(&srv.state).await;
    assert_eq!(branches(&owner.ok_get(&format!("/api/v1/projects/{dst}/repository/branches")).await), ["main"]);
    let here = srv.state.storage.resolve(src, "refs/heads/main").await.unwrap();
    assert_eq!(srv.state.storage.resolve(dst, "refs/heads/main").await.unwrap(), here);

    // protected branches only: other branches stay here, nothing is deleted there
    let m = mirror(&owner, src).await;
    owner.ok_put(&format!("{base}/mirrors/{}", m["id"]), json!({ "only_protected": true })).await;
    must(git(&work, &["push", "-q", &url, "feature"]).await);
    wait_jobs(&srv.state).await;
    assert_eq!(branches(&owner.ok_get(&format!("/api/v1/projects/{dst}/repository/branches")).await), ["main"]);

    // a token that no longer works: the failure is on the mirror, the push here went through
    owner.ok_put(&format!("{base}/mirrors/{}", m["id"]), json!({ "token": "wrong" })).await;
    wait_jobs(&srv.state).await;
    let failed = mirror(&owner, src).await;
    assert!(failed["last_error"].is_string(), "{failed}");
    assert_eq!(srv.audit_events("mirror_changed").await.len(), 2);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn mirrors_refuse_internal_addresses_by_default(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_, _, p) = org_project(&owner).await;
    for url in ["http://127.0.0.1:9/a.git", "file:///etc/passwd", "ssh://git@example.com/a.git"] {
        let (s, b) = owner.post(&format!("/api/v1/projects/{p}/mirrors"), json!({ "direction": "push", "url": url })).await;
        assert_eq!(s, 400, "{url}: {b}");
    }
}
