//! Against real backing services (compose.dev.yml): S3 for the blob store
//! (maxio) and SMTP for email (mailpit). Each test skips when its service is
//! not reachable, except in CI (`CI` set), where that is a failure.
//!
//! | Variable | Default |
//! | --- | --- |
//! | `FORGUST_TEST_S3_ENDPOINT` | `http://127.0.0.1:9010` |
//! | `FORGUST_TEST_S3_BUCKET` / `_ACCESS_KEY` / `_SECRET_KEY` | `forgust` / `forgust` / `forgust-secret` |
//! | `FORGUST_TEST_SMTP_URL` | `smtp://127.0.0.1:1026` |
//! | `FORGUST_TEST_MAILPIT_API` | `http://127.0.0.1:8026/api/v1` |

mod common;

use std::time::Duration;

use bytes::Bytes;
use common::*;
use forgust_server::blob::{BlobStore, ObjectStoreBlobs};
use forgust_server::config::Config;
use futures::StreamExt;
use serde_json::{Value, json};

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Whether the service answers; outside CI a missing service skips the test.
async fn available(name: &str, url: &str) -> bool {
    let up = reqwest::Client::builder().timeout(Duration::from_secs(3)).build().unwrap().get(url).send().await.is_ok();
    if !up {
        if std::env::var_os("CI").is_some() {
            panic!("{name} is not reachable at {url} (start it: docker compose -f compose.dev.yml up -d)");
        }
        eprintln!("skipping: {name} not reachable at {url} (docker compose -f compose.dev.yml up -d)");
    }
    up
}

fn s3_config(c: &mut Config) {
    c.blobs.backend = "s3".into();
    c.blobs.s3_endpoint = env("FORGUST_TEST_S3_ENDPOINT", "http://127.0.0.1:9010");
    c.blobs.s3_bucket = env("FORGUST_TEST_S3_BUCKET", "forgust");
    c.blobs.s3_access_key_id = env("FORGUST_TEST_S3_ACCESS_KEY", "forgust");
    c.blobs.s3_secret_access_key = env("FORGUST_TEST_S3_SECRET_KEY", "forgust-secret");
    c.blobs.s3_allow_http = true;
}

fn unique(prefix: &str) -> String {
    format!("{prefix}{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
}

#[tokio::test]
async fn blob_store_on_s3() {
    if !available("S3", &env("FORGUST_TEST_S3_ENDPOINT", "http://127.0.0.1:9010")).await {
        return;
    }
    let mut cfg = Config::default();
    s3_config(&mut cfg);
    let blobs = ObjectStoreBlobs::from_config(&cfg).unwrap();
    assert_eq!(blobs.backend(), "s3");
    let key = format!("tests/{}", unique("obj-"));

    // streamed (multipart) upload of more than one part
    let data: Vec<u8> = (0..12_000_000u32).map(|i| (i % 253) as u8).collect();
    let chunks: Vec<std::io::Result<Bytes>> = data.chunks(1 << 20).map(|c| Ok(Bytes::copy_from_slice(c))).collect();
    let n = blobs.put_stream(&key, futures::stream::iter(chunks).boxed()).await.unwrap();
    assert_eq!(n, data.len() as u64);
    assert_eq!(blobs.size(&key).await.unwrap(), Some(data.len() as u64));
    let (size, mut s) = blobs.get(&key).await.unwrap().unwrap();
    assert_eq!(size, data.len() as u64);
    let mut got = Vec::new();
    while let Some(c) = s.next().await {
        got.extend_from_slice(&c.unwrap());
    }
    assert_eq!(got, data);

    // ranged reads (artifact browsing reads zip directories this way)
    assert_eq!(blobs.get_range(&key, 1_000_000, 1_000_010).await.unwrap().as_ref(), &data[1_000_000..1_000_010]);
    let mut part = Vec::new();
    let mut s = blobs.get_range_stream(&key, 5, 2_000_005).await.unwrap();
    while let Some(c) = s.next().await {
        part.extend_from_slice(&c.unwrap());
    }
    assert_eq!(part, &data[5..2_000_005]);

    // presigned GET works without credentials
    let url = blobs.presigned_get(&key, Duration::from_secs(60)).await.unwrap().expect("S3 signs URLs");
    let r = reqwest::get(url).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap().len(), data.len());

    let moved = format!("{key}-moved");
    blobs.rename(&key, &moved).await.unwrap();
    assert_eq!(blobs.size(&key).await.unwrap(), None);
    blobs.put_bytes(&key, Bytes::from_static(b"small")).await.unwrap();
    assert_eq!(blobs.size(&key).await.unwrap(), Some(5));
    for k in [&key, &moved] {
        blobs.delete(k).await.unwrap();
        assert!(blobs.get(k).await.unwrap().is_none());
    }
}

