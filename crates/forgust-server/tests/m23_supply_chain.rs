//! Supply chain: Trivy's databases mirrored on the server (copied from an
//! upstream registry, or imported from a file) and served read-only by the
//! registry; scans told to read them from here.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{Path, Request};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn sha(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(b)))
}

/// A registry like ghcr.io: anonymous tokens from a challenge, blobs behind a
/// redirect to another path (a CDN). Counts layer downloads.
struct Upstream {
    base: String,
    layer: Vec<u8>,
    downloads: Arc<AtomicUsize>,
}

async fn upstream(layer: Vec<u8>) -> Upstream {
    upstream_serving(layer.clone(), layer).await
}

/// As [`upstream`], its CDN serving `served` for the layer.
async fn upstream_serving(layer: Vec<u8>, served: Vec<u8>) -> Upstream {
    let config = b"{}".to_vec();
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "artifactType": "application/vnd.aquasec.trivy.config.v1+json",
        "config": { "mediaType": "application/vnd.oci.empty.v1+json", "digest": sha(&config), "size": 2 },
        "layers": [{ "mediaType": "application/vnd.aquasec.trivy.db.layer.v1.tar+gzip", "digest": sha(&layer), "size": layer.len() }],
    }))
    .unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let downloads = Arc::new(AtomicUsize::new(0));
    let (realm, m, c, lay, n) = (format!("{base}/token"), manifest.clone(), config.clone(), served, downloads.clone());
    let authed = |r: &Request| r.headers().get(header::AUTHORIZATION).is_some_and(|v| v == "Bearer anon-token");
    let challenge = move || {
        (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, format!(r#"Bearer realm="{realm}",service="fake""#))]).into_response()
    };
    let (ch1, ch2) = (challenge.clone(), challenge);
    let cdn = base.replace("127.0.0.1", "localhost");
    let app = axum::Router::new()
        .route(
            "/token",
            axum::routing::get(|q: axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                assert_eq!(q.get("scope").map(String::as_str), Some("repository:aquasecurity/trivy-db:pull"));
                axum::Json(json!({ "token": "anon-token" }))
            }),
        )
        .route(
            "/v2/aquasecurity/trivy-db/manifests/2",
            axum::routing::get(move |r: Request| async move {
                if !authed(&r) {
                    return ch1();
                }
                ([(header::CONTENT_TYPE, "application/vnd.oci.image.manifest.v1+json")], m).into_response()
            }),
        )
        .route(
            "/v2/aquasecurity/trivy-db/blobs/{digest}",
            axum::routing::get(move |Path(d): Path<String>, r: Request| async move {
                if !authed(&r) {
                    return ch2();
                }
                let cdn = cdn.clone();
                // another host, as ghcr.io's CDN
                (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, format!("{cdn}/cdn/{d}"))]).into_response()
            }),
        )
        .route(
            "/cdn/{digest}",
            axum::routing::get(move |Path(d): Path<String>, r: Request| async move {
                // credentials stay with the registry
                assert!(r.headers().get(header::AUTHORIZATION).is_none(), "the token went to the CDN");
                let body: Vec<u8> = if d == sha(&c) {
                    c.clone()
                } else {
                    n.fetch_add(1, Ordering::SeqCst);
                    lay.clone()
                };
                Response::new(axum::body::Body::from(body))
            }),
        );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    Upstream { base, layer, downloads }
}

