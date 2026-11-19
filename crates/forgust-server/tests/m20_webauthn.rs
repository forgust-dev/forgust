//! FOR-20: security keys (a second factor) and passkeys (a sign-in of their
//! own), against a software authenticator that can also misbehave: another
//! origin, no user verification, an old counter, someone else's key.

mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use common::*;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The instance as browsers reach it (the server itself listens elsewhere: the
/// relying party only checks what the authenticator says the page's origin was).
const ORIGIN: &str = "http://localhost:3000";

async fn server(pool: sqlx::PgPool) -> TestServer {
    TestServer::start_with(pool, |c| c.server.external_url = ORIGIN.into()).await
}

/// A software authenticator holding one ES256 credential.
#[derive(Clone)]
struct Authenticator {
    id: Vec<u8>,
    key: SigningKey,
    counter: u32,
    /// The account handle it keeps (a passkey).
    user_handle: Option<Vec<u8>>,
}

impl Authenticator {
    fn new() -> Self {
        let mut id = vec![0u8; 16];
        let mut seed = [0u8; 32];
        rand::fill(&mut id[..]);
        rand::fill(&mut seed[..]);
        Authenticator { id, key: SigningKey::from_slice(&seed).unwrap(), counter: 0, user_handle: None }
    }

    fn client_data(kind: &str, options: &Value, origin: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({ "type": kind, "challenge": options["publicKey"]["challenge"], "origin": origin, "crossOrigin": false }))
            .unwrap()
    }

    fn auth_data(rp_id: &str, flags: u8, counter: u32) -> Vec<u8> {
        let mut d = Sha256::digest(rp_id.as_bytes()).to_vec();
        d.push(flags);
        d.extend_from_slice(&counter.to_be_bytes());
        d
    }

    /// `navigator.credentials.create`: user present, verified when `uv`.
    fn create(&mut self, options: &Value, origin: &str, uv: bool) -> Value {
        let o = &options["publicKey"];
        let rp_id = o["rp"]["id"].as_str().unwrap();
        self.user_handle = Some(B64.decode(o["user"]["id"].as_str().unwrap()).unwrap());
        let point = self.key.verifying_key().to_sec1_point(false);
        // COSE_Key {1: 2 (EC2), 3: -7 (ES256), -1: 1 (P-256), -2: x, -3: y}
        let mut cose = vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21, 0x58, 0x20];
        cose.extend_from_slice(point.x().unwrap());
        cose.extend_from_slice(&[0x22, 0x58, 0x20]);
        cose.extend_from_slice(point.y().unwrap());
        let mut auth = Self::auth_data(rp_id, 0x01 | 0x40 | if uv { 0x04 } else { 0 }, self.counter);
        auth.extend_from_slice(&[0u8; 16]); // AAGUID
        auth.extend_from_slice(&(self.id.len() as u16).to_be_bytes());
        auth.extend_from_slice(&self.id);
        auth.extend_from_slice(&cose);
        // {"fmt": "none", "attStmt": {}, "authData": bytes}
        let mut att = vec![0xa3, 0x63];
        att.extend_from_slice(b"fmt");
        att.push(0x64);
        att.extend_from_slice(b"none");
        att.push(0x67);
        att.extend_from_slice(b"attStmt");
        att.extend_from_slice(&[0xa0, 0x68]);
        att.extend_from_slice(b"authData");
        att.push(0x59);
        att.extend_from_slice(&(auth.len() as u16).to_be_bytes());
        att.extend_from_slice(&auth);
        json!({
            "id": B64.encode(&self.id),
            "rawId": B64.encode(&self.id),
            "type": "public-key",
            "response": {
                "attestationObject": B64.encode(att),
                "clientDataJSON": B64.encode(Self::client_data("webauthn.create", options, origin)),
            },
            "extensions": {},
        })
    }

    /// `navigator.credentials.get`, its counter one step on (or as given).
    fn get(&mut self, options: &Value, origin: &str, uv: bool, counter: Option<u32>) -> Value {
        self.counter = counter.unwrap_or(self.counter + 1);
        let rp_id = options["publicKey"]["rpId"].as_str().unwrap();
        let auth = Self::auth_data(rp_id, 0x01 | if uv { 0x04 } else { 0 }, self.counter);
        let client = Self::client_data("webauthn.get", options, origin);
        let mut signed = auth.clone();
        signed.extend_from_slice(&Sha256::digest(&client));
        let sig: Signature = self.key.sign(&signed);
        json!({
            "id": B64.encode(&self.id),
            "rawId": B64.encode(&self.id),
            "type": "public-key",
            "response": {
                "authenticatorData": B64.encode(auth),
                "clientDataJSON": B64.encode(client),
                "signature": B64.encode(sig.to_der().as_bytes()),
                "userHandle": self.user_handle.as_ref().map(|h| B64.encode(h)),
            },
            "extensions": {},
        })
    }
}

