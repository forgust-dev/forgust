//! Admin tools. Read-only views: counts and CI activity, storage by
//! kind and project, a scan of the blob store, health, the job queue.

mod common;

use common::*;
use serde_json::json;

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn admin_views_are_for_admins(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    for path in ["/api/v1/admin/overview", "/api/v1/admin/storage", "/api/v1/admin/health", "/api/v1/admin/jobs"] {
        assert_eq!(owner.get(path).await.0, 403, "{path}");
    }
    assert_eq!(owner.post("/api/v1/admin/storage/scan", json!({})).await.0, 403);

    // a pushed repository is measured after the push
    let work = srv.work_dir("adm");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;

    let o = admin.ok_get("/api/v1/admin/overview").await;
    assert_eq!((o["counts"]["users_active"].as_i64(), o["counts"]["projects"].as_i64()), (Some(2), Some(1)), "{o}");
    assert_eq!(o["counts"]["groups"], 2);
    assert_eq!(o["counts"]["users_admins"], 1);
    assert_eq!(o["ci"]["pipelines_per_day"].as_array().unwrap().len(), 30);

    let s = admin.ok_get("/api/v1/admin/storage").await;
    let repo = s["kinds"]["repositories"].as_i64().unwrap();
    assert!(repo > 0, "{s}");
    assert_eq!(s["projects"][0]["project_id"], project);
    assert_eq!(s["projects"][0]["full_path"], "acme/platform/api");
    assert_eq!(s["projects"][0]["repository"], repo);
    assert_eq!(s["namespaces"][0]["full_path"], "acme", "top-level groups: {s}");
    assert_eq!(s["repositories_unmeasured"], 0);
    assert!(s["scan"].is_null());

    // the blob store, listed in the background; one scan at a time
    srv.state.blobs.put_bytes("uploads/x/y.txt", bytes::Bytes::from_static(b"hello")).await.unwrap();
    admin.ok_post("/api/v1/admin/storage/scan", json!({})).await;
    wait_jobs(&srv.state).await;
    let s = admin.ok_get("/api/v1/admin/storage").await;
    let scan = &s["scan"];
    assert_eq!(scan["complete"], true, "{scan}");
    let up = scan["prefixes"].as_array().unwrap().iter().find(|p| p["prefix"] == "uploads").unwrap_or_else(|| panic!("{scan}"));
    assert_eq!((up["objects"].as_i64(), up["bytes"].as_i64()), (Some(1), Some(5)));

    // health: every part answers
    let h = admin.ok_get("/api/v1/admin/health").await;
    let status =
        |n: &str| h["checks"].as_array().unwrap().iter().find(|c| c["name"] == n).unwrap_or_else(|| panic!("{h}"))["status"].clone();
    assert_eq!(status("Database"), "ok");
    assert_eq!(status("Blob store"), "ok");
    assert_eq!(status("Git"), "ok");
    assert_eq!(status("Email"), "warn", "not configured in tests");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn the_scan_reports_what_nothing_references(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let db = &srv.state.db;
    let sql = |q: &'static str| sqlx::query(q);
    let (oid_used, oid_unused, oid_gone) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
    // what the database accounts for
    for oid in [&oid_used, &oid_unused] {
        sql("INSERT INTO lfs_objects (oid, size) VALUES ($1, 2)").bind(oid).execute(db).await.unwrap();
    }
    sql("INSERT INTO lfs_objects_projects (project_id, oid) VALUES ($1, $2)").bind(project).bind(&oid_used).execute(db).await.unwrap();
    sql("INSERT INTO uploads (project_id, secret, filename, size, content_type, blob_key) VALUES ($1, 's', 'f', 2, 'text/plain', 'uploads/1/kept')")
        .bind(project)
        .execute(db)
        .await
        .unwrap();
    sql("UPDATE projects SET avatar_at = now() WHERE id = $1").bind(project).execute(db).await.unwrap();
    sql("INSERT INTO blob_deletions (blob_key) VALUES ('ci/artifacts/9/9/queued')").execute(db).await.unwrap();

    // the bucket: old objects (a day and more), and a recent one
    let dir = srv.state.cfg.blobs_dir();
    let put = |key: &str, old: bool| {
        let p = dir.join(key);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"xy").unwrap();
        if old {
            let then = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86400);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(then).unwrap();
        }
    };
    let kept = [
        format!("lfs/aa/aa/{oid_used}"),
        "uploads/1/kept".into(),
        format!("avatars/project/{project}.png"),
        "ci/artifacts/9/9/queued".into(),
    ];
    for k in &kept {
        put(k, true);
    }
    put("uploads/1/recent", false);
    let gone = [
        ("lfs", format!("lfs/cc/cc/{oid_gone}")),
        ("lfs_unused", format!("lfs/bb/bb/{oid_unused}")),
        ("registry", format!("registry/blobs/sha256/{}", "d".repeat(64))),
        ("registry_uploads", "registry/uploads/6f1c2a52-6e3b-4b1f-9a51-0c6a2a7e1f00/0".into()),
        ("ci", "ci/traces/9/8.log".into()),
        ("uploads", "uploads/1/lost".into()),
        ("avatars", "avatars/user/987654.png".into()),
        ("tmp", "tmp/lfs-x-y".into()),
        ("unrecognized", "misc/thing".into()),
    ];
    for (_, k) in &gone {
        put(k, true);
    }

    admin.ok_post("/api/v1/admin/storage/scan", json!({})).await;
    wait_jobs(&srv.state).await;
    let scan = admin.ok_get("/api/v1/admin/storage").await["scan"].clone();
    assert_eq!(scan["complete"], true, "{scan}");
    let orphans = scan["orphans"].as_array().unwrap();
    for (family, key) in &gone {
        let g = orphans.iter().find(|g| g["family"] == *family).unwrap_or_else(|| panic!("{family}: {scan}"));
        assert_eq!((g["objects"].as_i64(), g["bytes"].as_i64()), (Some(1), Some(2)), "{family}: {g}");
        assert_eq!(g["sample"][0], key.as_str());
    }
    assert_eq!(orphans.len(), gone.len(), "nothing else: {scan}");
    let listed = scan.to_string();
    for k in kept.iter().map(String::as_str).chain(["uploads/1/recent"]) {
        assert!(!listed.contains(k), "{k} is accounted for: {scan}");
    }

    // an avatar goes with its project (it used to stay behind)
    let (s, b) = owner.delete(&format!("/api/v1/projects/{project}")).await;
    assert!(s < 300, "{s} {b}");
    wait_jobs(&srv.state).await;
    let queued: Vec<String> = sqlx::query_scalar("SELECT blob_key FROM blob_deletions").fetch_all(db).await.unwrap();
    let avatar = format!("avatars/project/{project}.png");
    assert!(queued.contains(&avatar) || !dir.join(&avatar).exists(), "{queued:?}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn failed_jobs_show_without_secrets(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    // a job out of attempts, its error carrying a URL with credentials
    sqlx::query("SELECT graphile_worker.add_job('ping', '{}'::json, run_at => now() + interval '1 day', max_attempts => 1)")
        .execute(&srv.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE graphile_worker._private_jobs SET attempts = 1, last_error = $1 WHERE task_id = (SELECT id FROM graphile_worker._private_tasks WHERE identifier = 'ping')")
        .bind("clone failed: https://oauth2:glpat-secret@gitlab.example.com/a.git\nstack trace")
        .execute(&srv.state.db)
        .await
        .unwrap();
    let j = admin.ok_get("/api/v1/admin/jobs").await;
    let ping = j["queues"].as_array().unwrap().iter().find(|q| q["task"] == "ping").unwrap_or_else(|| panic!("{j}"));
    assert_eq!(ping["failed"], 1);
    assert_eq!(j["failed"][0]["task"], "ping");
    assert_eq!(j["failed"][0]["error"], "clone failed: https://***@gitlab.example.com/a.git");
    assert!(!j.to_string().contains("glpat-secret"));
}

/// Slice 2: actions on people, jobs, scheduled tasks, the banner.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn admin_actions(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let rita = srv.user("rita", false).await;
    let rid = rita.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let u = format!("/api/v1/admin/users/{rid}");
    let find = |list: &serde_json::Value| list.as_array().unwrap().iter().find(|x| x["username"] == "rita").cloned();

    // the list: the last sign-in, sessions, tokens; filters
    let tok = rita.ok_post("/api/v1/user/tokens", json!({ "name": "cli", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let r = find(&admin.ok_get("/api/v1/admin/users").await).unwrap();
    assert!(r["last_sign_in_at"].is_string(), "{r}");
    assert_eq!((r["sessions"].as_i64(), r["tokens"].as_i64()), (Some(1), Some(1)), "{r}");
    assert_eq!(admin.ok_get("/api/v1/admin/users?filter=admins").await.as_array().unwrap().len(), 1);
    assert!(find(&admin.ok_get("/api/v1/admin/users?filter=external").await).is_none());
    assert_eq!(rita.get("/api/v1/admin/users").await.0, 403);

    // external: not for administrators
    assert_eq!(admin.put(&u, json!({ "is_external": true })).await.0, 200);
    assert!(find(&admin.ok_get("/api/v1/admin/users?filter=external").await).is_some());
    let root_id = admin.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    assert_eq!(admin.put(&format!("/api/v1/admin/users/{root_id}"), json!({ "is_external": true })).await.0, 400);

    // two-factor reset, tokens revoked, sessions ended
    sqlx::query("UPDATE users SET totp_enabled = true WHERE id = $1").bind(rid).execute(&srv.state.db).await.unwrap();
    admin.ok_post(&format!("{u}/reset_two_factor"), json!({})).await;
    assert_eq!(find(&admin.ok_get("/api/v1/admin/users").await).unwrap()["two_factor_enabled"], false);
    assert_eq!(admin.ok_post(&format!("{u}/revoke_tokens"), json!({})).await["revoked"], 1);
    assert_eq!(Client::with_token(&srv.base, &tok).get("/api/v1/user").await.0, 401);
    admin.ok_post(&format!("{u}/sign_out"), json!({})).await;
    assert_eq!(rita.get("/api/v1/user").await.0, 401);
    assert_eq!(admin.post(&format!("/api/v1/admin/users/{root_id}/sign_out"), json!({})).await.0, 400, "not yourself");

    // confirmation: only for an address waiting for it
    assert_eq!(admin.post(&format!("{u}/resend_confirmation"), json!({})).await.0, 409);
    sqlx::query("UPDATE users SET email_verified_at = NULL WHERE id = $1").bind(rid).execute(&srv.state.db).await.unwrap();
    admin.ok_post(&format!("{u}/resend_confirmation"), json!({})).await;
    wait_jobs(&srv.state).await;
    assert!(srv.state.mailer.sent().iter().any(|m| m.to == vec!["rita@example.com".to_string()] && m.subject.contains("Confirm")));

    // failed jobs: retried (it runs again) or dropped; not running ones
    let add = r#"SELECT (graphile_worker.add_job('ping', '{"marker": "admin-test"}'::json, max_attempts => 1, run_at => now() + interval '1 day')).id"#;
    let a: i64 = sqlx::query_scalar(add).fetch_one(&srv.state.db).await.unwrap();
    let b: i64 = sqlx::query_scalar(add).fetch_one(&srv.state.db).await.unwrap();
    assert_eq!(admin.post(&format!("/api/v1/admin/jobs/{a}/retry"), json!({})).await.0, 409, "not failed");
    sqlx::query("UPDATE graphile_worker._private_jobs SET attempts = 1, last_error = 'boom' WHERE id = ANY($1)")
        .bind(vec![a, b])
        .execute(&srv.state.db)
        .await
        .unwrap();
    admin.ok_post(&format!("/api/v1/admin/jobs/{a}/retry"), json!({})).await;
    assert_eq!(admin.delete(&format!("/api/v1/admin/jobs/{b}")).await.0, 204);
    wait_jobs(&srv.state).await;
    let jobs = admin.ok_get("/api/v1/admin/jobs").await;
    assert!(jobs["failed"].as_array().unwrap().is_empty(), "a ran again, b is gone: {jobs}");

    // scheduled tasks run now; email not configured here
    assert!(admin.ok_get("/api/v1/admin/tasks").await.as_array().unwrap().iter().any(|t| t["task"] == "ci_sweep"));
    for t in ["cleanup", "measure_repositories", "ci_sweep", "maintenance_sweep"] {
        admin.ok_post(&format!("/api/v1/admin/tasks/{t}"), json!({})).await;
    }
    wait_jobs(&srv.state).await;
    let jobs = admin.ok_get("/api/v1/admin/jobs").await;
    assert!(jobs["failed"].as_array().unwrap().is_empty(), "the tasks ran: {jobs}");
    assert_eq!(admin.post("/api/v1/admin/tasks/drop_database", json!({})).await.0, 404);
    assert_eq!(admin.post("/api/v1/admin/health/email", json!({})).await.0, 409);

    // the banner: everyone sees it, signed out too, until it ends
    let anon = Client::new(&srv.base);
    assert!(anon.ok_get("/api/v1/banner").await.is_null());
    assert_eq!(admin.put("/api/v1/admin/banner", json!({ "text": "two\nlines", "tone": "info", "until": null })).await.0, 400);
    assert_eq!(rita.put("/api/v1/admin/banner", json!({ "text": "x", "tone": "info", "until": null })).await.0, 401, "signed out above");
    assert_eq!(
        admin.put("/api/v1/admin/banner", json!({ "text": "Maintenance at 18:00 <b>UTC</b>", "tone": "warn", "until": null })).await.0,
        200
    );
    let b = anon.ok_get("/api/v1/banner").await;
    assert_eq!((b["text"].as_str(), b["tone"].as_str()), (Some("Maintenance at 18:00 <b>UTC</b>"), Some("warn")), "stored as text");
    assert_eq!(admin.put("/api/v1/admin/banner", json!({ "text": "old", "tone": "info", "until": "2020-01-01T00:00:00Z" })).await.0, 200);
    assert!(anon.ok_get("/api/v1/banner").await.is_null(), "ended");
    assert_eq!(admin.ok_get("/api/v1/admin/banner").await["text"], "old");
}

/// Slice 2b: every project and group, repacking, backups.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn projects_groups_and_backups(pool: sqlx::PgPool) {
    let pg = (std::env::var("FORGUST_TEST_PG_DUMP"), std::env::var("FORGUST_TEST_PG_URL"));
    let db_name: String = sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
    let dump = pg.0.clone();
    let srv = TestServer::start_with(pool, move |c| {
        if let Ok(d) = dump {
            c.backup.pg_dump = d;
        }
    })
    .await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let (org, _sub, project) = org_project(&owner).await;
    owner.ok_post("/api/v1/projects", json!({ "name": "Zeta", "path": "zeta", "namespace_id": org })).await;
    let work = srv.work_dir("pb");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;

    assert_eq!(owner.get("/api/v1/admin/projects").await.0, 403);
    let by_size = admin.ok_get("/api/v1/admin/projects?sort=size").await;
    assert_eq!(by_size[0]["full_path"], "acme/platform/api", "{by_size}");
    assert!(by_size[0]["total"].as_i64().unwrap() > 0);
    let by_name = admin.ok_get("/api/v1/admin/projects?sort=name").await;
    assert_eq!(by_name[1]["full_path"], "acme/zeta");
    assert_eq!(admin.ok_get("/api/v1/admin/projects?search=zet").await.as_array().unwrap().len(), 1);
    let groups = admin.ok_get("/api/v1/admin/groups").await;
    let acme = groups.as_array().unwrap().iter().find(|g| g["full_path"] == "acme").unwrap();
    assert_eq!((acme["projects"].as_i64(), acme["subgroups"].as_i64(), acme["members"].as_i64()), (Some(2), Some(1), Some(1)), "{acme}");
    admin.ok_post(&format!("/api/v1/admin/projects/{project}/maintenance"), json!({})).await;

    // backups: the database and the repositories, listed, deleted
    assert_eq!(admin.delete("/api/v1/admin/backups/..%2Fdata").await.0, 404);
    if let (Ok(_), Ok(url)) = pg {
        // SAFETY: only this test in this binary reads the variable.
        unsafe { std::env::set_var("FORGUST_BACKUP_DATABASE_URL", format!("{url}/{db_name}")) };
        admin.ok_post("/api/v1/admin/backups", json!({})).await;
        wait_jobs(&srv.state).await;
        let b = admin.ok_get("/api/v1/admin/backups").await;
        assert_eq!(b["running"], false);
        let first = &b["backups"][0];
        assert_eq!((first["complete"].as_bool(), first["repositories"].as_i64()), (Some(true), Some(2)), "{b}");
        assert!(first["bytes"].as_u64().unwrap() > 0);
        let name = first["name"].as_str().unwrap();
        assert_eq!(admin.delete(&format!("/api/v1/admin/backups/{name}")).await.0, 204);
        assert!(admin.ok_get("/api/v1/admin/backups").await["backups"].as_array().unwrap().is_empty());
    } else {
        eprintln!("backup itself not run: FORGUST_TEST_PG_DUMP / FORGUST_TEST_PG_URL not set");
    }
}

/// Slice 3: deleting a person. Their work stays without its author.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn deleting_a_user(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let dave = srv.user("dave", false).await;
    let did = dave.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let (_org, _sub, project) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    let issue = dave.ok_post(&format!("/api/v1/projects/{project}/issues"), json!({ "title": "From dave" })).await;
    let solo = dave.ok_post("/api/v1/groups", json!({ "name": "Solo", "path": "solo" })).await["id"].as_i64().unwrap();
    let mine = dave.ok_post("/api/v1/projects", json!({ "name": "Mine", "path": "mine" })).await["id"].as_i64().unwrap();
    let u = format!("/api/v1/admin/users/{did}");

    assert_eq!(owner.delete(&u).await.0, 403);
    let root_id = admin.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    assert_eq!(admin.delete(&format!("/api/v1/admin/users/{root_id}")).await.0, 400, "not yourself");
    // the only Owner of a group: someone else first
    let (s, b) = admin.delete(&u).await;
    assert_eq!(s, 409);
    assert!(b.to_string().contains("only Owner of solo"), "{b}");
    dave.ok_post(&format!("/api/v1/groups/{solo}/members"), json!({ "username": "olivia", "access_level": "owner" })).await;
    // personal projects: transferred, or deleted with the account
    let (s, b) = admin.delete(&u).await;
    assert_eq!(s, 409);
    assert!(b.to_string().contains("dave/mine"), "{b}");
    let (s, r) = admin.delete(&format!("{u}?delete_projects=true")).await;
    assert_eq!(s, 200, "{r}");
    assert_eq!((r["deleted"].as_str(), r["projects_deleted"].as_i64()), (Some("dave"), Some(1)));

    // gone: their session, their project, their name is free again
    assert_eq!(dave.get("/api/v1/user").await.0, 401);
    assert_eq!(admin.get(&format!("/api/v1/projects/{mine}")).await.0, 404);
    assert_eq!(admin.get("/api/v1/users/dave").await.0, 404);
    let again = srv.user("dave", false).await;
    again.ok_post("/api/v1/projects", json!({ "name": "Mine", "path": "mine" })).await;
    // what they wrote stays, without its author; the group keeps its other Owner
    let i = owner.ok_get(&format!("/api/v1/projects/{project}/issues/{}", issue["iid"])).await;
    assert_eq!(i["title"], "From dave");
    assert!(i["author_username"].is_null(), "{i}");
    assert_eq!(owner.get(&format!("/api/v1/groups/{solo}")).await.0, 200);
    let audit = admin.ok_get("/api/v1/admin/audit").await.to_string();
    assert!(audit.contains("user_deleted"), "{audit}");
}

/// Slice 3c: an administrator signs in as someone else, audited, without
/// minting credentials, then comes back.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn impersonation(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let other_admin = srv.user("ada", true).await;
    let owner = srv.user("olivia", false).await;
    let dave = srv.user("dave", false).await;
    let did = dave.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let ada = other_admin.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let (_org, _sub, project) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": "dave", "access_level": "maintainer" })).await;

    assert_eq!(owner.post(&format!("/api/v1/admin/users/{did}/impersonate"), json!({})).await.0, 403);
    assert_eq!(admin.post(&format!("/api/v1/admin/users/{ada}/impersonate"), json!({})).await.0, 400, "not an admin");
    let tok = admin.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    assert_eq!(
        Client::with_token(&srv.base, &tok).post(&format!("/api/v1/admin/users/{did}/impersonate"), json!({})).await.0,
        403,
        "a browser session only"
    );

    // signed in as dave: the session says so; the admin's own session ended
    let old = admin.clone();
    let resp = admin.req(reqwest::Method::POST, &format!("/api/v1/admin/users/{did}/impersonate")).json(&json!({})).send().await.unwrap();
    let mut as_dave = admin.clone();
    let r = as_dave.take_session(resp).await;
    assert_eq!(r["impersonating"], "dave");
    let s = as_dave.ok_get("/api/v1/session").await;
    as_dave.csrf = Some(s["csrf_token"].as_str().unwrap().into());
    assert_eq!((s["user"]["username"].as_str(), s["impersonator"]["username"].as_str()), (Some("dave"), Some("root")));
    assert_eq!(old.get("/api/v1/user").await.0, 401);

    // dave's rights, audited as the admin's
    let issue = as_dave.ok_post(&format!("/api/v1/projects/{project}/issues"), json!({ "title": "Checked as dave" })).await;
    assert_eq!(issue["author_username"], "dave");
    // nothing that outlives the impersonation, nothing about the account
    for (path, body) in [
        ("/api/v1/user/tokens".to_string(), json!({ "name": "x", "scopes": ["api"] })),
        (
            "/api/v1/user/keys".to_string(),
            json!({ "title": "k", "key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl x" }),
        ),
        (format!("/api/v1/projects/{project}/deploy_tokens"), json!({ "name": "d", "scopes": ["read_repository"] })),
        (format!("/api/v1/projects/{project}/access_tokens"), json!({ "name": "b", "access_level": "developer", "scopes": ["api"] })),
        (format!("/api/v1/projects/{project}/runners"), json!({ "description": "r" })),
    ] {
        let (st, b) = as_dave.post(&path, body).await;
        assert_eq!(st, 403, "POST {path} while impersonating: {b}");
    }
    assert_eq!(as_dave.put("/api/v1/user", json!({ "name": "Hacked", "email": "evil@example.com" })).await.0, 403);
    assert_eq!(as_dave.put("/api/v1/user/password", json!({ "current": "x", "new": "whatever-long-1" })).await.0, 403);
    assert_eq!(as_dave.get("/api/v1/admin/users").await.0, 403, "dave is no admin");

    // every change made meanwhile is audited, with the admin
    let audit = other_admin.ok_get("/api/v1/admin/audit").await;
    assert!(
        audit.as_array().unwrap().iter().any(|e| e["action"] == "impersonated_request"
            && e["details"]["path"] == format!("/api/v1/projects/{project}/issues")
            && e["details"]["method"] == "POST"),
        "{audit}"
    );

    // back as root, in a new session
    let resp = as_dave.req(reqwest::Method::POST, "/api/v1/auth/stop_impersonating").json(&json!({})).send().await.unwrap();
    let mut back = as_dave.clone();
    assert_eq!(back.take_session(resp).await["back_as"], "root");
    let s = back.ok_get("/api/v1/session").await;
    back.csrf = Some(s["csrf_token"].as_str().unwrap().into());
    assert_eq!((s["user"]["username"].as_str(), s["impersonator"].is_null()), (Some("root"), true));
    assert_eq!(as_dave.get("/api/v1/user").await.0, 401, "the impersonation session is gone");
    let audit = back.ok_get("/api/v1/admin/audit").await;
    let root_id = back.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    // that session is no fresh sign-in (changes needing one ask again)
    let fresh: bool = sqlx::query_scalar("SELECT max(created_at) > now() - interval '10 minutes' FROM sessions WHERE user_id = $1")
        .bind(root_id)
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert!(!fresh);
    let started = audit.as_array().unwrap().iter().find(|e| e["action"] == "impersonation_started").unwrap();
    assert_eq!(started["target_id"], did);
    let stopped = audit.as_array().unwrap().iter().find(|e| e["action"] == "impersonation_stopped").unwrap();
    assert_eq!(stopped["details"]["impersonated_by"], root_id, "{stopped}");
}

