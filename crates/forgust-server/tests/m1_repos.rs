//! M1: auth, orgs and groups, repos, git over HTTP and SSH, push rules.

mod common;

use common::*;
use serde_json::json;

/// The M1 exit criteria, end to end.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn push_rules_and_moves(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let _dev = srv.user("dave", false).await;
    let (org, _sub, proj) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "dave", "access_level": "developer" })).await;

    let work = srv.work_dir("push");
    seed_repo(&work).await;

    // the owner (Owner role) pushes the protected default branch
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);

    // a Developer cannot push to the protected default branch...
    std::fs::write(work.join("dev.txt"), "dev\n").unwrap();
    must(git(&work, &["add", "dev.txt"]).await);
    must(git(&work, &["commit", "-q", "-m", "dev change"]).await);
    let rejected = git(&work, &["push", &srv.http_url("dave", "acme/platform/api"), "main"]).await;
    assert!(!rejected.ok);
    assert!(rejected.stderr.contains("not allowed to push code to protected branch main"), "{}", rejected.stderr);

    // ...but can push a feature branch
    must(git(&work, &["push", "-q", &srv.http_url("dave", "acme/platform/api"), "main:feature/login"]).await);

    // force pushes and deletions of protected branches are refused, even for owners
    must(git(&work, &["reset", "-q", "--hard", "HEAD~2"]).await);
    std::fs::write(work.join("other.txt"), "x\n").unwrap();
    must(git(&work, &["add", "other.txt"]).await);
    must(git(&work, &["commit", "-q", "-m", "diverge"]).await);
    let force = git(&work, &["push", "-f", &srv.http_url("olivia", "acme/platform/api"), "main"]).await;
    assert!(!force.ok && force.stderr.contains("force push"), "{}", force.stderr);
    let del = git(&work, &["push", &srv.http_url("olivia", "acme/platform/api"), ":main"]).await;
    assert!(!del.ok && del.stderr.contains("cannot delete the protected branch"), "{}", del.stderr);

    // unknown users and wrong passwords get 401 / not found, never data
    let anon = git(&work, &["ls-remote", &format!("{}/acme/platform/api.git", srv.base)]).await;
    assert!(!anon.ok);
    let stranger = srv.user("sam", false).await;
    drop(stranger);
    let hidden = git(&work, &["ls-remote", &srv.http_url("sam", "acme/platform/api")]).await;
    assert!(!hidden.ok && (hidden.stderr.contains("not found") || hidden.stderr.contains("404")), "{}", hidden.stderr);

    // moving the project keeps the old remote URL working (redirect)
    owner.ok_post(&format!("/api/v1/projects/{proj}/transfer"), json!({ "namespace_id": org })).await;
    let moved = must(git(&work, &["ls-remote", &srv.http_url("dave", "acme/platform/api")]).await);
    assert!(moved.contains("refs/heads/feature/login"));
    let detail = owner.ok_get(&format!("/api/v1/projects/{proj}")).await;
    assert_eq!(detail["full_path"], "acme/api");
    assert_eq!(detail["redirects"][0], "acme/platform/api");

    // renaming the group moves everything under it
    owner.ok_post(&format!("/api/v1/groups/{org}/path"), json!({ "path": "acme-corp" })).await;
    let renamed = must(git(&work, &["ls-remote", &srv.http_url("dave", "acme/platform/api")]).await);
    assert!(renamed.contains("refs/heads/main"));
    let (s, v) = owner.get("/api/v1/resolve?path=acme/api").await;
    assert_eq!(s, 200);
    assert_eq!(v["full_path"], "acme-corp/api");
    assert_eq!(v["redirected"], true);

    // the push was recorded in the audit log, and the rejection too
    wait_jobs(&srv.state).await;
    let audit = owner.ok_get(&format!("/api/v1/projects/{proj}/audit")).await;
    assert!(audit.as_array().unwrap().iter().any(|e| e["action"] == "push_rejected"));
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn nested_groups_permissions_and_sharing(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let reporter = srv.user("rita", false).await;
    let outsider = srv.user("otto", false).await;
    let (org, sub, proj) = org_project(&owner).await;

    // nesting up to 20 levels, then refused
    let mut parent = sub;
    for i in 3..=20 {
        let g = owner.ok_post("/api/v1/groups", json!({ "name": format!("L{i}"), "path": format!("l{i}"), "parent_id": parent })).await;
        parent = g["id"].as_i64().unwrap();
    }
    let (s, v) = owner.post("/api/v1/groups", json!({ "name": "too deep", "path": "deep", "parent_id": parent })).await;
    assert_eq!(s, 400, "{v}");

    // reserved and invalid paths
    assert_eq!(owner.post("/api/v1/groups", json!({ "name": "API", "path": "api" })).await.0, 400);
    assert_eq!(owner.post("/api/v1/groups", json!({ "name": "x", "path": "x.git" })).await.0, 400);
    assert_eq!(owner.post("/api/v1/groups", json!({ "name": "dup", "path": "ACME" })).await.0, 409);

    // a private project is invisible to non-members
    assert_eq!(outsider.get(&format!("/api/v1/projects/{proj}")).await.0, 404);

    // a Reporter in the org can read code in a sub-subgroup project, not push
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let d = reporter.ok_get(&format!("/api/v1/projects/{proj}")).await;
    assert_eq!(d["access_level"], "reporter");
    assert_eq!(d["can_read_code"], true);
    assert_eq!(d["can_push"], false);

    // a Maintainer cannot grant Owner
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "rita", "access_level": "maintainer" })).await;
    let (s, _) = reporter.post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "otto", "access_level": "owner" })).await;
    assert_eq!(s, 403);

    // the last owner cannot leave
    let me = owner.ok_get("/api/v1/user").await;
    assert_eq!(owner.delete(&format!("/api/v1/groups/{org}/members/{}", me["id"])).await.0, 409);

    // share the project with another org's group, capped at reporter
    let other = outsider.ok_post("/api/v1/groups", json!({ "name": "Ops", "path": "ops" })).await;
    let (s, v) =
        owner.post(&format!("/api/v1/projects/{proj}/shares"), json!({ "group_id": other["id"], "max_access_level": "reporter" })).await;
    // the owner must be able to see the target group: it is private to otto
    assert_eq!(s, 404, "{v}");
    outsider.put(&format!("/api/v1/groups/{}", other["id"]), json!({ "name": "Ops", "visibility": "internal" })).await;
    owner.ok_post(&format!("/api/v1/projects/{proj}/shares"), json!({ "group_id": other["id"], "max_access_level": "reporter" })).await;
    let d = outsider.ok_get(&format!("/api/v1/projects/{proj}")).await;
    assert_eq!(d["access_level"], "reporter");
    owner.delete(&format!("/api/v1/projects/{proj}/shares/{}", other["id"])).await;
    assert_eq!(outsider.get(&format!("/api/v1/projects/{proj}")).await.0, 404);

    // internal projects are visible to every signed-in user
    owner.put(&format!("/api/v1/groups/{org}"), json!({ "name": "Acme", "visibility": "internal" })).await;
    owner.put(&format!("/api/v1/groups/{sub}"), json!({ "name": "Platform", "visibility": "internal" })).await;
    let (s, v) = owner.put(&format!("/api/v1/projects/{proj}"), json!({ "name": "API", "visibility": "public" })).await;
    assert_eq!(s, 400, "public inside an internal group must be refused: {v}");
    owner.put(&format!("/api/v1/projects/{proj}"), json!({ "name": "API", "visibility": "internal" })).await;
    assert_eq!(outsider.get(&format!("/api/v1/projects/{proj}")).await.0, 200);
    assert_eq!(Client::new(&srv.base).get(&format!("/api/v1/projects/{proj}")).await.0, 404);

    // org policy: maximum visibility
    let (s, _) = owner.put(&format!("/api/v1/groups/{org}/policy"), json!({ "max_visibility": "private", "require_two_factor": false, "project_creation_level": "maintainer", "subgroup_creation_level": "owner" })).await;
    assert_eq!(s, 400, "internal children exceed the new maximum");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn tokens_deploy_keys_and_two_factor(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (org, _sub, proj) = org_project(&owner).await;
    let work = srv.work_dir("tokens");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);

    // personal access token: read_repository can clone, not push
    let t = owner.ok_post("/api/v1/user/tokens", json!({ "name": "ci", "scopes": ["read_repository"] })).await;
    let read_token = t["token"].as_str().unwrap().to_string();
    assert!(read_token.starts_with("fgpat-"));
    must(git(&work, &["ls-remote", &srv.http_url_token(&read_token, "acme/platform/api")]).await);
    let p = git(&work, &["push", &srv.http_url_token(&read_token, "acme/platform/api"), "main:x"]).await;
    assert!(!p.ok);
    // and it cannot call the API
    assert_eq!(Client::with_token(&srv.base, &read_token).get("/api/v1/user").await.0, 403);
    // even a full `api` token cannot change the password, the email or 2FA
    let api = owner.ok_post("/api/v1/user/tokens", json!({ "name": "full", "scopes": ["api"] })).await;
    let full = Client::with_token(&srv.base, api["token"].as_str().unwrap());
    assert_eq!(full.put("/api/v1/user/password", json!({ "current": password("olivia"), "new": "taken-over-123" })).await.0, 403);
    assert_eq!(full.put("/api/v1/user", json!({ "name": "x", "email": "evil@example.com" })).await.0, 403);
    assert_eq!(full.post("/api/v1/user/two_factor/setup", json!({})).await.0, 403);
    assert_eq!(full.post("/api/v1/user/tokens", json!({ "name": "more", "scopes": ["api"] })).await.0, 403);
    // nor plant an SSH key, which would outlive the token's revocation
    let planted = test_key().public_key().to_openssh().unwrap();
    assert_eq!(full.post("/api/v1/user/keys", json!({ "title": "planted", "key": planted })).await.0, 403);

    // project access token = bot user with a role on just this project
    let bot = owner
        .ok_post(
            &format!("/api/v1/projects/{proj}/access_tokens"),
            json!({ "name": "renovate", "access_level": "developer", "scopes": ["api", "write_repository"] }),
        )
        .await;
    let bot_token = bot["token"].as_str().unwrap();
    let bot_client = Client::with_token(&srv.base, bot_token);
    assert_eq!(bot_client.ok_get(&format!("/api/v1/projects/{proj}")).await["access_level"], "developer");
    must(git(&work, &["push", "-q", &srv.http_url_token(bot_token, "acme/platform/api"), "main:renovate/deps"]).await);
    let list = owner.ok_get(&format!("/api/v1/projects/{proj}/access_tokens")).await;
    let bot_id = list[0]["bot_user_id"].as_i64().unwrap();
    owner.delete(&format!("/api/v1/projects/{proj}/access_tokens/{bot_id}")).await;
    assert_eq!(bot_client.get(&format!("/api/v1/projects/{proj}")).await.0, 401);

    // deploy token: read-only clone with its own username
    let dt =
        owner.ok_post(&format!("/api/v1/projects/{proj}/deploy_tokens"), json!({ "name": "k8s", "scopes": ["read_repository"] })).await;
    let url = format!("http://{}:{}@{}/acme/platform/api.git", dt["username"].as_str().unwrap(), dt["token"].as_str().unwrap(), srv.host());
    must(git(&work, &["ls-remote", &url]).await);
    assert!(!git(&work, &["push", &url, "main:y"]).await.ok);

    // org requires 2FA: members without it are blocked (API and git)
    let dev = srv.user("dave", false).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    owner
        .put(
            &format!("/api/v1/groups/{org}/policy"),
            json!({ "require_two_factor": true, "project_creation_level": "maintainer", "subgroup_creation_level": "owner" }),
        )
        .await;
    // the policy applies to owners too
    assert_eq!(owner.get(&format!("/api/v1/projects/{proj}")).await.0, 403);
    let (s, v) = dev.get(&format!("/api/v1/projects/{proj}")).await;
    assert_eq!((s, v["error"].as_str()), (403, Some("two_factor_required")));

    // enrol TOTP, after which access works and sign-in needs the code
    let setup = dev.ok_post("/api/v1/user/two_factor/setup", json!({})).await;
    let secret = setup["secret"].as_str().unwrap();
    let code = totp_code(secret);
    assert_eq!(dev.post("/api/v1/user/two_factor/enable", json!({ "code": code })).await.0, 403, "enrolment needs the password");
    dev.ok_post("/api/v1/user/two_factor/enable", json!({ "code": code, "password": password("dave") })).await;
    assert_eq!(dev.get(&format!("/api/v1/projects/{proj}")).await.0, 200);
    let mut fresh = Client::new(&srv.base);
    let r = fresh
        .http
        .post(format!("{}/api/v1/auth/login", srv.base))
        .json(&json!({ "login": "dave", "password": password("dave") }))
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["two_factor_required"], true);
    let totp = |code: String| {
        fresh.http.post(format!("{}/api/v1/auth/login/totp", srv.base)).json(&json!({ "challenge": v["challenge"], "code": code })).send()
    };
    // a code works once: the one used to enrol is spent
    assert_eq!(totp(code.clone()).await.unwrap().status(), 401, "a TOTP code cannot be replayed");
    let resp = totp(totp_code_at(secret, 30)).await.unwrap();
    fresh.take_session(resp).await;
    assert_eq!(fresh.ok_get("/api/v1/user").await["username"], "dave");
    // with 2FA on, plain passwords no longer work for git: tokens are required
    assert!(!git(&work, &["ls-remote", &srv.http_url("dave", "acme/platform/api")]).await.ok);
}