/// Register `auth` on the signed-in account as a `kind`; the answer.
async fn register(c: &Client, username: &str, auth: &mut Authenticator, kind: &str, name: &str) -> (u16, Value) {
    let start = c.ok_post("/api/v1/user/webauthn/register/start", json!({ "kind": kind, "password": password(username) })).await;
    // security keys are asked not to verify their user (presence is enough);
    // passkeys must verify it
    let credential = auth.create(&start["options"], ORIGIN, kind == "passkey");
    c.post("/api/v1/user/webauthn/register/finish", json!({ "ceremony": start["ceremony"], "name": name, "credential": credential })).await
}

/// A POST from someone not signed in, as the raw response (for its session cookie).
async fn anon_send(srv: &TestServer, path: &str, body: Value) -> reqwest::Response {
    Client::new(&srv.base).req(reqwest::Method::POST, path).json(&body).send().await.unwrap()
}

/// A security key's sign-in: what the pending one asks it to sign.
async fn key_question(srv: &TestServer, challenge: &str) -> Value {
    Client::new(&srv.base).ok_post("/api/v1/auth/login/webauthn/start", json!({ "challenge": challenge })).await["options"].clone()
}

async fn key_answer(srv: &TestServer, challenge: &str, credential: Value) -> u16 {
    Client::new(&srv.base).post("/api/v1/auth/login/webauthn", json!({ "challenge": challenge, "credential": credential })).await.0
}

