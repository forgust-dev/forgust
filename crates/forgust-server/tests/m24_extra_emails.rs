//! More addresses per account: confirmed by link, by an administrator on an
//! instance without email, or at once when it trusts its people's addresses.
mod common;
use common::*;
use serde_json::json;

fn link_token(body: &str, page: &str) -> String {
    let i = body.find(&format!("/{page}?token=")).unwrap_or_else(|| panic!("no {page} link in {body}"));
    body[i + page.len() + 8..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

fn last_mail_to(srv: &TestServer, to: &str) -> Option<forgust_server::services::notify::Email> {
    srv.state.mailer.sent().into_iter().rev().find(|m| m.to.iter().any(|t| t == to))
}

/// Who commits made with `email` show as.
async fn linked(srv: &TestServer, email: &str) -> Option<String> {
    let rows = forgust_db::people::usernames_by_email(&srv.state.db, &[email.to_lowercase()]).await.unwrap();
    rows.into_iter().next().map(|(_, u)| u)
}

async fn settings(admin: &Client, signup: bool, auto: bool) {
    admin
        .ok_put(
            "/api/v1/admin/settings",
            json!({ "signup_enabled": signup, "allow_personal_projects": true, "default_branch_name": "main", "auto_confirm_emails": auto }),
        )
        .await;
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn without_email_an_administrator_confirms_them(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.email.smtp_url.clear()).await;
    let admin = srv.user("root", true).await;
    let alice = srv.user("alice", false).await;
    let bob = srv.user("bob", false).await;
    let aid = srv.user_id("alice").await;

    // an import that met alice under her work address, before she added it
    let ns: i64 = sqlx::query_scalar("SELECT namespace_id FROM users WHERE id = $1").bind(aid).fetch_one(&srv.state.db).await.unwrap();
    let import: i64 = sqlx::query_scalar(
        "INSERT INTO imports (user_id, source, base_url, target_namespace_id) VALUES ($1, 'gitlab', 'https://gitlab.example', $2) RETURNING id",
    )
    .bind(aid)
    .bind(ns)
    .fetch_one(&srv.state.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO import_users (import_id, source_user_id, username, email) VALUES ($1, '7', 'alice', 'Alice@Work.example')")
        .bind(import)
        .execute(&srv.state.db)
        .await
        .unwrap();

    // added unconfirmed: nothing links to it, and no link can be sent
    let row = alice.ok_post("/api/v1/user/emails", json!({ "email": "alice@work.example" })).await;
    assert!(row["confirmed_at"].is_null(), "{row}");
    let eid = row["id"].as_i64().unwrap();
    assert_eq!(linked(&srv, "alice@work.example").await, None);
    assert_eq!(forgust_db::user_emails::confirmed_owner(&srv.state.db, "alice@work.example").await.unwrap(), None);
    assert_eq!(alice.post(&format!("/api/v1/user/emails/{eid}/resend"), json!({})).await.0, 409);
    let links: i64 = sqlx::query_scalar("SELECT count(*) FROM user_tokens").fetch_one(&srv.state.db).await.unwrap();
    assert_eq!(links, 0);

    // it is hers, confirmed or not: nobody else takes it, in any way
    assert_eq!(bob.post("/api/v1/user/emails", json!({ "email": "ALICE@work.example" })).await.0, 409);
    let (code, body) =
        bob.put("/api/v1/user", json!({ "name": "Bob", "email": "alice@work.example", "current_password": password("bob") })).await;
    assert_eq!(code, 409, "{body}");
    assert_eq!(admin.post("/api/v1/admin/users", json!({ "username": "carol", "email": "alice@work.example" })).await.0, 409);
    settings(&admin, true, false).await;
    let (code, body) = Client::new(&srv.base)
        .post("/api/v1/auth/register", json!({ "username": "mallory", "email": "alice@work.example", "password": "mallory-password-1" }))
        .await;
    assert_eq!(code, 409, "{body}");
    settings(&admin, false, false).await;
    // nor is it her own address while it is in her list, nor anyone's own one an extra
    let (code, body) =
        alice.put("/api/v1/user", json!({ "name": "Alice", "email": "alice@work.example", "current_password": password("alice") })).await;
    assert_eq!(code, 409, "{body}");
    assert!(body["message"].as_str().unwrap().contains("other addresses"), "{body}");
    assert_eq!(alice.post("/api/v1/user/emails", json!({ "email": "bob@example.com" })).await.0, 409);
    assert_eq!(alice.post("/api/v1/user/emails", json!({ "email": "alice@example.com" })).await.0, 409);
    assert_eq!(alice.post("/api/v1/user/emails", json!({ "email": "not an address" })).await.0, 400);

    // an administrator vouches for it: commits, code owners and imports link to her
    let listed = admin.ok_get(&format!("/api/v1/admin/users/{aid}/emails")).await;
    assert_eq!(listed[0]["email"], "alice@work.example");
    let row = admin.ok_post(&format!("/api/v1/admin/users/{aid}/emails/{eid}/confirm"), json!({})).await;
    assert_eq!(row["confirmed_by"], "admin");
    assert_eq!(admin.post(&format!("/api/v1/admin/users/{aid}/emails/{eid}/confirm"), json!({})).await.0, 409);
    assert_eq!(linked(&srv, "Alice@Work.example").await.as_deref(), Some("alice"));
    assert_eq!(forgust_db::user_emails::confirmed_owner(&srv.state.db, "alice@work.example").await.unwrap(), Some(aid));
    assert!(forgust_db::user_emails::owns(&srv.state.db, aid, "ALICE@work.example").await.unwrap());
    let matched: Option<i64> =
        sqlx::query_scalar("SELECT user_id FROM import_users WHERE import_id = $1").bind(import).fetch_one(&srv.state.db).await.unwrap();
    assert_eq!(matched, Some(aid));
    // and an import that meets the address afterwards matches her at once
    let later = forgust_db::imports::upsert_user(&srv.state.db, import, "8", "alice-old", "", Some("ALICE@work.example")).await.unwrap();
    assert_eq!(later, Some(aid));
    // the audit log names the address by id only
    let events = srv.audit_events("email_confirmed").await;
    let (_, target, details) = events.last().unwrap();
    assert_eq!(*target, Some(aid));
    assert_eq!(details["email_id"], eid);
    assert!(!details.to_string().contains("work.example"), "{details}");

    // an administrator's own: confirmed as they add it (they answer for it), with their password
    let (code, body) = admin.post("/api/v1/user/emails", json!({ "email": "root@work.example" })).await;
    assert_eq!(code, 403, "{body}");
    let row = admin.ok_post("/api/v1/user/emails", json!({ "email": "root@work.example", "current_password": password("root") })).await;
    assert_eq!(row["confirmed_by"], "admin");

    // removed (by her or an administrator): her commits with it are not linked anymore
    admin.ok_get(&format!("/api/v1/admin/users/{aid}/emails")).await;
    assert_eq!(admin.delete(&format!("/api/v1/admin/users/{aid}/emails/{eid}")).await.0, 200);
    assert_eq!(linked(&srv, "alice@work.example").await, None);
    assert_eq!(alice.ok_get("/api/v1/user/emails").await.as_array().unwrap().len(), 0);
    assert_eq!(alice.delete(&format!("/api/v1/user/emails/{eid}")).await.0, 404);
    // and free for whoever it belongs to
    bob.ok_post("/api/v1/user/emails", json!({ "email": "alice@work.example" })).await;
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn trusted_at_once_only_while_sign_up_is_closed(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.email.smtp_url.clear()).await;
    let admin = srv.user("root", true).await;
    let alice = srv.user("alice", false).await;

    // off (the default): it waits for an administrator
    let row = alice.ok_post("/api/v1/user/emails", json!({ "email": "a1@example.org" })).await;
    assert!(row["confirmed_at"].is_null());

    // on, sign-up closed: confirmed as it is added, with the password
    settings(&admin, false, true).await;
    assert_eq!(admin.ok_get("/api/v1/admin/settings").await["auto_confirm_emails"], true);
    let (code, body) = alice.post("/api/v1/user/emails", json!({ "email": "a2@example.org" })).await;
    assert_eq!(code, 403, "{body}");
    let row = alice.ok_post("/api/v1/user/emails", json!({ "email": "a2@example.org", "current_password": password("alice") })).await;
    assert_eq!(row["confirmed_by"], "auto");
    assert_eq!(linked(&srv, "a2@example.org").await.as_deref(), Some("alice"));
    // one added before stays as it was
    assert_eq!(linked(&srv, "a1@example.org").await, None);

    // a save that leaves the switch out keeps it
    admin
        .ok_put(
            "/api/v1/admin/settings",
            json!({ "signup_enabled": false, "allow_personal_projects": true, "default_branch_name": "main" }),
        )
        .await;
    assert_eq!(admin.ok_get("/api/v1/admin/settings").await["auto_confirm_emails"], true);

    // sign-up open: anyone could claim someone's address, so nothing is trusted
    settings(&admin, true, true).await;
    let row = alice.ok_post("/api/v1/user/emails", json!({ "email": "a3@example.org" })).await;
    assert!(row["confirmed_at"].is_null(), "{row}");
    // nor, once closed again, for an account whose own address nobody confirmed
    let (code, body) = Client::new(&srv.base)
        .post("/api/v1/auth/register", json!({ "username": "newbie", "email": "newbie@example.com", "password": "newbie-password-1" }))
        .await;
    assert_eq!(code, 200, "{body}");
    settings(&admin, false, true).await;
    let mut newbie = Client::new(&srv.base);
    newbie.login("newbie", "newbie-password-1").await;
    let row = newbie.ok_post("/api/v1/user/emails", json!({ "email": "n1@example.org", "current_password": "newbie-password-1" })).await;
    assert!(row["confirmed_at"].is_null(), "{row}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn with_email_its_link_confirms_it(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let alice = srv.user("alice", false).await;
    let bob = srv.user("bob", false).await;
    let aid = srv.user_id("alice").await;
    // a move of her own address waits for its link meanwhile
    alice.ok_put("/api/v1/user", json!({ "name": "Alice", "email": "moved@example.com", "current_password": password("alice") })).await;
    let row = alice.ok_post("/api/v1/user/emails", json!({ "email": "alice@work.example" })).await;
    assert!(row["confirmed_at"].is_null());
    let eid = row["id"].as_i64().unwrap();
    wait_jobs(&srv.state).await;
    let first = link_token(&last_mail_to(&srv, "alice@work.example").expect("a link to the new address").body, "verify-email");
    // a new link: the first one stops working
    alice.ok_post(&format!("/api/v1/user/emails/{eid}/resend"), json!({})).await;
    wait_jobs(&srv.state).await;
    let token = link_token(&last_mail_to(&srv, "alice@work.example").unwrap().body, "verify-email");
    assert_ne!(first, token);
    assert_eq!(alice.post("/api/v1/auth/email/confirm", json!({ "token": first })).await.0, 400);
    // only she, signed in, confirms it
    assert_eq!(bob.post("/api/v1/auth/email/confirm", json!({ "token": token })).await.0, 403);
    assert_eq!(linked(&srv, "alice@work.example").await, None);
    assert_eq!(alice.ok_post("/api/v1/auth/email/confirm", json!({ "token": token })).await["email"], "alice@work.example");
    assert_eq!(linked(&srv, "alice@work.example").await.as_deref(), Some("alice"));
    let me = alice.ok_get("/api/v1/user/emails").await;
    assert_eq!(me[0]["confirmed_by"], "link");
    assert_eq!(alice.post(&format!("/api/v1/user/emails/{eid}/resend"), json!({})).await.0, 400);
    // her own address is untouched, and its move still waits for its own link
    let u = alice.ok_get("/api/v1/user").await;
    assert_eq!((u["email"].as_str(), u["unconfirmed_email"].as_str()), (Some("alice@example.com"), Some("moved@example.com")));
    let moved = link_token(&last_mail_to(&srv, "moved@example.com").unwrap().body, "verify-email");
    alice.ok_post("/api/v1/auth/email/confirm", json!({ "token": moved })).await;
    assert_eq!(alice.ok_get("/api/v1/user").await["email"], "moved@example.com");
    assert_eq!(linked(&srv, "alice@work.example").await.as_deref(), Some("alice"));

    // a link for an address removed since confirms nothing
    let row = alice.ok_post("/api/v1/user/emails", json!({ "email": "old@example.org" })).await;
    wait_jobs(&srv.state).await;
    let token = link_token(&last_mail_to(&srv, "old@example.org").unwrap().body, "verify-email");
    alice.delete(&format!("/api/v1/user/emails/{}", row["id"])).await;
    assert_eq!(alice.post("/api/v1/auth/email/confirm", json!({ "token": token })).await.0, 400);
    let events = srv.audit_events("email_removed").await;
    assert!(events.last().unwrap().2["by"].is_null(), "removed by herself");
    let _ = aid;
}

/// Someone registered with another person's address, and added their own as
/// another one: when the owner takes the account back by a password reset,
/// that one goes (its commits are not the owner's).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_reset_that_takes_an_account_back_drops_them(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    settings(&admin, true, false).await;
    let anon = Client::new(&srv.base);
    anon.ok_post(
        "/api/v1/auth/register",
        json!({ "username": "squatter", "email": "victim@example.com", "password": "squatter-password-1" }),
    )
    .await;
    let mut squatter = Client::new(&srv.base);
    squatter.login("squatter", "squatter-password-1").await;
    squatter.ok_post("/api/v1/user/emails", json!({ "email": "squatter@example.org" })).await;
    wait_jobs(&srv.state).await;
    let token = link_token(&last_mail_to(&srv, "squatter@example.org").unwrap().body, "verify-email");
    squatter.ok_post("/api/v1/auth/email/confirm", json!({ "token": token })).await;
    assert_eq!(linked(&srv, "squatter@example.org").await.as_deref(), Some("squatter"));
    // the owner of the address resets the password
    anon.ok_post("/api/v1/auth/password/forgot", json!({ "login": "victim@example.com" })).await;
    wait_jobs(&srv.state).await;
    let reset = link_token(&last_mail_to(&srv, "victim@example.com").unwrap().body, "reset-password");
    anon.ok_post("/api/v1/auth/password/reset", json!({ "token": reset, "password": "the-owners-password-1" })).await;
    assert_eq!(linked(&srv, "squatter@example.org").await, None);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM user_emails").fetch_one(&srv.state.db).await.unwrap();
    assert_eq!(left, 0);
}
