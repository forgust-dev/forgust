//! M18: single sign-on — linking a provider to a local account (from its
//! settings, or by an administrator), unlinking it, limiting a provider to
//! groups, and what a sign-in takes over from the provider (administrator
//! rights, name, email). The provider is a small stand-in served by the test.

mod common;

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::{get, post};
use common::*;
use serde_json::{Value, json};

/// A stand-in OpenID provider: whoever "signs in" next is `next` (the claims
/// of the ID token it hands out).
#[derive(Clone)]
struct Idp {
    base: String,
    next: Arc<Mutex<Value>>,
    /// How often its discovery document was asked for.
    discoveries: Arc<std::sync::atomic::AtomicUsize>,
    key: Arc<forgust_server::ci::oidc::SigningKey>,
}

/// A server with the provider `corp` (as `tweak` sets it up), and the provider.
async fn with_idp(pool: sqlx::PgPool, tweak: Value) -> (TestServer, Idp) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut provider = json!({ "display_name": "Corp", "issuer": base, "client_id": "forgust", "client_secret": "s3cret" });
    provider.as_object_mut().unwrap().extend(tweak.as_object().unwrap().clone());
    let srv = TestServer::start_with(pool, |c| {
        c.oidc.insert("corp".into(), serde_json::from_value(provider).unwrap());
    })
    .await;
    // any P-256 key does: the instance's own saves making one
    let key = forgust_server::ci::oidc::key(&srv.state).await.unwrap();
    let idp = Idp { base, next: Arc::default(), discoveries: Arc::default(), key };
    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(|State(i): State<Idp>| async move {
                i.discoveries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                axum::Json(json!({
                    "issuer": i.base,
                    "authorization_endpoint": format!("{}/authorize", i.base),
                    "token_endpoint": format!("{}/token", i.base),
                    "jwks_uri": format!("{}/jwks", i.base),
                    "response_types_supported": ["code"],
                    "subject_types_supported": ["public"],
                    "id_token_signing_alg_values_supported": ["ES256"],
                }))
            }),
        )
        .route("/jwks", get(|State(i): State<Idp>| async move { axum::Json(json!({ "keys": [i.key.jwk()] })) }))
        .route(
            "/token",
            post(|State(i): State<Idp>| async move {
                let now = chrono::Utc::now().timestamp();
                let mut claims = json!({ "iss": i.base, "aud": "forgust", "iat": now, "exp": now + 300 });
                claims.as_object_mut().unwrap().extend(i.next.lock().unwrap().as_object().unwrap().clone());
                axum::Json(json!({ "access_token": "at", "token_type": "Bearer", "id_token": i.key.sign(&claims).unwrap() }))
            }),
        )
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (srv, idp)
}

fn query_param(url: &str, name: &str) -> String {
    let url = reqwest::Url::parse(url).unwrap();
    url.query_pairs().find(|(k, _)| k == name).unwrap_or_else(|| panic!("{name} in {url}")).1.into_owned()
}

/// The person signs in at the provider as `claims` and comes back to Forgust,
/// in the browser of `client` (signed in or not): the callback's answer.
async fn come_back(client: &Client, idp: &Idp, authorize_url: &str, mut claims: Value) -> reqwest::Response {
    let state = query_param(authorize_url, "state");
    claims["nonce"] = json!(query_param(authorize_url, "nonce"));
    *idp.next.lock().unwrap() = claims;
    let mut cookie = format!("forgust_oidc={state}");
    if let Some(session) = &client.cookie {
        cookie = format!("{session}; {cookie}");
    }
    client
        .http
        .get(format!("{}/api/v1/auth/oidc/corp/callback?code=the-code&state={state}", client.base))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
}

/// A sign-in through the provider from a browser with no session.
async fn sign_in(srv: &TestServer, idp: &Idp, claims: Value) -> reqwest::Response {
    let anon = Client::new(&srv.base);
    let start = anon.req(reqwest::Method::GET, "/api/v1/auth/oidc/corp/start").send().await.unwrap();
    assert!(start.status().is_redirection(), "{}", start.status());
    come_back(&anon, idp, &location(&start), claims).await
}