/// Git LFS with the objects in S3: the push goes through Forgust, downloads
/// through presigned URLs.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn lfs_on_s3(pool: sqlx::PgPool) {
    if !has("git-lfs") {
        eprintln!("skipping: git-lfs not installed");
        return;
    }
    if !available("S3", &env("FORGUST_TEST_S3_ENDPOINT", "http://127.0.0.1:9010")).await {
        return;
    }
    let srv = TestServer::start_with(pool, s3_config).await;
    assert_eq!(srv.state.blobs.backend(), "s3");
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, proj) = org_project(&owner).await;
    let work = srv.work_dir("lfs-s3");
    seed_repo(&work).await;
    must(git(&work, &["lfs", "install", "--local"]).await);
    must(git(&work, &["lfs", "track", "*.bin"]).await);
    // unique content: objects are content-addressed and the bucket is shared
    let mut data: Vec<u8> = unique("lfs-").into_bytes();
    data.extend((0..300_000u32).map(|i| (i * 13 % 251) as u8));
    std::fs::write(work.join("weights.bin"), &data).unwrap();
    must(git(&work, &["add", ".gitattributes", "weights.bin"]).await);
    must(git(&work, &["commit", "-q", "-m", "weights"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;

    let b = owner.ok_get(&format!("/api/v1/projects/{proj}/repository/blob?ref=main&path=weights.bin")).await;
    assert_eq!(b["lfs"]["stored"], true, "{b}");
    let (s, raw) = owner.get_bytes(&format!("/api/v1/projects/{proj}/repository/raw?ref=main&path=weights.bin")).await;
    assert_eq!(s, 200);
    assert_eq!(raw, data);

    let clone = srv.work_dir("lfs-s3-clone");
    must(git(&clone, &["lfs", "install"]).await);
    must(git(&clone, &["clone", "-q", &srv.http_url("olivia", "acme/platform/api"), "c"]).await);
    assert_eq!(std::fs::read(clone.join("c/weights.bin")).unwrap(), data);
}

/// Project uploads in S3: streamed in, served back, gone with the project.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn uploads_on_s3(pool: sqlx::PgPool) {
    if !available("S3", &env("FORGUST_TEST_S3_ENDPOINT", "http://127.0.0.1:9010")).await {
        return;
    }
    let srv = TestServer::start_with(pool, s3_config).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, proj) = org_project(&owner).await;
    let data: Vec<u8> = unique("file-").into_bytes().into_iter().chain((0..3_000_000u32).map(|i| (i % 251) as u8)).collect();
    let (code, up) = owner.upload(&format!("/api/v1/projects/{proj}/uploads"), "build.bin", &data).await;
    assert_eq!(code, 201, "{up}");
    let path = up["full_path"].as_str().unwrap();
    // a download comes straight from S3, presigned, named and not sniffable as a page
    let r = owner.req(reqwest::Method::GET, path).send().await.unwrap();
    assert_eq!(r.status(), 302);
    let location = r.headers()["location"].to_str().unwrap().to_string();
    // the name and type ride in the signature (S3's response-* parameters; the
    // dev server, maxio, ignores them, AWS S3, MinIO and Ceph apply them)
    let u = url::Url::parse(&location).unwrap();
    let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
    assert_eq!(q["response-content-type"], "application/octet-stream");
    assert!(
        q["response-content-disposition"].starts_with("attachment;")
            && q["response-content-disposition"].contains("filename*=UTF-8''build.bin")
    );
    let r = reqwest::get(&location).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap().as_ref(), &data[..]);
    // still for those who may see the project only
    assert!(matches!(Client::new(&srv.base).get_bytes(path).await.0, 401 | 404));
    let key = format!("uploads/{proj}/{}", path.split('/').nth(5).unwrap());
    assert_eq!(srv.state.blobs.size(&key).await.unwrap(), Some(data.len() as u64));
    owner.delete(&format!("/api/v1/projects/{proj}")).await;
    forgust_server::services::uploads::drain_deletions(&srv.state).await.unwrap();
    assert_eq!(srv.state.blobs.size(&key).await.unwrap(), None);
}