/// An impersonation ends when its administrator stops being one.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn impersonation_ends_with_the_admins_rights(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let other = srv.user("ada", true).await;
    let dave = srv.user("dave", false).await;
    let did = dave.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let root_id = admin.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let resp = admin.req(reqwest::Method::POST, &format!("/api/v1/admin/users/{did}/impersonate")).json(&json!({})).send().await.unwrap();
    let mut as_dave = admin.clone();
    as_dave.take_session(resp).await;
    assert_eq!(as_dave.ok_get("/api/v1/user").await["username"], "dave");
    // another admin takes root's rights away: the impersonation stops working
    assert_eq!(other.put(&format!("/api/v1/admin/users/{root_id}"), json!({ "is_admin": false })).await.0, 200);
    assert_eq!(as_dave.get("/api/v1/user").await.0, 401);
}

/// An administrator's token reaches the admin API only with `admin_mode`
/// (an `api` token handed to some application must not create admins), and
/// tokens never mint lasting credentials (deploy keys, runners).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn tokens_need_admin_mode_and_cannot_mint_credentials(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let user = srv.user("ursula", false).await;
    let tok = |c: &Client, scopes: serde_json::Value| {
        let c = c.clone();
        async move { c.ok_post("/api/v1/user/tokens", json!({ "name": "t", "scopes": scopes })).await["token"].as_str().unwrap().to_string() }
    };
    let api = Client::with_token(&srv.base, &tok(&admin, json!(["api"])).await);
    let new_admin = json!({ "username": "eve", "email": "eve@example.com", "name": "", "password": "eve-password-123", "is_admin": true });
    assert_eq!(api.post("/api/v1/admin/users", new_admin.clone()).await.0, 403);
    assert_eq!(api.get("/api/v1/admin/users").await.0, 403);
    let admin_mode = Client::with_token(&srv.base, &tok(&admin, json!(["api", "admin_mode"])).await);
    admin_mode.ok_post("/api/v1/admin/users", new_admin).await;
    assert_eq!(
        user.post("/api/v1/user/tokens", json!({ "name": "t", "scopes": ["api", "admin_mode"] })).await.0,
        400,
        "admin_mode is for administrators"
    );

    let p = user.ok_post("/api/v1/projects", json!({ "name": "P", "path": "p" })).await["id"].as_i64().unwrap();
    let user_api = Client::with_token(&srv.base, &tok(&user, json!(["api"])).await);
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl x";
    assert_eq!(
        user_api.post(&format!("/api/v1/projects/{p}/deploy_keys"), json!({ "title": "k", "key": key, "can_push": true })).await.0,
        403
    );
    assert_eq!(
        user_api.post(&format!("/api/v1/projects/{p}/deploy_tokens"), json!({ "name": "d", "scopes": ["read_repository"] })).await.0,
        403
    );
    assert_eq!(user_api.post(&format!("/api/v1/projects/{p}/runners"), json!({ "description": "r" })).await.0, 403);
    // from the browser session, fine
    user.ok_post(&format!("/api/v1/projects/{p}/runners"), json!({ "description": "r" })).await;
}

