//! The audit log's gap pass: sign-ins and sessions, refusals (throttled),
//! repository reads (a setting), and changes that record what they replaced.

mod common;

use common::*;
use serde_json::{Value, json};

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn sign_ins_sign_outs_and_sessions(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let ann = srv.user("ann", false).await;
    let ann_id = srv.user_id("ann").await;
    assert!(srv.audit_events("login").await.iter().any(|(_, t, d)| *t == Some(ann_id) && d["method"] == "password"));

    // a wrong password
    let (status, _) = Client::new(&srv.base).post("/api/v1/auth/login", json!({ "login": "ann", "password": "wrong" })).await;
    assert_eq!(status, 401);
    let failed = srv.audit_events("login_failed").await;
    assert_eq!(failed.len(), 1);
    assert_eq!((failed[0].1, failed[0].2["reason"].as_str()), (Some(ann_id), Some("password")));

    // signing out
    let (status, _) = ann.post("/api/v1/auth/logout", json!({})).await;
    assert_eq!(status, 200);
    assert_eq!(srv.audit_events("logout").await.iter().filter(|(_, t, _)| *t == Some(ann_id)).count(), 1);

    // a session that runs out is recorded by the sweep, as the system
    let mut again = Client::new(&srv.base);
    again.login("ann", &password("ann")).await;
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 minute' WHERE user_id = $1")
        .bind(ann_id)
        .execute(&srv.state.db)
        .await
        .unwrap();
    admin.ok_post("/api/v1/admin/tasks/cleanup", json!({})).await;
    wait_jobs(&srv.state).await;
    let expired = srv.audit_events("session_expired").await;
    assert_eq!(expired.len(), 1, "{expired:?}");
    assert_eq!((expired[0].0.as_str(), expired[0].1), ("system", Some(ann_id)));
    assert!(expired[0].2["session_id"].is_i64());

    // blocking records the state it replaced, once: the form sends every field
    let path = format!("/api/v1/admin/users/{ann_id}");
    admin.ok_put(&path, json!({ "state": "blocked", "is_admin": false })).await;
    admin.ok_put(&path, json!({ "state": "blocked", "is_admin": false })).await;
    let blocked = srv.audit_events("user_blocked").await;
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].2["previous_state"], "active");
    assert!(srv.audit_events("admin_revoked").await.is_empty(), "not an administrator before either");

    // a blocked account's right password is a refused sign-in too
    let (status, _) = Client::new(&srv.base).post("/api/v1/auth/login", json!({ "login": "ann", "password": password("ann") })).await;
    assert_eq!(status, 403);
    let failed = srv.audit_events("login_failed").await;
    assert_eq!(failed.last().unwrap().2["reason"], "blocked");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn wrong_second_factors_are_recorded(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let bob = srv.user("bob", false).await;
    let setup = bob.ok_post("/api/v1/user/two_factor/setup", json!({ "password": password("bob") })).await;
    let secret = setup["secret"].as_str().unwrap().to_string();
    bob.ok_post("/api/v1/user/two_factor/enable", json!({ "code": totp_code(&secret), "password": password("bob") })).await;

    let challenge = challenge(&srv, "bob").await;
    let (status, _) = Client::new(&srv.base).post("/api/v1/auth/login/totp", json!({ "challenge": challenge, "code": "000000" })).await;
    assert_eq!(status, 401);
    let failed = srv.audit_events("login_failed").await;
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!((failed[0].2["reason"].as_str(), failed[0].2["method"].as_str()), (Some("second_factor"), Some("totp")));
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn refusals_are_recorded_once_a_minute(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let carl = srv.user("carl", false).await;
    let carl_id = srv.user_id("carl").await;

    // a read-only token used to write, three times in a row: one row
    let token =
        carl.ok_post("/api/v1/user/tokens", json!({ "name": "ro", "scopes": ["read_api"] })).await["token"].as_str().unwrap().to_string();
    let ro = Client::with_token(&srv.base, &token);
    for _ in 0..3 {
        let (status, _) = ro.post("/api/v1/projects", json!({ "name": "X", "path": "x" })).await;
        assert_eq!(status, 403);
    }
    let refused = srv.audit_events("token_scope_refused").await;
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(
        (refused[0].1, refused[0].2["method"].as_str(), refused[0].2["path"].as_str()),
        (Some(carl_id), Some("POST"), Some("/api/v1/projects"))
    );

    // another route is another row, written as the route (no ids, no names)
    let (status, _) = ro.post("/api/v1/projects/123/issues", json!({ "title": "x" })).await;
    assert_eq!(status, 403);
    let refused = srv.audit_events("token_scope_refused").await;
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert_eq!(refused[1].2["path"], "/api/v1/projects/{id}/issues", "the route, not the id");

    // the admin API, by someone who is not an administrator
    let (status, _) = carl.get("/api/v1/admin/users").await;
    assert_eq!(status, 403);
    // and by an administrator's token not made for it
    let api =
        admin.ok_post("/api/v1/user/tokens", json!({ "name": "api", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let (status, _) = Client::with_token(&srv.base, &api).get("/api/v1/admin/users").await;
    assert_eq!(status, 403);
    let reasons: Vec<_> =
        srv.audit_events("admin_access_refused").await.into_iter().map(|(_, _, d)| d["reason"].as_str().unwrap().to_string()).collect();
    assert_eq!(reasons, ["not_admin", "token_scope"]);

    // what the catalogue says is served to administrators
    let catalogue = admin.ok_get("/api/v1/admin/audit/catalogue").await;
    assert!(catalogue["version"].as_u64().unwrap() >= 1);
    assert!(catalogue["events"].as_array().unwrap().iter().any(|e| e["name"] == "token_scope_refused"));
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn role_changes_record_what_they_replaced(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    srv.user("dave", false).await;
    let dave = srv.user_id("dave").await;
    let (org, _sub, project) = org_project(&owner).await;

    let members = format!("/api/v1/groups/{org}/members");
    owner.ok_post(&members, json!({ "user_id": dave, "access_level": "developer" })).await;
    owner.ok_post(&members, json!({ "user_id": dave, "access_level": "maintainer" })).await;
    let (status, _) = owner.delete(&format!("{members}/{dave}")).await;
    assert!(status < 300, "{status}");
    let set = srv.audit_events("member_set").await;
    let for_dave: Vec<_> =
        set.iter().filter(|(_, _, d)| d["user_id"] == dave).map(|(_, _, d)| (d["previous_level"].clone(), d["level"].clone())).collect();
    assert_eq!(for_dave, [(Value::Null, json!(30)), (json!(30), json!(40))]);
    let removed = srv.audit_events("member_removed").await;
    assert_eq!(removed.last().unwrap().2, json!({ "user_id": dave, "previous_level": 40 }));

    // a branch rule changed, then removed: each time what it was
    let rules = format!("/api/v1/projects/{project}/protected_branches");
    owner.ok_post(&rules, json!({ "name": "release/*", "push_access_level": "maintainer" })).await;
    let list = owner.ok_post(&rules, json!({ "name": "release/*", "push_access_level": "developer" })).await;
    let protected = srv.audit_events("branch_protected").await;
    let release: Vec<_> = protected.iter().filter(|(_, _, d)| d["name"] == "release/*").collect();
    assert_eq!((release[0].2["push"].as_i64(), &release[0].2["previous"]), (Some(40), &Value::Null), "levels as stored");
    assert_eq!(release[1].2["push"], 30);
    assert_eq!(release[1].2["previous"]["push"], 40);
    let id = list.as_array().unwrap().iter().find(|r| r["name"] == "release/*").unwrap()["id"].as_i64().unwrap();
    let (status, _) = owner.delete(&format!("{rules}/{id}")).await;
    assert_eq!(status, 200);
    let gone = srv.audit_events("branch_unprotected").await;
    assert_eq!((gone[0].2["name"].as_str(), gone[0].2["previous"]["push"].as_i64()), (Some("release/*"), Some(30)));

    // visibility
    let ns = owner.ok_get("/api/v1/user").await["namespace_id"].clone();
    let mine =
        owner.ok_post("/api/v1/projects", json!({ "name": "Notes", "path": "notes", "namespace_id": ns, "visibility": "private" })).await;
    owner.ok_put(&format!("/api/v1/projects/{}", mine["id"]), json!({ "name": "Notes", "visibility": "internal" })).await;
    let updated = srv.audit_events("project_updated").await;
    assert_eq!((updated[0].2["previous_visibility"].as_i64(), updated[0].2["visibility"].as_i64()), (Some(0), Some(10)));
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn repository_reads_only_when_asked(pool: sqlx::PgPool) {
    if !has("git") {
        return;
    }
    let srv = TestServer::start_with(pool, |c| c.audit.repository_reads = true).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let work = srv.work_dir("seed");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    assert!(srv.audit_events("repository_read").await.is_empty(), "a push is no read");

    // two clones in a row: one row, the second held back
    for n in 0..2 {
        let dir = srv.work_dir(&format!("clone{n}"));
        must(git(&dir, &["clone", "-q", &srv.http_url("olivia", "acme/platform/api"), "."]).await);
    }
    let (status, _) = owner.get_bytes(&format!("/api/v1/projects/{project}/repository/archive")).await;
    assert_eq!(status, 200);
    let reads = srv.audit_events("repository_read").await;
    let via: Vec<_> = reads.iter().map(|(_, t, d)| (*t, d["via"].as_str().unwrap().to_string())).collect();
    assert_eq!(via, [(Some(project), "http".to_string()), (Some(project), "archive".to_string())]);

    // public repositories are not recorded
    owner.ok_post("/api/v1/projects", json!({ "name": "Site", "path": "site", "namespace_id": owner.ok_get("/api/v1/user").await["namespace_id"], "visibility": "public" })).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "olivia/site"), "main"]).await);
    let dir = srv.work_dir("public");
    must(git(&dir, &["clone", "-q", &srv.http_url("olivia", "olivia/site"), "."]).await);
    assert_eq!(srv.audit_events("repository_read").await.len(), 2);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn repository_reads_are_off_by_default(pool: sqlx::PgPool) {
    if !has("git") {
        return;
    }
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    org_project(&owner).await;
    let work = srv.work_dir("seed");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    let dir = srv.work_dir("clone");
    must(git(&dir, &["clone", "-q", &srv.http_url("olivia", "acme/platform/api"), "."]).await);
    assert!(srv.audit_events("repository_read").await.is_empty());
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn names_are_looked_up_not_stored(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    srv.user("dave", false).await;
    let dave = srv.user_id("dave").await;
    let (_org, _sub, project) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "user_id": dave, "access_level": "developer" })).await;

    let log = owner.ok_get(&format!("/api/v1/projects/{project}/audit")).await;
    let set = log.as_array().unwrap().iter().find(|e| e["action"] == "member_set" && e["details"]["user_id"] == dave).unwrap();
    assert!(!set["details"].to_string().contains("dave"), "{set}");
    assert_eq!((set["user_name"].as_str(), set["target_name"].as_str()), (Some("dave"), Some("acme/platform/api")));
    let all = admin.ok_get(&format!("/api/v1/admin/audit?target_type=user&target_id={dave}")).await;
    assert!(all.as_array().unwrap().iter().all(|e| e["target_name"] == "dave"), "{all}");
}

/// The migration that took names and addresses out of rows written before.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn older_rows_lose_names_and_addresses(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    for (action, details) in [
        ("member_set", json!({ "user": "dave", "level": 30 })),
        ("member_set", json!({ "user": "gone", "level": 30 })),
        ("lfs_lock_removed", json!({ "path": "a.bin", "owner": "dave" })),
        ("user_deleted", json!({ "username": "gone", "email": "gone@example.com", "projects_deleted": 2 })),
        ("email_changed", json!({ "from": "a@example.com", "email": "b@example.com", "confirmed": false })),
        ("test_email_sent", json!({ "to": "root@example.com" })),
        ("security_scanning_settings", json!({ "by": "root" })),
    ] {
        sqlx::query("INSERT INTO audit_events (principal_kind, action, details) VALUES ('user', $1, $2)")
            .bind(action)
            .bind(details)
            .execute(&srv.state.db)
            .await
            .unwrap();
    }
    sqlx::raw_sql(include_str!("../../forgust-db/migrations/20261120140000_audit_ids.sql")).execute(&srv.state.db).await.unwrap();
    let rows: Vec<Value> = sqlx::query_scalar("SELECT details FROM audit_events WHERE principal_id IS NULL ORDER BY id")
        .fetch_all(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(
        rows,
        [
            json!({ "level": 30 }),
            json!({ "level": 30 }),
            json!({ "path": "a.bin" }),
            json!({ "projects_deleted": 2 }),
            json!({ "confirmed": false }),
            json!({}),
            json!({}),
        ]
    );
}