fn totp_code(secret: &str) -> String {
    totp_code_at(secret, 0)
}

/// The code `ahead` seconds from now (the server accepts the next step too).
fn totp_code_at(secret: &str, ahead: u64) -> String {
    let secret = totp_rs::Secret::try_from_base32(secret).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    totp_rs::Builder::new().with_secret(secret).build().unwrap().generate(now + ahead).to_string()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn git_over_ssh_with_user_and_deploy_keys(pool: sqlx::PgPool) {
    if !has("ssh") && !std::path::Path::new("/usr/bin/ssh").exists() {
        eprintln!("skipping: no ssh client");
        return;
    }
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, proj) = org_project(&owner).await;
    let work = srv.work_dir("ssh");

    let key = test_key();
    let key_path = work.join("id_ed25519");
    key.write_openssh_file(&key_path, russh::keys::ssh_key::LineEnding::LF).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let public = key.public_key().to_openssh().unwrap();
    owner.ok_post("/api/v1/user/keys", json!({ "title": "laptop", "key": public })).await;
    // the same key cannot be added twice
    assert_eq!(owner.post("/api/v1/user/keys", json!({ "title": "again", "key": public })).await.0, 409);

    let ssh_cmd = format!(
        "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR",
        key_path.display()
    );
    let url = format!("ssh://git@127.0.0.1:{}/acme/platform/api.git", srv.ssh_port);
    let repo = work.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    seed_repo(&repo).await;
    let env = [("GIT_SSH_COMMAND", ssh_cmd.as_str())];
    must(git_env(&repo, &["push", "-q", &url, "main"], &env).await);
    let clone = work.join("clone");
    must(git_env(&work, &["-c", "protocol.version=2", "clone", "-q", &url, clone.to_str().unwrap()], &env).await);
    assert!(clone.join("src/main.rs").exists());

    // a read-only deploy key can clone but not push
    let dkey = test_key();
    let dpath = work.join("deploy_key");
    dkey.write_openssh_file(&dpath, russh::keys::ssh_key::LineEnding::LF).unwrap();
    std::fs::set_permissions(&dpath, std::fs::Permissions::from_mode(0o600)).unwrap();
    owner
        .ok_post(&format!("/api/v1/projects/{proj}/deploy_keys"), json!({ "title": "ci", "key": dkey.public_key().to_openssh().unwrap() }))
        .await;
    let dcmd = ssh_cmd.replace(key_path.to_str().unwrap(), dpath.to_str().unwrap());
    let denv = [("GIT_SSH_COMMAND", dcmd.as_str())];
    must(git_env(&work, &["ls-remote", &url], &denv).await);
    let push = git_env(&repo, &["push", &url, "main:deploy"], &denv).await;
    assert!(!push.ok && push.stderr.contains("not allowed to push"), "{}", push.stderr);
}