/// Who the session a sign-in started belongs to.
async fn signed_in_as(srv: &TestServer, r: reqwest::Response) -> String {
    session_of(srv, r).await.ok_get("/api/v1/user").await["username"].as_str().unwrap().to_string()
}

/// Start linking from the settings; the provider's authorize URL.
async fn start_link(c: &Client, username: &str) -> String {
    c.ok_post("/api/v1/user/identities/corp/link", json!({ "password": password(username) })).await["url"].as_str().unwrap().to_string()
}

/// Why a sign-in was refused, as the sign-in page is told.
fn refusal(r: &reqwest::Response) -> String {
    let to = location(r);
    assert!(r.status().is_redirection() && to.starts_with("/login?error="), "{} {to}", r.status());
    query_param(&format!("http://x{to}"), "error")
}

/// A browser session from a sign-in through the provider.
async fn session_of(srv: &TestServer, r: reqwest::Response) -> Client {
    assert_eq!(location(&r), "/");
    let session = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|c| c.to_str().unwrap().split(';').next().unwrap().to_string())
        .find(|c| !c.starts_with("forgust_oidc="))
        .expect("a session cookie");
    let mut c = Client::new(&srv.base);
    c.cookie = Some(session);
    c.csrf = Some(c.ok_get("/api/v1/session").await["csrf_token"].as_str().unwrap().to_string());
    c
}

fn person(sub: &str, email: &str) -> Value {
    json!({ "sub": sub, "email": email, "email_verified": true })
}

/// A local account whose address differs from the provider's links it from its
/// settings, then signs in through the provider; unlinking takes that away.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_local_account_links_a_provider(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({})).await;
    let lina = srv.user("lina", false).await;
    let at_corp = person("corp-lina", "lina.martin@corp.example");

    // nothing ties the two yet
    let r = sign_in(&srv, &idp, at_corp.clone()).await;
    assert!(refusal(&r).contains("no account matches"), "{}", refusal(&r));
    let listed = lina.ok_get("/api/v1/user/identities").await;
    assert_eq!(listed, json!([{ "name": "corp", "display_name": "Corp", "linked_at": null }]));

    // a new way in is asked for with the password
    let (code, body) = lina.post("/api/v1/user/identities/corp/link", json!({ "password": "not-it" })).await;
    assert_eq!(code, 403, "{body}");
    let url = start_link(&lina, "lina").await;
    assert!(url.starts_with(&format!("{}/authorize?", idp.base)), "{url}");
    let r = come_back(&lina, &idp, &url, at_corp.clone()).await;
    assert_eq!(location(&r), "/profile/security?linked=corp");
    assert!(lina.ok_get("/api/v1/user/identities").await[0]["linked_at"].is_string());
    assert_eq!(signed_in_as(&srv, sign_in(&srv, &idp, at_corp.clone()).await).await, "lina");

    // the same provider account cannot serve a second account
    let mallory = srv.user("mallory", false).await;
    let url = start_link(&mallory, "mallory").await;
    let r = come_back(&mallory, &idp, &url, at_corp.clone()).await;
    assert!(location(&r).starts_with("/profile/security?link_error="), "{}", location(&r));
    assert!(location(&r).contains("already%20linked"), "{}", location(&r));
    assert_eq!(signed_in_as(&srv, sign_in(&srv, &idp, at_corp.clone()).await).await, "lina");

    // unlinking, with the password
    let (code, body) = lina.post("/api/v1/user/identities/corp/unlink", json!({ "password": "not-it" })).await;
    assert_eq!(code, 403, "{body}");
    lina.ok_post("/api/v1/user/identities/corp/unlink", json!({ "password": password("lina") })).await;
    assert!(refusal(&sign_in(&srv, &idp, at_corp).await).contains("no account matches"));
    let (code, _) = lina.post("/api/v1/user/identities/corp/unlink", json!({ "password": password("lina") })).await;
    assert_eq!(code, 404);
}