/// Git over HTTP with the account's password: the status.
async fn git_refs(srv: &TestServer, username: &str, project: &str) -> u16 {
    reqwest::Client::new()
        .get(format!("{}/{username}/{project}.git/info/refs?service=git-upload-pack", srv.base))
        .basic_auth(username, Some(password(username)))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn passkey_start(srv: &TestServer) -> Value {
    Client::new(&srv.base).ok_post("/api/v1/auth/passkey/start", json!({})).await
}

async fn passkey_finish(srv: &TestServer, ceremony: &Value, credential: Value) -> reqwest::Response {
    anon_send(srv, "/api/v1/auth/passkey/finish", json!({ "ceremony": ceremony, "credential": credential })).await
}

async fn audited(srv: &TestServer, action: &str) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM audit_events WHERE action = $1)")
        .bind(action)
        .fetch_one(&srv.state.db)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn security_keys_are_a_second_factor(pool: sqlx::PgPool) {
    let srv = server(pool).await;
    let admin = srv.user("root", true).await;
    let ann = srv.user("ann", false).await;
    let ns = ann.ok_get("/api/v1/user").await["namespace_id"].clone();
    ann.ok_post("/api/v1/projects", json!({ "name": "Notes", "path": "notes", "namespace_id": ns, "visibility": "private" })).await;
    assert_eq!(git_refs(&srv, "ann", "notes").await, 200, "her password, before any key");
    let mut yubikey = Authenticator::new();

    // adding one: the password again; the options name this host only
    assert_eq!(ann.post("/api/v1/user/webauthn/register/start", json!({ "kind": "security_key" })).await.0, 403);
    let start = ann.ok_post("/api/v1/user/webauthn/register/start", json!({ "kind": "security_key", "password": password("ann") })).await;
    assert_eq!(start["options"]["publicKey"]["rp"]["id"], "localhost");
    // made on another origin (a phishing page): refused, and the ceremony is spent
    let phished = yubikey.create(&start["options"], "http://localhost:3001", true);
    let body = json!({ "ceremony": start["ceremony"], "name": "Key", "credential": phished });
    assert_eq!(ann.post("/api/v1/user/webauthn/register/finish", body.clone()).await.0, 400);
    assert_eq!(ann.post("/api/v1/user/webauthn/register/finish", body).await.0, 400, "used once");

    let (st, added) = register(&ann, "ann", &mut yubikey, "security_key", "YubiKey 5C").await;
    assert_eq!(st, 200, "{added}");
    assert_eq!(added["credential"]["name"], "YubiKey 5C");
    assert_eq!((added["user"]["two_factor_enabled"].as_bool(), added["user"]["totp_enabled"].as_bool()), (Some(true), Some(false)));
    assert_eq!(added["recovery_codes"].as_array().unwrap().len(), 10, "the first second factor brings recovery codes");
    wait_jobs(&srv.state).await;
    assert!(srv.state.mailer.sent().iter().any(|m| m.to == vec!["ann@example.com".to_string()] && m.subject == "A security key was added"));
    // the same authenticator twice: refused
    let start = ann.ok_post("/api/v1/user/webauthn/register/start", json!({ "kind": "security_key", "password": password("ann") })).await;
    let excluded = start["options"]["publicKey"]["excludeCredentials"].as_array().unwrap();
    assert_eq!(excluded[0]["id"], B64.encode(&yubikey.id));
    let again = yubikey.create(&start["options"], ORIGIN, false);
    let (st, b) = ann
        .post("/api/v1/user/webauthn/register/finish", json!({ "ceremony": start["ceremony"], "name": "Again", "credential": again }))
        .await;
    assert_eq!(st, 409, "{b}");
    // a second key: the codes stay as they are
    let (_, second) = register(&ann, "ann", &mut Authenticator::new(), "security_key", "Spare").await;
    assert_eq!(second["recovery_codes"], json!([]));

    // the account counts as protected: administrators' filter
    let off = admin.ok_get("/api/v1/admin/users?filter=two_factor_off").await;
    assert!(!off.as_array().unwrap().iter().any(|u| u["username"] == "ann"));

    // signing in: the password, then the key
    let ch = challenge(&srv, "ann").await;
    let factors = Client::new(&srv.base).ok_post("/api/v1/auth/login/second_factor", json!({ "challenge": ch })).await;
    assert_eq!(factors, json!({ "totp": false, "security_key": true, "recovery_code": true }));
    let ask = key_question(&srv, &ch).await;
    assert_eq!(ask["publicKey"]["allowCredentials"].as_array().unwrap().len(), 2, "her keys, and only hers");
    // signed on another origin: refused
    let phished = yubikey.get(&ask, "http://evil.localhost:3000", false, None);
    assert_eq!(key_answer(&srv, &ch, phished).await, 401);
    // someone else's key, on her challenge: refused
    let bob = srv.user("bob", false).await;
    let mut bobs = Authenticator::new();
    assert_eq!(register(&bob, "bob", &mut bobs, "security_key", "Bob's").await.0, 200);
    let ask = key_question(&srv, &ch).await;
    let foreign = bobs.get(&ask, ORIGIN, false, None);
    assert_eq!(key_answer(&srv, &ch, foreign).await, 401);
    // hers: signed in, once
    let ask = key_question(&srv, &ch).await;
    let answer = yubikey.get(&ask, ORIGIN, false, None);
    let mut signed_in = Client::new(&srv.base);
    let r = anon_send(&srv, "/api/v1/auth/login/webauthn", json!({ "challenge": ch, "credential": answer })).await;
    assert_eq!(r.status(), 200);
    signed_in.take_session(r).await;
    assert_eq!(signed_in.ok_get("/api/v1/user").await["username"], "ann");
    assert_eq!(key_answer(&srv, &ch, answer).await, 401, "replayed");
    // an answer to an older question: refused
    let ch = challenge(&srv, "ann").await;
    let stale = yubikey.get(&key_question(&srv, &ch).await, ORIGIN, false, None);
    key_question(&srv, &ch).await;
    assert_eq!(key_answer(&srv, &ch, stale).await, 401);
    // a counter that went back (a copy of the key): refused, audited
    let cloned = yubikey.get(&key_question(&srv, &ch).await, ORIGIN, false, Some(1));
    assert_eq!(key_answer(&srv, &ch, cloned).await, 401);
    assert!(audited(&srv, "webauthn_clone_suspected").await);

    // a password alone is no longer enough for git over HTTP
    assert_eq!(git_refs(&srv, "ann", "notes").await, 401);

    // rename, then remove (the password again); with the last key go the codes
    let keys = ann.ok_get("/api/v1/user/webauthn").await;
    assert_eq!(keys["available"], true);
    let ids: Vec<i64> = keys["credentials"].as_array().unwrap().iter().map(|k| k["id"].as_i64().unwrap()).collect();
    assert_eq!(ann.put(&format!("/api/v1/user/webauthn/{}", ids[0]), json!({ "name": "Desk key" })).await.0, 200);
    assert_eq!(ann.put(&format!("/api/v1/user/webauthn/{}", ids[0]), json!({ "name": "  " })).await.0, 400);
    let bobs_id = bob.ok_get("/api/v1/user/webauthn").await["credentials"][0]["id"].as_i64().unwrap();
    assert_eq!(ann.post(&format!("/api/v1/user/webauthn/{bobs_id}/remove"), json!({ "password": password("ann") })).await.0, 404);
    assert_eq!(ann.post(&format!("/api/v1/user/webauthn/{}/remove", ids[0]), json!({ "password": "wrong" })).await.0, 403);
    let u = ann.ok_post(&format!("/api/v1/user/webauthn/{}/remove", ids[0]), json!({ "password": password("ann") })).await;
    assert_eq!(u["two_factor_enabled"], true);
    assert_eq!(ann.ok_get("/api/v1/user/two_factor/recovery_codes").await["remaining"], 10);
    let u = ann.ok_post(&format!("/api/v1/user/webauthn/{}/remove", ids[1]), json!({ "password": password("ann") })).await;
    assert_eq!(u["two_factor_enabled"], false);
    assert_eq!(ann.ok_get("/api/v1/user/two_factor/recovery_codes").await["remaining"], 0);
    assert!(audited(&srv, "webauthn_removed").await);
}