/// A membership with an end date ends on that date (access is materialised,
/// so the cleanup task removes it and recomputes), and so does a share.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn memberships_and_shares_expire(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let temp = srv.user("tim", false).await;
    let (org, _sub, p) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "tim", "access_level": "developer" })).await;
    let other = owner.ok_post("/api/v1/groups", json!({ "name": "Partners", "path": "partners" })).await["id"].as_i64().unwrap();
    let pat = srv.user("pat", false).await;
    owner.ok_post(&format!("/api/v1/groups/{other}/members"), json!({ "username": "pat", "access_level": "developer" })).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/shares"), json!({ "group_id": other, "max_access_level": "reporter" })).await;
    assert_eq!(temp.get(&format!("/api/v1/projects/{p}")).await.0, 200);
    assert_eq!(pat.get(&format!("/api/v1/projects/{p}")).await.0, 200);
    // their end dates pass
    sqlx::query(
        "UPDATE members SET expires_at = now() - interval '1 second' WHERE user_id = (SELECT id FROM users WHERE username = 'tim')",
    )
    .execute(&srv.state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE group_group_links SET expires_at = now() - interval '1 second'").execute(&srv.state.db).await.unwrap();
    let mut conn = srv.state.db.acquire().await.unwrap();
    let ended = forgust_db::members::expire(&mut conn).await.unwrap();
    assert!(ended.len() >= 2, "tim, and the partners group's members: {ended:?}");
    assert_eq!(temp.get(&format!("/api/v1/projects/{p}")).await.0, 404);
    assert_eq!(pat.get(&format!("/api/v1/projects/{p}")).await.0, 404);
    assert!(forgust_db::members::expire(&mut conn).await.unwrap().is_empty(), "once");
}