/// A link only lands on the account that started it, in a browser still signed
/// in as that account: a callback alone links nothing.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_link_needs_the_session_that_started_it(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({})).await;
    let lina = srv.user("lina", false).await;
    let mallory = srv.user("mallory", false).await;
    let attacker_at_corp = person("corp-mallory", "mallory@corp.example");

    // the link lina started, finished in a browser that is not signed in
    let url = start_link(&lina, "lina").await;
    let r = come_back(&Client::new(&srv.base), &idp, &url, attacker_at_corp.clone()).await;
    assert!(location(&r).contains("link_error=") && location(&r).contains("no%20longer%20signed%20in"), "{}", location(&r));
    // … and in someone else's
    let url = start_link(&lina, "lina").await;
    let r = come_back(&mallory, &idp, &url, attacker_at_corp.clone()).await;
    assert!(location(&r).contains("no%20longer%20signed%20in"), "{}", location(&r));
    assert!(refusal(&sign_in(&srv, &idp, attacker_at_corp).await).contains("no account matches"));
    for c in [&lina, &mallory] {
        assert!(c.ok_get("/api/v1/user/identities").await[0]["linked_at"].is_null());
    }

    // a token cannot add a way in
    let pat = lina.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let (code, body) =
        Client::with_token(&srv.base, &pat).post("/api/v1/user/identities/corp/link", json!({ "password": password("lina") })).await;
    assert_eq!(code, 403, "{body}");
}

/// An account made by the provider has no password: its last provider stays.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn the_last_way_in_cannot_be_unlinked(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({ "auto_create": true })).await;
    let nora = session_of(&srv, sign_in(&srv, &idp, person("corp-nora", "nora@corp.example")).await).await;
    let (code, body) = nora.post("/api/v1/user/identities/corp/unlink", json!({})).await;
    assert_eq!(code, 403, "{body}");
    assert!(body.to_string().contains("only way into your account"), "{body}");
    // with a password, it can go
    nora.ok_put("/api/v1/user/password", json!({ "new": "nora-has-a-password-1" })).await;
    nora.ok_post("/api/v1/user/identities/corp/unlink", json!({ "password": "nora-has-a-password-1" })).await;
}

/// Without a password, a recent sign-in stands in for it once: one sign-in,
/// one change, whatever is sent as the password.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_recent_sign_in_covers_one_change(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({ "auto_create": true })).await;
    let olga = session_of(&srv, sign_in(&srv, &idp, person("corp-olga", "olga@corp.example")).await).await;
    // setting the app up keeps the sign-in for enabling it
    let setup = olga.ok_post("/api/v1/user/two_factor/setup", json!({ "password": "anything" })).await;
    let code = totp_code(setup["secret"].as_str().unwrap());
    olga.ok_post("/api/v1/user/two_factor/enable", json!({ "code": code, "password": "anything" })).await;
    let (status, body) = olga.post("/api/v1/user/two_factor/recovery_codes", json!({ "password": "anything" })).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(olga.post("/api/v1/user/two_factor/disable", json!({ "password": "" })).await.0, 403);
}

/// `allowed_groups`: only members come in through the provider, to a new
/// account, an existing one or a link.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_provider_can_be_limited_to_groups(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({ "auto_create": true, "allowed_groups": ["engineering", "ops"] })).await;
    let mut gus = person("corp-gus", "gus@corp.example");

    for groups in [json!(null), json!(["sales"]), json!("sales")] {
        gus["groups"] = groups;
        let r = sign_in(&srv, &idp, gus.clone()).await;
        assert!(refusal(&r).contains("not in a group allowed"), "{}", refusal(&r));
    }
    gus["groups"] = json!(["sales", "ops"]);
    assert_eq!(signed_in_as(&srv, sign_in(&srv, &idp, gus.clone()).await).await, "gus");
    // a single name instead of a list
    gus["groups"] = json!("engineering");
    assert_eq!(signed_in_as(&srv, sign_in(&srv, &idp, gus.clone()).await).await, "gus");
    // out of the groups: the provider no longer lets the account in
    gus["groups"] = json!([]);
    assert!(refusal(&sign_in(&srv, &idp, gus).await).contains("not in a group allowed"));

    // linking is held to it too
    let lina = srv.user("lina", false).await;
    let url = start_link(&lina, "lina").await;
    let r = come_back(&lina, &idp, &url, person("corp-lina", "l@corp.example")).await;
    assert!(location(&r).contains("link_error=") && location(&r).contains("not%20in%20a%20group"), "{}", location(&r));
}