async fn mirror_server(pool: sqlx::PgPool, db_source: String) -> TestServer {
    TestServer::start_with(pool, move |c| {
        c.security_scanning.mirror_vulnerability_dbs = true;
        c.security_scanning.trivy_db_source = db_source;
        c.security_scanning.trivy_java_db_source = String::new();
        c.security_scanning.trivy_checks_source = String::new();
    })
    .await
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn trivy_db_is_mirrored_and_served(pool: sqlx::PgPool) {
    let up = upstream(b"pretend this is db.tar.gz".to_vec()).await;
    let source = format!("{}/aquasecurity/trivy-db:2", up.base);
    let srv = mirror_server(pool, source.clone()).await;
    let admin = srv.user("root", true).await;

    forgust_server::services::vulnerability_dbs::sync(&srv.state).await.unwrap();
    assert_eq!(up.downloads.load(Ordering::SeqCst), 1);
    // the same upstream copy: nothing downloaded again
    forgust_server::services::vulnerability_dbs::sync(&srv.state).await.unwrap();
    assert_eq!(up.downloads.load(Ordering::SeqCst), 1);

    // anyone pulls it, by tag and by digest, without a token
    let anon = reqwest::Client::new();
    let r = anon.get(format!("{}/v2/admin/mirrors/trivy-db/manifests/2", srv.base)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let digest = r.headers()["docker-content-digest"].to_str().unwrap().to_string();
    let manifest: Value = r.json().await.unwrap();
    let layer = manifest["layers"][0]["digest"].as_str().unwrap();
    assert_eq!(layer, sha(&up.layer));
    let by_digest = anon.get(format!("{}/v2/admin/mirrors/trivy-db/manifests/{digest}", srv.base)).send().await.unwrap();
    assert_eq!(by_digest.status(), 200);
    let blob = anon.get(format!("{}/v2/admin/mirrors/trivy-db/blobs/{layer}", srv.base)).send().await.unwrap();
    assert_eq!(blob.bytes().await.unwrap().as_ref(), up.layer.as_slice());
    // a blob it does not name is unknown, whatever is in the blob store
    let other = anon.get(format!("{}/v2/admin/mirrors/trivy-db/blobs/{}", srv.base, sha(b"x"))).send().await.unwrap();
    assert_eq!(other.status(), 404);
    // and nobody writes it, administrators included
    let token = admin.ok_post("/api/v1/user/tokens", json!({ "name": "w", "scopes": ["api", "write_repository"] })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let push =
        anon.post(format!("{}/v2/admin/mirrors/trivy-db/blobs/uploads/", srv.base)).basic_auth("root", Some(&token)).send().await.unwrap();
    assert_eq!(push.status(), 403);

    // administrators see it; the scans are told to read it from here
    let status = admin.ok_get("/api/v1/admin/security/scanning").await;
    let m = &status["mirrors"][0];
    assert_eq!(
        (m["name"].as_str(), m["origin"].as_str(), m["digest"].as_str()),
        (Some("trivy-db"), Some(source.as_str()), Some(digest.as_str()))
    );
    let cfg = forgust_server::services::scanning::config(&srv.state).await.unwrap();
    let mirror = forgust_server::services::vulnerability_dbs::trivy_mirror(&srv.state, &cfg).await.unwrap();
    let at = format!("{}/admin/mirrors/trivy-db:2", srv.host());
    // only what the mirror holds; no TLS on the test server
    assert_eq!(mirror.0, [("TRIVY_DB_REPOSITORY", at), ("TRIVY_INSECURE", "true".to_string())]);
    assert_eq!(srv.audit_events("vulnerability_db_stored").await.len(), 1, "once: the second sync stored nothing");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_bad_download_keeps_the_copy_there_was(pool: sqlx::PgPool) {
    let srv = mirror_server(pool, String::new()).await;
    let point_at = |source: String| {
        let state = srv.state.clone();
        async move {
            let mut cfg = forgust_server::services::scanning::config(&state).await.unwrap();
            cfg.trivy_db_source = source;
            forgust_db::security::set_scanning_overrides(&state.db, &serde_json::to_value(&cfg).unwrap()).await.unwrap();
        }
    };
    let current = || async { forgust_db::mirrors::get(&srv.state.db, "trivy-db").await.unwrap().unwrap() };
    // a first copy, from a file
    let file = srv.tmp.path().join("db.tar.gz");
    std::fs::write(&file, b"imported db").unwrap();
    let k = forgust_server::services::vulnerability_dbs::TRIVY_DB;
    forgust_server::services::vulnerability_dbs::import_file(&srv.state, k, &file).await.unwrap();
    let imported = current().await;

    // an upstream whose layer is not what its manifest says: refused
    let corrupt = upstream_serving(b"the real db".to_vec(), b"something else".to_vec()).await;
    point_at(format!("{}/aquasecurity/trivy-db:2", corrupt.base)).await;
    let err = forgust_server::services::vulnerability_dbs::sync(&srv.state).await.unwrap_err();
    assert!(format!("{err:#}").contains("trivy-db"), "{err:#}");
    assert_eq!(current().await.digest, imported.digest, "the copy there was stays");

    // one that answers with a blob the copy here serves: refused, that blob stays
    let confused = upstream_serving(b"the real db".to_vec(), b"imported db".to_vec()).await;
    point_at(format!("{}/aquasecurity/trivy-db:2", confused.base)).await;
    assert!(forgust_server::services::vulnerability_dbs::sync(&srv.state).await.is_err());
    assert_eq!(current().await.digest, imported.digest);
    let r = reqwest::get(format!("{}/v2/admin/mirrors/trivy-db/blobs/{}", srv.base, sha(b"imported db"))).await.unwrap();
    assert_eq!(r.status(), 200, "the blob the copy names is not deleted");

    // unreachable: the same
    point_at("http://127.0.0.1:9/aquasecurity/trivy-db:2".into()).await;
    assert!(forgust_server::services::vulnerability_dbs::sync(&srv.state).await.is_err());
    assert_eq!(current().await.digest, imported.digest);

    // a good one replaces it; the copy before stays for pulls in flight
    let good = upstream(b"the real db".to_vec()).await;
    point_at(format!("{}/aquasecurity/trivy-db:2", good.base)).await;
    forgust_server::services::vulnerability_dbs::sync(&srv.state).await.unwrap();
    let now = current().await;
    assert_ne!(now.digest, imported.digest);
    assert!(now.previous_blob_digests.contains(&sha(b"imported db")));
    let r = reqwest::get(format!("{}/v2/admin/mirrors/trivy-db/blobs/{}", srv.base, sha(b"imported db"))).await.unwrap();
    assert_eq!(r.status(), 200, "still served");

    // a blob the store lost comes back at the next sync, upstream unchanged
    let key = forgust_server::services::vulnerability_dbs::blob_key(&sha(b"the real db"));
    srv.state.blobs.delete(&key).await.unwrap();
    let before = good.downloads.load(Ordering::SeqCst);
    forgust_server::services::vulnerability_dbs::sync(&srv.state).await.unwrap();
    assert_eq!(good.downloads.load(Ordering::SeqCst), before + 1);
    let after = current().await;
    assert_eq!((after.digest, after.previous_blob_digests), (now.digest, now.previous_blob_digests), "the same copy, not a new one");
    let r = reqwest::get(format!("{}/v2/admin/mirrors/trivy-db/blobs/{}", srv.base, sha(b"the real db"))).await.unwrap();
    assert_eq!(r.status(), 200, "served again");
    // nothing lost: nothing downloaded
    forgust_server::services::vulnerability_dbs::sync(&srv.state).await.unwrap();
    assert_eq!(good.downloads.load(Ordering::SeqCst), before + 1);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn air_gapped_installs_import_from_a_file(pool: sqlx::PgPool) {
    let srv = mirror_server(pool, String::new()).await;
    let file = srv.tmp.path().join("bundle.tar.gz");
    std::fs::write(&file, b"checks").unwrap();
    let k = forgust_server::services::vulnerability_dbs::TRIVY_CHECKS;
    let digest = forgust_server::services::vulnerability_dbs::import_file(&srv.state, k, &file).await.unwrap();
    let r = reqwest::get(format!("{}/v2/admin/mirrors/trivy-checks/manifests/1", srv.base)).await.unwrap();
    assert_eq!(r.headers()["docker-content-digest"], digest.as_str());
    let m: Value = r.json().await.unwrap();
    assert_eq!(m["artifactType"], "application/vnd.cncf.openpolicyagent.config.v1+json");
    assert_eq!(m["layers"][0]["mediaType"], "application/vnd.cncf.openpolicyagent.layer.v1.tar+gzip");
    assert_eq!(m["layers"][0]["annotations"]["org.opencontainers.image.title"], "bundle.tar.gz");
    let blob = reqwest::get(format!("{}/v2/admin/mirrors/trivy-checks/blobs/{}", srv.base, sha(b"checks"))).await.unwrap();
    assert_eq!(blob.bytes().await.unwrap().as_ref(), b"checks");
    let events = srv.audit_events("vulnerability_db_stored").await;
    assert_eq!(events[0].2["origin"], "file");

    // mirroring off: the scans read Trivy's own sources
    let mut cfg = forgust_server::services::scanning::config(&srv.state).await.unwrap();
    cfg.mirror_vulnerability_dbs = false;
    let mirror = forgust_server::services::vulnerability_dbs::trivy_mirror(&srv.state, &cfg).await.unwrap();
    assert_eq!(mirror, Default::default());
}

// ------------------------------------------------------------------ SBOMs

async fn try_request(srv: &TestServer, runner: &str) -> Option<Value> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", srv.base))
        .json(&json!({ "token": runner, "system_id": "s_sbom", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    if r.status() == StatusCode::CREATED { Some(r.json().await.unwrap()) } else { None }
}

fn var(job: &Value, k: &str) -> Option<String> {
    job["variables"].as_array().unwrap().iter().rev().find(|v| v["key"] == k).map(|v| v["value"].as_str().unwrap().to_string())
}

/// The next `sbom` job (others the runner gets pass with nothing).
async fn sbom_job(srv: &TestServer, runner: &str) -> Option<Value> {
    while let Some(j) = try_request(srv, runner).await {
        if j["job_info"]["name"] == "sbom" {
            return Some(j);
        }
        finish(srv, &j, None).await;
    }
    None
}

/// Upload `archive` (a zip) as the job's artifacts, and pass.
async fn finish(srv: &TestServer, job: &Value, archive: Option<Vec<u8>>) {
    end(srv, job, archive, "success").await
}

async fn end(srv: &TestServer, job: &Value, archive: Option<Vec<u8>>, state: &str) {
    let http = reqwest::Client::new();
    if let Some(zip) = archive {
        let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(zip).file_name("artifacts.zip"));
        let r = http
            .post(format!("{}/api/v4/jobs/{}/artifacts?artifact_format=zip&artifact_type=archive&expire_in=1+day", srv.base, job["id"]))
            .header("JOB-TOKEN", job["token"].as_str().unwrap())
            .multipart(form)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
    }
    let r = http
        .put(format!("{}/api/v4/jobs/{}", srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": state, "exit_code": if state == "success" { 0 } else { 1 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    wait_jobs(&srv.state).await;
}

/// What the SBOM job leaves: both files, as a zip (`git archive` writes it).
async fn sbom_zip(dir: &std::path::Path, tag: &str) -> Vec<u8> {
    must(git(dir, &["init", "-q", "-b", "main"]).await);
    std::fs::write(dir.join("sbom.cdx.json"), format!(r#"{{"bomFormat":"CycloneDX","specVersion":"1.6","tag":"{tag}"}}"#)).unwrap();
    std::fs::write(dir.join("sbom.spdx.json"), format!(r#"{{"spdxVersion":"SPDX-2.3","tag":"{tag}"}}"#)).unwrap();
    must(git(dir, &["add", "."]).await);
    must(git(dir, &["commit", "-q", "-m", "sbom"]).await);
    let out =
        tokio::process::Command::new("git").args(["-C", dir.to_str().unwrap(), "archive", "--format=zip", "HEAD"]).output().await.unwrap();
    assert!(out.status.success());
    out.stdout
}

fn sbom_links(release: &Value) -> Vec<(String, String)> {
    release["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["name"].as_str().unwrap().starts_with("SBOM"))
        .map(|l| (l["name"].as_str().unwrap().to_string(), l["href"].as_str().unwrap().to_string()))
        .collect()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn tags_get_an_sbom_on_their_release(pool: sqlx::PgPool) {
    if !has("git") {
        return;
    }
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    let work = srv.work_dir("repo");
    seed_repo(&work).await;
    let url = srv.http_url("olivia", "acme/platform/api");
    must(git(&work, &["push", "-q", &url, "main"]).await);
    wait_jobs(&srv.state).await;
    let runner = admin.ok_post("/api/v1/admin/runners", json!({ "description": "shared", "run_untagged": true })).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let scanning = format!("/api/v1/projects/{p}/security/scanning");

    // off: a tag gets nothing
    must(git(&work, &["tag", "v0.9"]).await);
    must(git(&work, &["push", "-q", &url, "v0.9"]).await);
    wait_jobs(&srv.state).await;
    assert!(sbom_job(&srv, &runner).await.is_none());

    let st = owner.ok_put(&scanning, json!({ "enabled": true, "scanners": ["sast"], "sbom_on_tags": true })).await;
    assert_eq!(st["sbom_on_tags"], true, "{st}");
    assert_eq!(srv.audit_events("sbom_on_tags_enabled").await.len(), 1);
    // left out of a later change: as it was
    let st = owner.ok_put(&scanning, json!({ "enabled": true, "scanners": ["sast"] })).await;
    assert_eq!(st["sbom_on_tags"], true);

    // a tag pushed before its release: the SBOM waits for the release
    must(git(&work, &["tag", "-a", "v1.0", "-m", "one"]).await);
    must(git(&work, &["push", "-q", &url, "v1.0"]).await);
    wait_jobs(&srv.state).await;
    let job = sbom_job(&srv, &runner).await.expect("an sbom job");
    assert_eq!(var(&job, "CI_PIPELINE_SOURCE").as_deref(), Some("sbom"));
    assert_eq!(var(&job, "CI_COMMIT_TAG").as_deref(), Some("v1.0"));
    assert_eq!(var(&job, "GITLAB_USER_LOGIN"), Some(format!("security-bot-{p}")));
    assert_eq!(job["image"]["name"], "aquasec/trivy:0.57.1");
    let script = job["steps"][0]["script"].to_string();
    assert!(script.contains("--format cyclonedx") && script.contains("--format spdx-json"), "{script}");
    finish(&srv, &job, Some(sbom_zip(&srv.work_dir("z1"), "v1.0").await)).await;
    let release = owner.ok_post(&format!("/api/v1/projects/{p}/releases"), json!({ "tag_name": "v1.0", "name": "One" })).await;
    let links = sbom_links(&release);
    assert_eq!(links.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["SBOM (CycloneDX)", "SBOM (SPDX)"], "{release}");
    let (s, body) = owner.get_bytes(links[0].1.trim_start_matches(&srv.base)).await;
    assert_eq!(s, 200);
    assert!(String::from_utf8_lossy(&body).contains(r#""tag":"v1.0""#));

    // made from the code: Guests of the private project see the release, not its SBOMs
    let guest = srv.user("gina", false).await;
    let reporter = srv.user("rita", false).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "gina", "access_level": "guest" })).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let seen = guest.ok_get(&format!("/api/v1/projects/{p}/releases/v1.0")).await;
    assert!(sbom_links(&seen).is_empty(), "{seen}");
    let v4 = guest.ok_get(&format!("/api/v4/projects/{p}/releases/v1.0")).await;
    assert!(!v4.to_string().contains("SBOM"), "{v4}");
    assert_eq!(guest.get_bytes(links[0].1.trim_start_matches(&srv.base)).await.0, 404, "nor its file");
    let download = format!("/api/v4/projects/{p}/releases/v1.0/downloads/sbom.cdx.json");
    assert_eq!(guest.get_bytes(&download).await.0, 404, "nor where it is");
    assert_eq!(reporter.get_bytes(&download).await.0, 302);
    let seen = reporter.ok_get(&format!("/api/v1/projects/{p}/releases/v1.0")).await;
    assert_eq!(sbom_links(&seen).len(), 2, "who reads the code does");
    assert_eq!(reporter.get_bytes(links[0].1.trim_start_matches(&srv.base)).await.0, 200);

    // a release made with its tag: the SBOM joins it once the job passed
    owner.ok_post(&format!("/api/v1/projects/{p}/releases"), json!({ "tag_name": "v2.0", "ref": "main", "name": "Two" })).await;
    wait_jobs(&srv.state).await;
    let job = sbom_job(&srv, &runner).await.expect("an sbom job for the tag the release made");
    assert_eq!(var(&job, "CI_COMMIT_TAG").as_deref(), Some("v2.0"));
    finish(&srv, &job, Some(sbom_zip(&srv.work_dir("z2"), "v2.0").await)).await;
    let release = owner.ok_get(&format!("/api/v1/projects/{p}/releases/v2.0")).await;
    assert_eq!(sbom_links(&release).len(), 2, "{release}");

    let kept = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM uploads WHERE project_id = $1 AND requires_code")
            .bind(p)
            .fetch_one(&srv.state.db)
            .await
            .unwrap()
    };
    assert_eq!(kept().await, 4, "two files per tag");

    // a deleted tag: its SBOM goes with it, files too; pushed back, it gets another
    must(git(&work, &["push", "-q", &url, ":refs/tags/v1.0"]).await);
    wait_jobs(&srv.state).await;
    assert_eq!(kept().await, 2);
    assert_eq!(owner.get_bytes(links[0].1.trim_start_matches(&srv.base)).await.0, 404);
    must(git(&work, &["push", "-q", &url, "v1.0"]).await);
    wait_jobs(&srv.state).await;
    let job = sbom_job(&srv, &runner).await.expect("an sbom job for the tag pushed back");
    finish(&srv, &job, Some(sbom_zip(&srv.work_dir("z1b"), "v1.0").await)).await;
    assert_eq!(kept().await, 4);

    // moved to another commit: the release made while its run is going gets
    // no SBOM of the old commit, then the new one once the run passed
    must(git(&work, &["push", "-q", &url, ":refs/tags/v1.0"]).await);
    wait_jobs(&srv.state).await;
    commit_file(&work, "CHANGELOG.md", "1.0 again\n", "changelog").await;
    must(git(&work, &["tag", "-f", "-a", "v1.0", "-m", "one, again"]).await);
    must(git(&work, &["push", "-q", &url, "main", "v1.0"]).await);
    wait_jobs(&srv.state).await;
    let release = owner.ok_post(&format!("/api/v1/projects/{p}/releases"), json!({ "tag_name": "v1.0", "name": "One again" })).await;
    assert!(sbom_links(&release).is_empty(), "not the old commit's SBOM: {release}");
    let job = sbom_job(&srv, &runner).await.expect("an sbom job for the moved tag");
    finish(&srv, &job, Some(sbom_zip(&srv.work_dir("z3"), "v1.0 again").await)).await;
    let release = owner.ok_get(&format!("/api/v1/projects/{p}/releases/v1.0")).await;
    let links = sbom_links(&release);
    assert_eq!(links.len(), 2, "{release}");
    let (_, body) = owner.get_bytes(links[0].1.trim_start_matches(&srv.base)).await;
    assert!(String::from_utf8_lossy(&body).contains("v1.0 again"));

    // moved by a forced push, its release staying: the old commit's SBOM
    // leaves the release at once, the new one joins it once its run passed
    commit_file(&work, "CHANGELOG.md", "1.0 once more\n", "changelog again").await;
    must(git(&work, &["tag", "-f", "-a", "v1.0", "-m", "one, once more"]).await);
    must(git(&work, &["push", "-q", "-f", &url, "main", "v1.0"]).await);
    wait_jobs(&srv.state).await;
    let release = owner.ok_get(&format!("/api/v1/projects/{p}/releases/v1.0")).await;
    assert!(sbom_links(&release).is_empty(), "not the SBOM of the commit it left: {release}");
    assert_eq!(owner.get_bytes(links[0].1.trim_start_matches(&srv.base)).await.0, 404, "nor its file");
    assert_eq!(kept().await, 2);
    let job = sbom_job(&srv, &runner).await.expect("an sbom job for the tag forced elsewhere");
    finish(&srv, &job, Some(sbom_zip(&srv.work_dir("z4"), "v1.0 once more").await)).await;
    let release = owner.ok_get(&format!("/api/v1/projects/{p}/releases/v1.0")).await;
    let links = sbom_links(&release);
    assert_eq!(links.len(), 2, "{release}");
    let (_, body) = owner.get_bytes(links[0].1.trim_start_matches(&srv.base)).await;
    assert!(String::from_utf8_lossy(&body).contains("v1.0 once more"));
    assert_eq!(kept().await, 4, "no file of a commit a tag left");

    // a run that failed holds its tag and commit no longer: another may start
    must(git(&work, &["tag", "v3.0"]).await);
    must(git(&work, &["push", "-q", &url, "v3.0"]).await);
    wait_jobs(&srv.state).await;
    let job = sbom_job(&srv, &runner).await.expect("an sbom job");
    let at = var(&job, "CI_COMMIT_SHA").unwrap();
    assert!(forgust_db::security::claim_sbom(&srv.state.db, p, "v3.0", &at).await.unwrap().is_none(), "one run while it goes");
    end(&srv, &job, None, "failed").await;
    assert!(forgust_db::security::claim_sbom(&srv.state.db, p, "v3.0", &at).await.unwrap().is_some());

    // SBOMs on tags turned off: the runs going on stop
    must(git(&work, &["tag", "v4.0"]).await);
    must(git(&work, &["push", "-q", &url, "v4.0"]).await);
    wait_jobs(&srv.state).await;
    let st = owner.ok_put(&scanning, json!({ "enabled": true, "scanners": ["sast"], "sbom_on_tags": false })).await;
    assert_eq!(st["sbom_on_tags"], false, "{st}");
    assert!(sbom_job(&srv, &runner).await.is_none(), "canceled with the setting");

    // SBOM runs are not scans: the scan settings' last run, if any, is a scan
    let last: Option<String> = sqlx::query_scalar(
        "SELECT p.source FROM security_scanning_settings s JOIN ci_pipelines p ON p.id = s.last_pipeline_id WHERE s.project_id = $1",
    )
    .bind(p)
    .fetch_optional(&srv.state.db)
    .await
    .unwrap();
    assert!(last.as_deref().is_none_or(|s| s == "security"), "{last:?}");
}