/// An authenticator app still being set up is no second factor: it cannot
/// stand in for the account's security keys, and setting it up asks for the password.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_pending_authenticator_app_is_no_second_factor(pool: sqlx::PgPool) {
    let srv = server(pool).await;
    let ann = srv.user("ann", false).await;
    assert_eq!(register(&ann, "ann", &mut Authenticator::new(), "security_key", "YubiKey").await.0, 200);
    assert_eq!(ann.post("/api/v1/user/two_factor/setup", json!({})).await.0, 403, "the password again");
    let pending = ann.ok_post("/api/v1/user/two_factor/setup", json!({ "password": password("ann") })).await;
    let code = totp_code(pending["secret"].as_str().unwrap());
    let ch = challenge(&srv, "ann").await;
    assert_eq!(Client::new(&srv.base).post("/api/v1/auth/login/totp", json!({ "challenge": ch, "code": code })).await.0, 401);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn passkeys_sign_in_without_a_password(pool: sqlx::PgPool) {
    let srv = server(pool).await;
    let admin = srv.user("root", true).await;
    let cat = srv.user("cat", false).await;
    let mut phone = Authenticator::new();

    // the authenticator is asked to keep it and to verify its user
    let start = cat.ok_post("/api/v1/user/webauthn/register/start", json!({ "kind": "passkey", "password": password("cat") })).await;
    let sel = &start["options"]["publicKey"]["authenticatorSelection"];
    assert_eq!((sel["residentKey"].as_str(), sel["userVerification"].as_str()), (Some("required"), Some("required")));
    // the account's handle is random, not its id or name
    let handle = B64.decode(start["options"]["publicKey"]["user"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(handle.len(), 16);
    // an authenticator that did not verify its user cannot make one
    let unverified = phone.clone().create(&start["options"], ORIGIN, false);
    let (st, _) = cat
        .post("/api/v1/user/webauthn/register/finish", json!({ "ceremony": start["ceremony"], "name": "x", "credential": unverified }))
        .await;
    assert_eq!(st, 400);
    let (st, added) = register(&cat, "cat", &mut phone, "passkey", "Phone").await;
    assert_eq!(st, 200, "{added}");
    assert_eq!(added["recovery_codes"], json!([]));
    // a passkey is no second factor: the password alone still signs in
    assert_eq!(added["user"]["two_factor_enabled"], false);
    let r: Value = Client::new(&srv.base).login("cat", &password("cat")).await;
    assert_eq!(r["two_factor_required"], false);

    // no username asked, nothing offered: the authenticator picks
    let s = passkey_start(&srv).await;
    assert_eq!(s["options"]["publicKey"]["allowCredentials"], json!([]));
    assert!(s["options"].get("mediation").is_none());
    // user not verified: refused
    let a = phone.get(&s["options"], ORIGIN, false, None);
    assert_eq!(passkey_finish(&srv, &s["ceremony"], a).await.status(), 401);
    // the ceremony was spent with it
    let a = phone.get(&s["options"], ORIGIN, true, None);
    assert_eq!(passkey_finish(&srv, &s["ceremony"], a).await.status(), 401);
    // verified: signed in, no password, no second step
    let s = passkey_start(&srv).await;
    let a = phone.get(&s["options"], ORIGIN, true, None);
    let r = passkey_finish(&srv, &s["ceremony"], a.clone()).await;
    assert_eq!(r.status(), 200);
    let mut signed_in = Client::new(&srv.base);
    let v = signed_in.take_session(r).await;
    assert!(v["session"].is_object());
    assert_eq!(signed_in.ok_get("/api/v1/user").await["username"], "cat");
    assert_eq!(passkey_finish(&srv, &s["ceremony"], a).await.status(), 401, "replayed");
    // it claims to be someone else's: refused
    let s = passkey_start(&srv).await;
    let mut liar = phone.clone();
    liar.user_handle = Some(vec![7u8; 16]);
    assert_eq!(passkey_finish(&srv, &s["ceremony"], liar.get(&s["options"], ORIGIN, true, None)).await.status(), 401);
    // another site's assertion: refused
    let s = passkey_start(&srv).await;
    assert_eq!(
        passkey_finish(&srv, &s["ceremony"], phone.get(&s["options"], "https://localhost.evil.test", true, None)).await.status(),
        401
    );

    // a security key is no passkey: it does not sign in alone
    let mut key = Authenticator::new();
    assert_eq!(register(&cat, "cat", &mut key, "security_key", "Key").await.0, 200);
    let s = passkey_start(&srv).await;
    assert_eq!(passkey_finish(&srv, &s["ceremony"], key.get(&s["options"], ORIGIN, true, None)).await.status(), 401);
    // with a second factor now, the passkey still signs in on its own
    let s = passkey_start(&srv).await;
    assert_eq!(passkey_finish(&srv, &s["ceremony"], phone.get(&s["options"], ORIGIN, true, None)).await.status(), 200);

    // a blocked account: refused
    let cat_id = cat.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    sqlx::query("UPDATE users SET state = 'blocked' WHERE id = $1").bind(cat_id).execute(&srv.state.db).await.unwrap();
    let s = passkey_start(&srv).await;
    assert_eq!(passkey_finish(&srv, &s["ceremony"], phone.get(&s["options"], ORIGIN, true, None)).await.status(), 403);
    sqlx::query("UPDATE users SET state = 'active' WHERE id = $1").bind(cat_id).execute(&srv.state.db).await.unwrap();

    // the administrator's reset takes keys and passkeys, and every session
    admin.ok_post(&format!("/api/v1/admin/users/{cat_id}/reset_two_factor"), json!({})).await;
    assert_eq!(signed_in.get("/api/v1/user").await.0, 401);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM webauthn_credentials WHERE user_id = $1")
        .bind(cat_id)
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
    let s = passkey_start(&srv).await;
    assert_eq!(passkey_finish(&srv, &s["ceremony"], phone.get(&s["options"], ORIGIN, true, None)).await.status(), 401);
}

/// A registration finishes in the session that started it; an address someone
/// else registered loses its passkeys to the reset link.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn registrations_stay_with_their_session(pool: sqlx::PgPool) {
    let srv = server(pool).await;
    let dan = srv.user("dan", false).await;
    let mut other = Client::new(&srv.base);
    other.login("dan", &password("dan")).await;
    let mut auth = Authenticator::new();
    let start = dan.ok_post("/api/v1/user/webauthn/register/start", json!({ "kind": "passkey", "password": password("dan") })).await;
    let cred = auth.create(&start["options"], ORIGIN, true);
    let body = json!({ "ceremony": start["ceremony"], "name": "P", "credential": cred });
    assert_eq!(other.post("/api/v1/user/webauthn/register/finish", body.clone()).await.0, 400, "another session");

    assert_eq!(register(&dan, "dan", &mut auth, "passkey", "P").await.0, 200);
    let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = 'dan'").fetch_one(&srv.state.db).await.unwrap();
    let mut tx = srv.state.db.begin().await.unwrap();
    forgust_db::users::forget_credentials(&mut tx, id).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(dan.ok_get("/api/v1/user/webauthn").await["credentials"], json!([]));
}

/// Over plain http on an address, browsers refuse WebAuthn: so does Forgust.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn off_without_https(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let eve = srv.user("eve", false).await;
    assert_eq!(eve.ok_get("/api/v1/user/webauthn").await["available"], false);
    assert_eq!(eve.post("/api/v1/user/webauthn/register/start", json!({ "kind": "passkey", "password": password("eve") })).await.0, 409);
    assert_eq!(Client::new(&srv.base).post("/api/v1/auth/passkey/start", json!({})).await.0, 409);
}