/// Every task Admin > Jobs can run now actually runs, and the scheduled ones
/// run with the payloads the queue gives them (they take no arguments; cron
/// hands them `{}`, which a unit struct used to refuse).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn run_now_tasks_run(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let tasks = admin.ok_get("/api/v1/admin/tasks").await;
    for t in tasks.as_array().unwrap() {
        let name = t["task"].as_str().unwrap();
        admin.ok_post(&format!("/api/v1/admin/tasks/{name}"), json!({})).await;
    }
    wait_jobs(&srv.state).await;
    // as the queue really hands them over: cron jobs without a payload get
    // `{}` (the cause of "invalid type: map, expected unit struct CiSweep")
    for task in ["ci_sweep", "cleanup", "maintenance_sweep", "measure_repositories", "blob_scan"] {
        for payload in [json!({}), json!({ "_cron": { "ts": "2026-11-12T16:54:00Z", "backfilled": false } })] {
            sqlx::query("SELECT graphile_worker.add_job($1, $2::json, max_attempts => 1)")
                .bind(task)
                .bind(&payload)
                .execute(&srv.state.db)
                .await
                .unwrap();
        }
    }
    wait_jobs(&srv.state).await;
    let failed: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT t.identifier, j.last_error FROM graphile_worker._private_jobs j JOIN graphile_worker._private_tasks t ON t.id = j.task_id
          WHERE j.last_error IS NOT NULL",
    )
    .fetch_all(&srv.state.db)
    .await
    .unwrap();
    assert!(failed.is_empty(), "{failed:?}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn administrators_hear_of_new_releases(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    assert_eq!(owner.get("/api/v1/admin/update").await.0, 403);

    let u = admin.ok_get("/api/v1/admin/update").await;
    assert_eq!(u["current"], env!("CARGO_PKG_VERSION"));
    assert!(u["available"].is_null(), "nothing checked yet: {u}");

    // what the worker keeps from GitHub
    sqlx::query("UPDATE instance_settings SET latest_version = '99.0.0', latest_release_url = 'https://example.com/v99'")
        .execute(&srv.state.db)
        .await
        .unwrap();
    let u = admin.ok_get("/api/v1/admin/update").await;
    assert_eq!(u["available"], "99.0.0");
    assert_eq!(u["url"], "https://example.com/v99");

    // turned off: nothing to tell
    let mut settings = admin.ok_get("/api/v1/admin/settings").await;
    assert_eq!(settings["update_check_enabled"], true);
    settings["update_check_enabled"] = json!(false);
    admin.ok_put("/api/v1/admin/settings", settings.clone()).await;
    assert!(admin.ok_get("/api/v1/admin/update").await["available"].is_null());

    // a client that does not know the switch leaves it off
    let older = json!({
        "signup_enabled": settings["signup_enabled"],
        "allow_personal_projects": settings["allow_personal_projects"],
        "default_branch_name": settings["default_branch_name"],
    });
    let saved = admin.ok_put("/api/v1/admin/settings", older).await;
    assert_eq!(saved["update_check_enabled"], false);
    assert_eq!(saved["mcp_enabled"], settings["mcp_enabled"]);
}