/// M0 spike: jobs are enqueued inside our own transaction (ADR-0004).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn transactional_enqueue(pool: sqlx::PgPool) {
    forgust_server::jobs::migrate(&pool).await.unwrap();
    let q = forgust_server::jobs::GraphileQueue;
    let req = || forgust_server::jobs::JobRequest { task: "ping", payload: json!({ "marker": "x" }), ..Default::default() };
    use forgust_server::jobs::JobQueue;

    let mut tx = pool.begin().await.unwrap();
    q.enqueue(&mut tx, req()).await.unwrap();
    tx.rollback().await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM graphile_worker._private_jobs").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0, "a rolled-back transaction must not leave a job");

    let mut tx = pool.begin().await.unwrap();
    q.enqueue(&mut tx, req()).await.unwrap();
    tx.commit().await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM graphile_worker._private_jobs").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn setup_and_admin(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let token = srv.state.setup_token.lock().unwrap().clone().expect("setup token");
    let mut c = Client::new(&srv.base);
    assert_eq!(c.ok_get("/api/v1/setup").await["needs_setup"], true);
    let bad =
        c.post("/api/v1/setup", json!({ "token": "nope", "username": "root", "email": "r@example.com", "password": "rootpass1" })).await;
    assert_eq!(bad.0, 403);
    let r = c
        .http
        .post(format!("{}/api/v1/setup", srv.base))
        .json(&json!({ "token": token, "username": "root", "email": "r@example.com", "password": "rootpass1" }))
        .send()
        .await
        .unwrap();
    c.take_session(r).await;
    assert_eq!(c.ok_get("/api/v1/user").await["is_admin"], true);
    // only once
    assert_eq!(
        Client::new(&srv.base)
            .post("/api/v1/setup", json!({ "token": token, "username": "x", "email": "x@example.com", "password": "rootpass1" }))
            .await
            .0,
        403
    );

    // CSRF: a session without the header cannot write
    let mut no_csrf = Client::new(&srv.base);
    no_csrf.cookie = c.cookie.clone();
    assert_eq!(no_csrf.post("/api/v1/groups", json!({ "name": "x" })).await.0, 403);

    // admins see everything; every bypass is audited
    let u = c.ok_post("/api/v1/admin/users", json!({ "username": "bob", "email": "bob@example.com", "password": "bobpassword" })).await;
    let mut bob = Client::new(&srv.base);
    bob.login("bob", "bobpassword").await;
    let g = bob.ok_post("/api/v1/groups", json!({ "name": "Bob's", "path": "bobs" })).await;
    c.ok_post(&format!("/api/v1/groups/{}/members", g["id"]), json!({ "username": "root", "access_level": "guest" })).await;
    let audit = c.ok_get("/api/v1/admin/audit").await;
    assert!(audit.as_array().unwrap().iter().any(|e| e["action"] == "admin_bypass"));

    // user search does not reveal emails to non-admins
    let found = bob.ok_get("/api/v1/users?search=bob@example").await;
    assert_eq!(found.as_array().unwrap().len(), 0);
    assert_eq!(bob.ok_get("/api/v1/users?search=bob").await.as_array().unwrap().len(), 1);
    assert_eq!(c.ok_get("/api/v1/users?search=bob@example").await.as_array().unwrap().len(), 1);

    // blocking signs the user out
    c.put(&format!("/api/v1/admin/users/{}", u["id"]), json!({ "state": "blocked" })).await;
    assert_eq!(bob.get("/api/v1/user").await.0, 401);

    // health and metrics are public
    assert_eq!(Client::new(&srv.base).get("/health").await.0, 200);
}