/// `admin_groups`: the provider says who is an administrator, at each sign-in
/// and both ways; the last administrator stays one.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_provider_group_makes_administrators(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({ "auto_create": true, "admin_groups": ["forge-admins"] })).await;
    let is_admin = |c: Client| async move { c.ok_get("/api/v1/user").await["is_admin"].as_bool().unwrap() };
    let mut ada = person("corp-ada", "ada@corp.example");
    let mut bob = person("corp-bob", "bob@corp.example");

    assert!(!is_admin(session_of(&srv, sign_in(&srv, &idp, ada.clone()).await).await).await);
    ada["groups"] = json!(["staff", "forge-admins"]);
    assert!(is_admin(session_of(&srv, sign_in(&srv, &idp, ada.clone()).await).await).await);
    // out of the group, but the only administrator: kept
    ada["groups"] = json!(["staff"]);
    assert!(is_admin(session_of(&srv, sign_in(&srv, &idp, ada.clone()).await).await).await);
    // with another administrator around, the rights go
    bob["groups"] = json!(["forge-admins"]);
    let bob_session = session_of(&srv, sign_in(&srv, &idp, bob).await).await;
    assert!(!is_admin(session_of(&srv, sign_in(&srv, &idp, ada).await).await).await);
    let log = bob_session.ok_get("/api/v1/admin/audit?per_page=100").await.to_string();
    assert!(log.contains("admin_granted") && log.contains("admin_revoked"), "{log}");
}

/// `sync_profile`: name and verified email follow the provider, except an
/// address another account holds.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_sign_in_takes_the_profile_from_the_provider(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({ "auto_create": true, "sync_profile": true })).await;
    srv.user("taken", false).await;
    let mut pat = person("corp-pat", "pat@corp.example");
    pat["name"] = json!("Pat Doe");
    let me = session_of(&srv, sign_in(&srv, &idp, pat.clone()).await).await.ok_get("/api/v1/user").await;
    assert_eq!((me["name"].as_str(), me["email"].as_str()), (Some("Pat Doe"), Some("pat@corp.example")));

    pat["name"] = json!("Pat Smith");
    pat["email"] = json!("pat.smith@corp.example");
    let me = session_of(&srv, sign_in(&srv, &idp, pat.clone()).await).await.ok_get("/api/v1/user").await;
    assert_eq!((me["name"].as_str(), me["email"].as_str()), (Some("Pat Smith"), Some("pat.smith@corp.example")));

    // someone else's address, or one the provider did not verify: the old one stays
    pat["email"] = json!("taken@example.com");
    let me = session_of(&srv, sign_in(&srv, &idp, pat.clone()).await).await.ok_get("/api/v1/user").await;
    assert_eq!(me["email"], "pat.smith@corp.example");
    pat["email"] = json!("unverified@corp.example");
    pat["email_verified"] = json!(false);
    let me = session_of(&srv, sign_in(&srv, &idp, pat).await).await.ok_get("/api/v1/user").await;
    assert_eq!(me["email"], "pat.smith@corp.example");
}