async fn mailpit(path: &str) -> Value {
    let api = env("FORGUST_TEST_MAILPIT_API", "http://127.0.0.1:8026/api/v1");
    reqwest::get(format!("{api}{path}")).await.unwrap().json().await.unwrap()
}

/// Wait for a message to `to` in mailpit; returns its full JSON.
async fn wait_for_mail(to: &str) -> Value {
    for _ in 0..50 {
        let found = mailpit(&format!("/search?query={}", urlencoding(&format!("to:{to}")))).await;
        if let Some(id) = found["messages"].as_array().and_then(|m| m.first()).and_then(|m| m["ID"].as_str()) {
            return mailpit(&format!("/message/{id}")).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("no email to {to} reached mailpit");
}

fn urlencoding(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// A review request goes out through SMTP: the message mailpit receives.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn email_through_smtp(pool: sqlx::PgPool) {
    let api = env("FORGUST_TEST_MAILPIT_API", "http://127.0.0.1:8026/api/v1");
    if !available("mailpit", &format!("{api}/info")).await {
        return;
    }
    let srv = TestServer::start_with(pool, |c| {
        c.email.smtp_url = env("FORGUST_TEST_SMTP_URL", "smtp://127.0.0.1:1026");
        c.email.from = "Forgust <forgust@example.com>".into();
    })
    .await;
    let owner = srv.user("olivia", false).await;
    // a unique reviewer, so parallel runs never read each other's mail
    let reviewer = unique("rev");
    srv.user(&reviewer, false).await;
    let (org, _sub, proj) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": reviewer, "access_level": "developer" })).await;
    let work = srv.work_dir("smtp");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    std::fs::write(work.join("t.txt"), "t\n").unwrap();
    must(git(&work, &["add", "t.txt"]).await);
    must(git(&work, &["commit", "-q", "-m", "t"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&srv.state).await;
    let users = owner.ok_get(&format!("/api/v1/projects/{proj}/users?search={reviewer}")).await;
    let rid = users[0]["id"].as_i64().unwrap();
    owner
        .ok_post(
            &format!("/api/v1/projects/{proj}/merge_requests"),
            json!({ "source_branch": "topic", "title": "Topic", "reviewer_ids": [rid] }),
        )
        .await;
    wait_jobs(&srv.state).await;

    let msg = wait_for_mail(&format!("{reviewer}@example.com")).await;
    assert_eq!(msg["Subject"], "acme/platform/api !1: Topic");
    assert_eq!(msg["From"]["Address"], "forgust@example.com");
    assert!(msg["Text"].as_str().unwrap().contains("olivia asked you to review merge request !1"), "{msg}");
    // one message per person, to them, with their reason and a one-click unsubscribe
    assert_eq!(msg["To"][0]["Address"], format!("{reviewer}@example.com"), "{msg}");
    let id = msg["ID"].as_str().unwrap();
    let headers = mailpit(&format!("/message/{id}/headers")).await;
    assert_eq!(headers["X-Forgust-Notification-Reason"][0], "directly_addressed", "{headers}");
    assert!(headers["List-Unsubscribe"][0].as_str().unwrap().contains("/api/v1/unsubscribe/"), "{headers}");
    assert_eq!(headers["List-Unsubscribe-Post"][0], "List-Unsubscribe=One-Click");
    assert!(headers["References"][0].as_str().unwrap().starts_with("<merge_request-"), "{headers}");
}