fn test_key() -> russh::keys::PrivateKey {
    let mut seed = [0u8; 32];
    rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut seed).unwrap();
    russh::keys::PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed))
}

/// Moving a subgroup to another org rewrites paths and access for everything inside.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn group_transfer_moves_paths_and_access(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let rita = srv.user("rita", false).await;
    let bob = srv.user("bob", false).await;
    let (org, sub, proj) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "rita", "access_level": "developer" })).await;
    let beta = owner.ok_post("/api/v1/groups", json!({ "name": "Beta", "path": "beta" })).await;
    let beta_id = beta["id"].as_i64().unwrap();
    owner.ok_post(&format!("/api/v1/groups/{beta_id}/members"), json!({ "username": "bob", "access_level": "reporter" })).await;
    assert_eq!(rita.get(&format!("/api/v1/projects/{proj}")).await.0, 200);
    assert_eq!(bob.get(&format!("/api/v1/projects/{proj}")).await.0, 404);

    // a group cannot move inside its own subtree
    let (s, _) = owner.post(&format!("/api/v1/groups/{org}/transfer"), json!({ "parent_id": sub })).await;
    assert_eq!(s, 400);

    let moved = owner.ok_post(&format!("/api/v1/groups/{sub}/transfer"), json!({ "parent_id": beta_id })).await;
    assert_eq!(moved["full_path"], "beta/platform");
    let p = owner.ok_get(&format!("/api/v1/projects/{proj}")).await;
    assert_eq!(p["full_path"], "beta/platform/api");

    // access follows the tree: the old org's member loses it, the new org's gains it
    assert_eq!(rita.get(&format!("/api/v1/projects/{proj}")).await.0, 404);
    assert_eq!(bob.ok_get(&format!("/api/v1/projects/{proj}")).await["access_level"], "reporter");

    // old paths redirect
    let r = owner.ok_get("/api/v1/resolve?path=acme/platform/api").await;
    assert_eq!((r["full_path"].as_str(), r["redirected"].as_bool()), (Some("beta/platform/api"), Some(true)));

    // and back to the top level: it becomes an org of its own
    let top = owner.ok_post(&format!("/api/v1/groups/{sub}/transfer"), json!({ "parent_id": null })).await;
    assert_eq!(top["full_path"], "platform");
    assert_eq!(top["is_org"], true);
    // the user who moved it is now its owner
    let p = owner.ok_get(&format!("/api/v1/projects/{proj}")).await;
    assert_eq!((p["full_path"].as_str(), p["access_level"].as_str()), (Some("platform/api"), Some("owner")));
    assert_eq!(bob.get(&format!("/api/v1/projects/{proj}")).await.0, 404);
}

