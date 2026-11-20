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
    let flags = forgust_server::services::vulnerability_dbs::trivy_flags(&srv.state, &cfg).await.unwrap();
    assert_eq!(flags.db.as_deref(), Some(format!("{}/admin/mirrors/trivy-db:2", srv.host()).as_str()));
    assert!(flags.insecure, "the test server has no TLS");
    assert!(flags.java_db.is_none() && flags.checks.is_none(), "only what the mirror holds");
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
    let flags = forgust_server::services::vulnerability_dbs::trivy_flags(&srv.state, &cfg).await.unwrap();
    assert_eq!(flags, Default::default());
}