/// The provider's discovery document and keys are asked for once, not at
/// every sign-in.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn discovery_is_kept(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({ "auto_create": true })).await;
    for _ in 0..3 {
        assert_eq!(signed_in_as(&srv, sign_in(&srv, &idp, person("corp-dee", "dee@corp.example")).await).await, "dee");
    }
    assert_eq!(idp.discoveries.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// An administrator sees who is linked to what, links someone by their
/// identifier at the provider, and unlinks them.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn an_administrator_links_and_unlinks(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({})).await;
    let admin = srv.user("root", true).await;
    let lina = srv.user("lina", false).await;
    let lina_id = lina.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let providers_of = |name: &'static str| {
        let admin = admin.clone();
        async move {
            let users = admin.ok_get("/api/v1/admin/users").await;
            users.as_array().unwrap().iter().find(|u| u["username"] == name).unwrap()["providers"].clone()
        }
    };
    assert_eq!(providers_of("lina").await, json!([]));

    let path = format!("/api/v1/admin/users/{lina_id}/identities");
    let (code, body) = admin.post(&path, json!({ "provider": "nope", "subject": "corp-lina" })).await;
    assert_eq!(code, 400, "{body}");
    assert_eq!(lina.post(&path, json!({ "provider": "corp", "subject": "corp-lina" })).await.0, 403);
    admin.ok_post(&path, json!({ "provider": "corp", "subject": "corp-lina" })).await;
    assert_eq!(providers_of("lina").await, json!(["corp"]));
    assert_eq!(signed_in_as(&srv, sign_in(&srv, &idp, person("corp-lina", "l@corp.example")).await).await, "lina");
    // one identity per provider and account, one account per identity
    assert_eq!(admin.post(&path, json!({ "provider": "corp", "subject": "corp-other" })).await.0, 409);
    let root_id = admin.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let (code, _) =
        admin.post(&format!("/api/v1/admin/users/{root_id}/identities"), json!({ "provider": "corp", "subject": "corp-lina" })).await;
    assert_eq!(code, 409);

    let (code, body) = admin.delete(&format!("{path}/corp")).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(providers_of("lina").await, json!([]));
    assert!(refusal(&sign_in(&srv, &idp, person("corp-lina", "l@corp.example")).await).contains("no account matches"));
    assert_eq!(admin.delete(&format!("{path}/corp")).await.0, 404);
}

/// With two-factor authentication on, the provider alone changes nothing on
/// the account: what it says is taken over once the code is given.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn the_provider_alone_does_not_change_a_two_factor_account(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({ "auto_create": true, "sync_profile": true, "admin_groups": ["forge-admins"] })).await;
    srv.user("root", true).await;
    let mut vic = person("corp-vic", "vic@corp.example");
    let session = session_of(&srv, sign_in(&srv, &idp, vic.clone()).await).await;
    let setup = session.ok_post("/api/v1/user/two_factor/setup", json!({})).await;
    let secret = setup["secret"].as_str().unwrap();
    session.ok_post("/api/v1/user/two_factor/enable", json!({ "code": totp_code(secret) })).await;

    // whoever holds the provider account moves it to their inbox and into the admin group
    vic["email"] = json!("thief@evil.example");
    vic["groups"] = json!(["forge-admins"]);
    let r = sign_in(&srv, &idp, vic).await;
    let to = location(&r);
    let challenge = to.strip_prefix("/login#totp=").unwrap_or_else(|| panic!("a code is asked for: {to}"));
    let me = session.ok_get("/api/v1/user").await;
    assert_eq!((me["email"].as_str(), me["is_admin"].as_bool()), (Some("vic@corp.example"), Some(false)), "nothing before the code");

    // the code given (by the account's owner): the sign-in is complete, and taken over
    let anon = Client::new(&srv.base);
    let (code, body) = anon.post("/api/v1/auth/login/totp", json!({ "challenge": challenge, "code": totp_code_at(secret, 30) })).await;
    assert_eq!(code, 200, "{body}");
    let me = session.ok_get("/api/v1/user").await;
    assert_eq!((me["email"].as_str(), me["is_admin"].as_bool()), (Some("thief@evil.example"), Some(true)));
}

/// Linking by email uses the account's own address only: a provider identity
/// with one of its other addresses neither signs in to it nor takes the
/// address for a new account.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn an_other_address_does_not_link_an_identity(pool: sqlx::PgPool) {
    let (srv, idp) = with_idp(pool, json!({ "link_existing_by_email": true, "auto_create": true })).await;
    srv.user("alice", false).await;
    let aid = srv.user_id("alice").await;
    forgust_db::user_emails::insert(&srv.state.db, aid, "alice@corp.example", Some("admin")).await.unwrap();
    let r = sign_in(&srv, &idp, person("a-1", "alice@corp.example")).await;
    let why = refusal(&r);
    assert!(why.contains("one of its other addresses"), "{why}");
    let linked: i64 = sqlx::query_scalar("SELECT count(*) FROM identities").fetch_one(&srv.state.db).await.unwrap();
    assert_eq!(linked, 0);
    // her own address still links
    let r = sign_in(&srv, &idp, person("a-1", "alice@example.com")).await;
    assert_eq!(signed_in_as(&srv, r).await, "alice");
}