/// Password guessing is capped per account whatever address the guesser
/// claims, and git over HTTP shares the cap (it had none).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn password_guessing_limits(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (org, _sub, _proj) = org_project(&owner).await;
    for u in ["victor", "gina"] {
        srv.user(u, false).await;
        owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": u, "access_level": "reporter" })).await;
    }
    let http = reqwest::Client::new();
    let refs = |user: &str, pw: &str| {
        http.get(format!("{}/acme/platform/api.git/info/refs?service=git-upload-pack", srv.base)).basic_auth(user, Some(pw)).send()
    };
    assert_eq!(refs("victor", &password("victor")).await.unwrap().status(), 200);

    // web login, a new X-Forwarded-For on every guess
    let mut stopped_at = None;
    for i in 0..30 {
        let r = http
            .post(format!("{}/api/v1/auth/login", srv.base))
            .header("x-forwarded-for", format!("203.0.113.{i}"))
            .json(&json!({ "login": "victor", "password": "wrong" }))
            .send()
            .await
            .unwrap();
        if r.status() == 403 {
            stopped_at = Some(i);
            break;
        }
        assert_eq!(r.status(), 401);
    }
    assert_eq!(stopped_at, Some(20), "20 failures per account, from any address");
    let login = |addr: &str, pw: String| {
        http.post(format!("{}/api/v1/auth/login", srv.base))
            .header("x-forwarded-for", addr.to_string())
            .json(&json!({ "login": "victor", "password": pw }))
            .send()
    };
    assert_eq!(login("198.51.100.7", password("victor")).await.unwrap().status(), 403, "from a new address, the right password waits too");
    // but not the owner, from where they signed in before: guessing cannot lock them out
    assert_eq!(refs("victor", &password("victor")).await.unwrap().status(), 200, "the usual address still gets in, over git");
    let r = http.post(format!("{}/api/v1/auth/login", srv.base)).json(&json!({ "login": "victor", "password": password("victor") }));
    assert_eq!(r.send().await.unwrap().status(), 200, "and on the web");
    // the address or the username: one budget for the account
    assert_eq!(login("198.51.100.8", "wrong".into()).await.unwrap().status(), 403);

    // guessing over git HTTP counts the same way
    let refs_from = |user: &str, pw: &str, addr: &str| {
        http.get(format!("{}/acme/platform/api.git/info/refs?service=git-upload-pack", srv.base))
            .header("x-forwarded-for", addr.to_string())
            .basic_auth(user, Some(pw))
            .send()
    };
    for i in 0..20 {
        assert_eq!(refs_from("gina", "wrong", &format!("203.0.113.{}", 100 + i)).await.unwrap().status(), 401);
    }
    assert_eq!(refs_from("gina", &password("gina"), "198.51.100.9").await.unwrap().status(), 401);
    // others are not affected
    assert_eq!(refs("olivia", &password("olivia")).await.unwrap().status(), 200);
}
