//! Hardening left from the security review (headers, the image proxy,
//! limits on anonymous reads, reauthentication without a password, what user
//! search shows, live streams that follow access).

mod common;

use std::time::Duration;

use common::*;
use serde_json::json;

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn security_headers_everywhere(pool: sqlx::PgPool) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><html><head><script>var theme = 1;</script><script type=\"module\" src=\"/assets/a.js\"></script></head></html>",
    )
    .unwrap();
    let static_dir = dir.path().to_path_buf();
    let srv = TestServer::start_with(pool, move |c| c.server.static_dir = static_dir).await;
    let http = reqwest::Client::new();

    // the web app: its own scripts plus the inline one, by hash; no framing;
    // images only from here (others come through the proxy)
    let r = http.get(format!("{}/some/page", srv.base)).send().await.unwrap();
    let csp = r.headers()["content-security-policy"].to_str().unwrap().to_string();
    use base64::Engine;
    use sha2::Digest;
    let hash = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(b"var theme = 1;"));
    assert!(csp.contains(&format!("script-src 'self' 'sha256-{hash}';")), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'") && csp.contains("img-src 'self' data: blob:;"), "{csp}");
    assert_eq!(r.headers()["x-frame-options"], "DENY");
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    assert!(r.headers().get("strict-transport-security").is_none(), "no HSTS over plain http");

    // anything else (API JSON) gets a locked-down policy
    let r = http.get(format!("{}/api/v1/version", srv.base)).send().await.unwrap();
    assert_eq!(r.headers()["content-security-policy"], "default-src 'none'; frame-ancestors 'none'; sandbox");
    assert_eq!(r.headers()["referrer-policy"], "strict-origin-when-cross-origin");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn https_instances_send_hsts_and_metrics_can_need_a_token(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| {
        c.server.external_url = "https://forge.example.com".into();
        c.security.image_proxy = false;
        c.telemetry.metrics_token = "scrape-me".into();
    })
    .await;
    let http = reqwest::Client::new();
    let r = http.get(format!("{}/health", srv.base)).send().await.unwrap();
    assert_eq!(r.headers()["strict-transport-security"], "max-age=31536000");
    // /metrics: refused without the token (with it, the test server has no metrics at all)
    assert_eq!(http.get(format!("{}/metrics", srv.base)).send().await.unwrap().status(), 401);
    assert_eq!(http.get(format!("{}/metrics", srv.base)).bearer_auth("wrong").send().await.unwrap().status(), 401);
    assert_eq!(http.get(format!("{}/metrics", srv.base)).bearer_auth("scrape-me").send().await.unwrap().status(), 404);
}

