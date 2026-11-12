//! Storage quotas per project and per top-level namespace. Every way to
//! add bytes stops at the quota (uploads, git push, LFS, registry; artifacts in
//! m7_ci); reading, deleting and pushes without new objects stay open.

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

async fn used(srv: &TestServer, project: i64) -> i64 {
    forgust_db::quotas::project_used(&srv.state.db, project).await.unwrap()
}

/// Content git cannot compress much.
fn noise(len: usize, seed: u64) -> String {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            char::from(b'a' + ((x >> 33) % 26) as u8)
        })
        .collect()
}

async fn lfs_batch(srv: &TestServer, user: &str, size: i64) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/{REPO}.git/info/lfs/objects/batch", srv.base))
        .basic_auth(user, Some(password(user)))
        .header("accept", "application/vnd.git-lfs+json")
        .json(&json!({ "operation": "upload", "objects": [{ "oid": "e".repeat(64), "size": size }] }))
        .send()
        .await
        .unwrap()
}

async fn registry_upload(srv: &TestServer, user: &str) -> (StatusCode, Value) {
    let t: Value = reqwest::Client::new()
        .get(format!("{}/jwt/auth", srv.base))
        .query(&[("service", "container_registry"), ("scope", &format!("repository:{REPO}:pull,push"))])
        .basic_auth(user, Some(password(user)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = reqwest::Client::new()
        .post(format!("{}/v2/{REPO}/blobs/uploads/", srv.base))
        .bearer_auth(t["token"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    (r.status(), r.json().await.unwrap_or(Value::Null))
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_full_project_takes_nothing_new(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    let work = srv.work_dir("q");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    let base = format!("/api/v1/projects/{p}");
    let quota = format!("/api/v1/admin/projects/{p}/quota");

    // administrators set quotas
    assert_eq!(owner.put(&quota, json!({ "bytes": 10 })).await.0, 403);
    let u0 = used(&srv, p).await;
    assert!(u0 > 0, "the repository counts");
    admin.ok_put(&quota, json!({ "bytes": u0 + 1000 })).await;

    // uploads: up to what is left
    let (s, b) = owner.upload(&format!("{base}/uploads"), "a.bin", &[b'a'; 600]).await;
    assert!(s < 300, "{s} {b}");
    let (s, b) = owner.upload(&format!("{base}/uploads"), "b.bin", &[b'b'; 600]).await;
    assert_eq!(s, 413, "600 + 600 > 1000: {b}");
    assert!(b.to_string().contains("Storage quota reached: this project"), "{b}");
    let st = owner.ok_get(&format!("{base}/storage")).await;
    assert_eq!((st["total_bytes"].as_i64(), st["quota"]["quota"].as_i64()), (Some(u0 + 600), Some(u0 + 1000)), "{st}");
    assert!(st["namespace"].is_null(), "no namespace quota: {st}");

    // a push with new objects that do not fit: refused, with the reason
    commit_file(&work, "big.txt", &noise(20_000, 1), "big").await;
    let pushed = git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await;
    assert!(!pushed.ok && pushed.stderr.contains("Storage quota"), "{}", pushed.stderr);
    must(git(&work, &["reset", "-q", "--hard", "HEAD~1"]).await);

    // full now
    admin.ok_put(&quota, json!({ "bytes": used(&srv, p).await })).await;
    let (s, b) = owner.upload(&format!("{base}/uploads"), "c.bin", b"c").await;
    assert_eq!(s, 413, "{b}");
    let r = lfs_batch(&srv, "olivia", 10).await;
    assert_eq!(r.status(), StatusCode::INSUFFICIENT_STORAGE);
    assert!(r.text().await.unwrap().contains("Storage quota"));
    let (s, b) = registry_upload(&srv, "olivia").await;
    assert_eq!((s, b["errors"][0]["code"].as_str()), (StatusCode::FORBIDDEN, Some("DENIED")), "{b}");
    assert!(b.to_string().contains("Storage quota"), "{b}");
    // what brings no new objects still goes through: a branch, then deleting it
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main:refs/heads/copy"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), ":copy"]).await);
    // reading and cloning too
    owner.ok_get(&format!("{base}/repository/tree")).await;
    must(git(&srv.work_dir("clone"), &["clone", "-q", &srv.http_url("olivia", REPO), "."]).await);

    // measured: a banner for who can push, and one email to the Owners
    srv.state.mailer.sent();
    forgust_server::services::quotas::measure(&srv.state).await.unwrap();
    let shown = owner.ok_get(&base).await;
    assert_eq!(shown["storage_warning"], json!({ "scope": "project", "namespace": null, "level": 100 }), "{shown}");
    let mails = |srv: &TestServer| {
        srv.state
            .mailer
            .sent()
            .into_iter()
            .filter(|m| m.subject == "Storage quota reached" && m.to == vec!["olivia@example.com".to_string()])
            .count()
    };
    assert_eq!(mails(&srv), 1);
    forgust_server::services::quotas::measure(&srv.state).await.unwrap();
    assert_eq!(mails(&srv), 1, "once per crossing");

    // no quota any more: writable again, the banner goes
    admin.ok_put(&quota, json!({ "bytes": null })).await;
    let (s, b) = owner.upload(&format!("{base}/uploads"), "d.bin", b"d").await;
    assert!(s < 300, "{s} {b}");
    assert!(owner.ok_get(&base).await["storage_warning"].is_null());
    let audit = admin.ok_get("/api/v1/admin/audit").await.to_string();
    assert!(audit.contains("storage_quota_changed"), "audited");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_namespace_quota_counts_every_project(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let (org, sub, p) = org_project(&owner).await;
    let other =
        owner.ok_post("/api/v1/projects", json!({ "namespace_id": org, "name": "Web", "path": "web" })).await["id"].as_i64().unwrap();

    // the instance default applies to the organization as a whole
    let ns_used = forgust_db::quotas::namespace_used(&srv.state.db, org).await.unwrap();
    admin.ok_put("/api/v1/admin/storage/quota", json!({ "bytes": ns_used + 500 })).await;
    let (s, b) = owner.upload(&format!("/api/v1/projects/{p}/uploads"), "a.bin", &[b'a'; 400]).await;
    assert!(s < 300, "{s} {b}");
    let (s, b) = owner.upload(&format!("/api/v1/projects/{other}/uploads"), "b.bin", &[b'b'; 400]).await;
    assert_eq!(s, 413, "{b}");
    assert!(b.to_string().contains("the group acme"), "{b}");
    let g = owner.ok_get(&format!("/api/v1/groups/{sub}/storage")).await;
    assert_eq!(
        (g["namespace"].as_str(), g["used"].as_i64(), g["quota"].as_i64()),
        (Some("acme"), Some(ns_used + 400), Some(ns_used + 500))
    );
    let st = owner.ok_get(&format!("/api/v1/projects/{other}/storage")).await;
    assert_eq!(st["namespace"]["name"], "acme", "{st}");
    let a = admin.ok_get("/api/v1/admin/storage").await;
    assert_eq!(a["default_quota"], ns_used + 500);
    let row = a["namespaces"].as_array().unwrap().iter().find(|n| n["full_path"] == "acme").unwrap().clone();
    assert_eq!((row["quota"].as_i64(), row["own_quota"].as_i64()), (Some(ns_used + 500), None), "{row}");

    // measured: every project of it shows the namespace's banner
    forgust_server::services::quotas::measure(&srv.state).await.unwrap();
    let w = owner.ok_get(&format!("/api/v1/projects/{other}")).await["storage_warning"].clone();
    assert_eq!((w["scope"].as_str(), w["namespace"].as_str(), w["level"].as_i64()), (Some("namespace"), Some("acme"), Some(80)), "{w}");

    // its own setting wins: 0 is none; a subgroup has none of its own
    let set = |id: i64| format!("/api/v1/admin/namespaces/{id}/quota");
    assert_eq!(admin.put(&set(sub), json!({ "bytes": 1 })).await.0, 400);
    assert_eq!(admin.put(&set(org), json!({ "bytes": -1 })).await.0, 400);
    admin.ok_put(&set(org), json!({ "bytes": 0 })).await;
    let (s, b) = owner.upload(&format!("/api/v1/projects/{other}/uploads"), "c.bin", &[b'c'; 400]).await;
    assert!(s < 300, "{s} {b}");
    assert!(owner.ok_get(&format!("/api/v1/groups/{org}/storage")).await["quota"].is_null());
}