/// A GitHub stand-in answering `releases/latest` with `tag`.
async fn releases(tag: &'static str) -> String {
    let app = axum::Router::new().route(
        "/releases/latest",
        axum::routing::get(move || async move {
            axum::Json(json!({ "tag_name": tag, "html_url": format!("https://github.com/forgust-dev/forgust/releases/tag/{tag}") }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/releases/latest")
}

/// The worker asks at start (no 6 hours to wait), keeps the release's own page,
/// and asks again when the check is turned back on.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn the_release_check_asks_at_start_and_when_turned_on(pool: sqlx::PgPool) {
    let url = releases("v99.0.0").await;
    let srv = TestServer::start_with(pool, |c| c.server.release_check_url = url).await;
    let admin = srv.user("root", true).await;
    wait_jobs(&srv.state).await;
    let u = admin.ok_get("/api/v1/admin/update").await;
    assert_eq!(u["available"], "99.0.0", "{u}");
    assert_eq!(u["url"], "https://github.com/forgust-dev/forgust/releases/tag/v99.0.0");

    let mut settings = admin.ok_get("/api/v1/admin/settings").await;
    settings["update_check_enabled"] = json!(false);
    admin.ok_put("/api/v1/admin/settings", settings.clone()).await;
    sqlx::query("UPDATE instance_settings SET latest_version = NULL, latest_release_url = NULL").execute(&srv.state.db).await.unwrap();
    settings["update_check_enabled"] = json!(true);
    admin.ok_put("/api/v1/admin/settings", settings).await;
    wait_jobs(&srv.state).await;
    assert_eq!(admin.ok_get("/api/v1/admin/update").await["available"], "99.0.0");
}

/// A release check an administrator runs from Admin > Jobs fails when it
/// cannot reach GitHub; the one at start only logs it.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn a_release_check_run_now_reports_failure(pool: sqlx::PgPool) {
    // nothing listens on port 1
    let srv = TestServer::start_with(pool, |c| c.server.release_check_url = "http://127.0.0.1:1/releases/latest".into()).await;
    let admin = srv.user("root", true).await;
    wait_jobs(&srv.state).await;
    let failed = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT j.last_error FROM graphile_worker._private_jobs j JOIN graphile_worker._private_tasks t ON t.id = j.task_id
              WHERE t.identifier = 'update_check' AND j.last_error IS NOT NULL",
        )
        .fetch_all(&srv.state.db)
        .await
        .unwrap()
    };
    assert!(failed().await.is_empty(), "the check at start only logs");

    admin.ok_post("/api/v1/admin/tasks/update_check", json!({})).await;
    wait_jobs(&srv.state).await;
    let errors = failed().await;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("release check failed"), "{errors:?}");
}