/// An image host on loopback: a PNG, a redirect to it, and a page that is not an image.
async fn image_host() -> String {
    use axum::http::header;
    use axum::response::IntoResponse;
    let app = axum::Router::new()
        .route("/a.png", axum::routing::get(|| async { ([(header::CONTENT_TYPE, "image/png")], b"\x89PNG fake".to_vec()).into_response() }))
        .route("/moved", axum::routing::get(|| async { axum::response::Redirect::temporary("/a.png") }))
        .route("/page", axum::routing::get(|| async { ([(header::CONTENT_TYPE, "text/html")], "<script>x</script>").into_response() }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn proxied(html: &str) -> String {
    let start = html.find("/-/image/").unwrap_or_else(|| panic!("no proxied image in {html}"));
    html[start..].split('"').next().unwrap().to_string()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn markdown_images_come_through_the_proxy(pool: sqlx::PgPool) {
    let host = image_host().await;
    // its own switch: allowing local webhooks does not open the proxy to them
    let srv = TestServer::start_with(pool, |c| {
        c.features.webhooks_allow_local = true;
        c.features.image_proxy_allow_local = true;
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let render = |text: String| {
        let owner = owner.clone();
        async move {
            owner.ok_post(&format!("/api/v1/projects/{project}/markdown"), json!({ "text": text })).await["html"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let html = render(format!("![a]({host}/a.png) ![b](/-/project/{project}/uploads/x/b.png)")).await;
    assert!(!html.contains(&host), "the image host never reaches the page: {html}");
    assert!(html.contains(&format!("/-/project/{project}/uploads/x/b.png")), "images on this instance stay as they are: {html}");
    let http = reqwest::Client::new();
    let path = proxied(&html);
    let r = http.get(format!("{}{path}", srv.base)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert!(r.headers()["content-security-policy"].to_str().unwrap().contains("sandbox"));
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"\x89PNG fake");
    // a redirect is followed (and checked again)
    let path = proxied(&render(format!("![m]({host}/moved)")).await);
    assert_eq!(http.get(format!("{}{path}", srv.base)).send().await.unwrap().status(), 200);
    // not an image: nothing is passed on
    let path = proxied(&render(format!("![p]({host}/page)")).await);
    assert_eq!(http.get(format!("{}{path}", srv.base)).send().await.unwrap().status(), 502);
    // only URLs the server signed
    let forged = format!("/-/image/AAAA/{}", path.rsplit('/').next().unwrap());
    assert_eq!(http.get(format!("{}{forged}", srv.base)).send().await.unwrap().status(), 404);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn the_proxy_refuses_internal_addresses(pool: sqlx::PgPool) {
    let host = image_host().await;
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let html = owner.ok_post(&format!("/api/v1/projects/{project}/markdown"), json!({ "text": format!("![a]({host}/a.png)") })).await;
    let path = proxied(html["html"].as_str().unwrap());
    assert_eq!(reqwest::get(format!("{}{path}", srv.base)).await.unwrap().status(), 502);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn anonymous_expensive_reads_are_limited(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.limits.anonymous_heavy_reads_per_minute = 3).await;
    let owner = srv.user("olivia", false).await;
    let g = owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await;
    let p =
        owner.ok_post("/api/v1/projects", json!({ "name": "Site", "path": "site", "namespace_id": g["id"], "visibility": "public" })).await;
    let work = srv.work_dir("site");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "open/site"), "main"]).await);
    wait_jobs(&srv.state).await;
    let anon = Client::new(&srv.base);
    let commits = format!("/api/v1/projects/{}/repository/commits", p["id"]);
    for _ in 0..3 {
        assert_eq!(anon.get(&commits).await.0, 200);
    }
    let (s, body) = anon.get(&format!("/api/v1/projects/{}/repository/blame?path=README.md", p["id"])).await;
    assert_eq!(s, 429, "{body}");
    // signed-in people are not counted
    for _ in 0..5 {
        assert_eq!(owner.get(&commits).await.0, 200);
    }
}

/// A session for `user_id` signed in `age` ago, as a client.
async fn session_aged(srv: &TestServer, user_id: i64, age: chrono::Duration) -> Client {
    let token = format!("test-session-{user_id}-{}", age.num_seconds());
    let hash = forgust_server::crypto::token_hash(&token);
    forgust_db::sessions::create(&srv.state.db, &hash, user_id, "csrf-x", None, None, chrono::Utc::now() + chrono::Duration::days(1))
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET created_at = now() - make_interval(secs => $1) WHERE token_hash = $2")
        .bind(age.num_seconds() as f64)
        .bind(&hash)
        .execute(&srv.state.db)
        .await
        .unwrap();
    let mut c = Client::new(&srv.base);
    c.cookie = Some(format!("{}={token}", forgust_server::auth::SESSION_COOKIE));
    c.csrf = Some("csrf-x".into());
    c
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn accounts_without_a_password_reauthenticate_by_signing_in(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let mut tx = srv.state.db.begin().await.unwrap();
    let u = forgust_server::services::users::create(
        &mut tx,
        forgust_server::services::users::CreateUser {
            username: "sso",
            email: "sso@example.com",
            name: "",
            password: None,
            is_admin: false,
            email_verified: true,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // an old session (a stolen cookie) cannot add a password or drop 2FA
    let old = session_aged(&srv, u.id, chrono::Duration::hours(2)).await;
    let (s, body) = old.put("/api/v1/user/password", json!({ "current": "", "new": "a-brand-new-password" })).await;
    assert_eq!(s, 403, "{body}");
    assert!(body["message"].as_str().unwrap().contains("sign in again"), "{body}");
    assert_eq!(old.post("/api/v1/user/two_factor/disable", json!({ "password": "" })).await.0, 403);
    // right after signing in, it can
    let fresh = session_aged(&srv, u.id, chrono::Duration::minutes(1)).await;
    let (s, body) = fresh.put("/api/v1/user/password", json!({ "current": "", "new": "a-brand-new-password" })).await;
    assert_eq!(s, 200, "{body}");
    // from now on the password is what counts
    let (s, _) = fresh.post("/api/v1/user/two_factor/disable", json!({ "password": "" })).await;
    assert_eq!(s, 403);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn user_search_hides_admin_and_state(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let olivia = srv.user("olivia", false).await;
    let found = olivia.ok_get("/api/v1/users?search=root").await;
    let root = found.as_array().unwrap().iter().find(|u| u["username"] == "root").unwrap();
    assert!(root["is_admin"].is_null() && root["state"].is_null(), "{root}");
    let found = admin.ok_get("/api/v1/users?search=root").await;
    let root = found.as_array().unwrap().iter().find(|u| u["username"] == "root").unwrap();
    assert_eq!((root["is_admin"].as_bool(), root["state"].as_str()), (Some(true), Some("active")));
    assert_eq!(olivia.ok_get("/api/v1/user").await["is_admin"], false);
}

/// The next chunk of an SSE stream, if one comes within 2 s.
async fn next_event<S: futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin>(s: &mut S) -> Option<String> {
    let chunk = tokio::time::timeout(Duration::from_secs(2), futures::StreamExt::next(s)).await.ok().flatten()?;
    Some(String::from_utf8_lossy(&chunk.unwrap()).into_owned())
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn live_streams_follow_access(pool: sqlx::PgPool) {
    use forgust_server::events::Event;
    let srv = TestServer::start_with(pool, |c| c.limits.events_recheck_secs = 0).await;
    let owner = srv.user("olivia", false).await;
    let dave = srv.user("dave", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    let resp = dave
        .http
        .get(format!("{}/api/v1/events?projects={project}", srv.base))
        .header("cookie", dave.cookie.clone().unwrap())
        .send()
        .await
        .unwrap();
    let mut stream = resp.bytes_stream();
    srv.state.events.publish(Event::Push { project_id: project, refs: vec!["refs/heads/a".into()] });
    let got = next_event(&mut stream).await.expect("a member hears about pushes");
    assert!(got.contains("refs/heads/a"), "{got}");
    let did = dave.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let (s, body) = owner.delete(&format!("/api/v1/projects/{project}/members/{did}")).await;
    assert_eq!(s, 200, "{body}");
    srv.state.events.publish(Event::Push { project_id: project, refs: vec!["refs/heads/b".into()] });
    let got: Option<String> = next_event(&mut stream).await;
    assert!(got.as_deref().is_none_or(|t| !t.contains("refs/heads/b")), "a removed member hears nothing more: {got:?}");
}

/// Over https the session cookie is `__Host-` prefixed (a sibling subdomain
/// cannot plant or replace it); the plain name is then ignored.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn https_sessions_use_a_host_only_cookie(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.security.cookie_secure = true).await;
    srv.user("olivia", false).await;
    let r = Client::new(&srv.base)
        .http
        .post(format!("{}/api/v1/auth/login", srv.base))
        .json(&json!({ "login": "olivia", "password": password("olivia") }))
        .send()
        .await
        .unwrap();
    let set = r.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(
        set.starts_with("__Host-forgust_session=") && set.contains("Secure") && set.contains("Path=/") && !set.contains("Domain"),
        "{set}"
    );
    let token = set.split(';').next().unwrap().split_once('=').unwrap().1.to_string();
    let get = |cookie: String| {
        let base = srv.base.clone();
        async move { reqwest::Client::new().get(format!("{base}/api/v1/session")).header("cookie", cookie).send().await.unwrap().status() }
    };
    assert_eq!(get(format!("__Host-forgust_session={token}")).await, 200);
    assert_eq!(get(format!("forgust_session={token}")).await, 401, "the unprefixed name is not read over https");
}
