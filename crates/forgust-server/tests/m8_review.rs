//! Push rules, signed commits, Git LFS locks and code owners.

mod common;

use std::path::Path;

use common::*;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

/// Commit as olivia (whose confirmed email is olivia@example.com).
const OLIVIA: &[(&str, &str)] = &[
    ("GIT_AUTHOR_NAME", "Olivia"),
    ("GIT_AUTHOR_EMAIL", "olivia@example.com"),
    ("GIT_COMMITTER_NAME", "Olivia"),
    ("GIT_COMMITTER_EMAIL", "olivia@example.com"),
];

async fn setup(srv: &TestServer) -> (Client, i64, std::path::PathBuf) {
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let work = srv.work_dir("review");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    (owner, project, work)
}

async fn push(srv: &TestServer, work: &Path, user: &str, refspec: &str) -> GitOut {
    let out = git(work, &["push", "-q", &srv.http_url(user, REPO), refspec]).await;
    wait_jobs(&srv.state).await;
    out
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn push_rules_refuse_what_they_should(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, project, work) = setup(&srv).await;
    let rule = format!("/api/v1/projects/{project}/push_rule");
    assert_eq!(owner.put(&rule, json!({ "commit_message_regex": "(" })).await.0, 400, "a bad regex is refused when saved");
    let (s, body) = owner
        .put(
            &rule,
            json!({ "commit_message_regex": "^(feat|fix): ", "author_email_regex": "@example\\.com$", "branch_name_regex": "^(main|feature/.+)$",
                    "max_file_size_mb": 1, "prevent_secrets": true }),
        )
        .await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(owner.ok_get(&rule).await["max_file_size_mb"], 1);

    // a message that does not follow the pattern
    commit_file(&work, "a.txt", "a\n", "add a").await;
    let out = push(&srv, &work, "olivia", "main").await;
    assert!(!out.ok && out.stderr.contains("does not follow the pattern '^(feat|fix): '"), "{}", out.stderr);
    must(git(&work, &["commit", "-q", "--amend", "-m", "feat: add a"]).await);
    must(push(&srv, &work, "olivia", "main").await);

    // an author outside the domain
    std::fs::write(work.join("b.txt"), "b\n").unwrap();
    must(git(&work, &["add", "b.txt"]).await);
    must(git_env(&work, &["commit", "-q", "-m", "fix: b"], &[("GIT_AUTHOR_EMAIL", "mallory@evil.test")]).await);
    let out = push(&srv, &work, "olivia", "main").await;
    assert!(!out.ok && out.stderr.contains("mallory@evil.test"), "{}", out.stderr);
    must(git(&work, &["reset", "-q", "--hard", "HEAD~1"]).await);

    // a branch name outside the pattern
    let out = push(&srv, &work, "olivia", "main:wip").await;
    assert!(!out.ok && out.stderr.contains("Branch name wip"), "{}", out.stderr);
    must(push(&srv, &work, "olivia", "main:feature/x").await);

    // a large file, and a private key
    std::fs::write(work.join("big.bin"), vec![b'x'; 1_500_000]).unwrap();
    must(git(&work, &["add", "big.bin"]).await);
    must(git(&work, &["commit", "-q", "-m", "feat: big"]).await);
    let out = push(&srv, &work, "olivia", "main").await;
    assert!(!out.ok && out.stderr.contains("File big.bin is 1.4 MB"), "{}", out.stderr);
    must(git(&work, &["reset", "-q", "--hard", "HEAD~1"]).await);
    commit_file(&work, "deploy/id_rsa", "-----BEGIN OPENSSH PRIVATE KEY-----\n", "feat: key").await;
    let out = push(&srv, &work, "olivia", "main").await;
    assert!(!out.ok && out.stderr.contains("deploy/id_rsa looks like a secret"), "{}", out.stderr);
    must(git(&work, &["reset", "-q", "--hard", "HEAD~1"]).await);

    // commits made in the browser follow the same rules
    let commits = format!("/api/v1/projects/{project}/repository/commits");
    let action = json!([{ "action": "create", "file_path": "web.txt", "content": "w\n" }]);
    let (s, body) = owner.post(&commits, json!({ "branch": "main", "commit_message": "edit", "actions": action })).await;
    assert_eq!(s, 403, "{body}");
    assert!(body["message"].as_str().unwrap().contains("does not follow the pattern"), "{body}");
    owner.ok_post(&commits, json!({ "branch": "main", "commit_message": "feat: edit", "actions": action })).await;
}

/// An SSH signing key made with ssh-keygen: (private key path, public key line).
fn ssh_signing_key(dir: &Path) -> (String, String) {
    let key = dir.join("signing");
    let out =
        std::process::Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-C", "olivia", "-f"]).arg(&key).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    (key.display().to_string(), std::fs::read_to_string(key.with_extension("pub")).unwrap().trim().to_string())
}

async fn signature(c: &Client, project: i64, sha: &str) -> (u16, Value) {
    c.get(&format!("/api/v1/projects/{project}/repository/commits/{sha}/signature")).await
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn ssh_signed_commits_are_verified_and_can_be_required(pool: sqlx::PgPool) {
    if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
        eprintln!("skipped: no ssh-keygen");
        return;
    }
    let srv = TestServer::start(pool).await;
    let (owner, project, work) = setup(&srv).await;
    let (private, public) = ssh_signing_key(&srv.work_dir("keys"));
    let signed = |msg: &'static str| {
        let (work, private) = (work.clone(), private.clone());
        async move {
            let mut env = OLIVIA.to_vec();
            env.extend([
                ("GIT_CONFIG_COUNT", "2"),
                ("GIT_CONFIG_KEY_0", "gpg.format"),
                ("GIT_CONFIG_VALUE_0", "ssh"),
                ("GIT_CONFIG_KEY_1", "user.signingkey"),
            ]);
            env.push(("GIT_CONFIG_VALUE_1", Box::leak(private.into_boxed_str())));
            must(git_env(&work, &["commit", "-q", "--allow-empty", "-S", "-m", msg], &env).await);
            must(git(&work, &["rev-parse", "HEAD"]).await)
        }
    };
    // unsigned commits have no signature
    let head = must(git(&work, &["rev-parse", "HEAD"]).await);
    assert_eq!(signature(&owner, project, &head).await.0, 404);
    // signed with a key nobody registered
    let sha = signed("feat: signed").await;
    must(push(&srv, &work, "olivia", "main").await);
    let (s, v) = signature(&owner, project, &sha).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!((v["signature_type"].as_str(), v["verification_status"].as_str()), (Some("SSH"), Some("unknown_key")), "{v}");
    // added for authentication only, the key does not verify commits
    let auth_only = owner.ok_post("/api/v1/user/keys", json!({ "title": "laptop", "key": public, "usage_type": "auth" })).await;
    assert_eq!(signature(&owner, project, &sha).await.1["verification_status"], "unknown_key");
    assert_eq!(owner.delete(&format!("/api/v1/user/keys/{}", auth_only["id"])).await.0, 200);
    // once olivia adds it as a signing key, the same commit is verified
    owner.ok_post("/api/v1/user/keys", json!({ "title": "laptop", "key": public, "usage_type": "signing" })).await;
    let (_, v) = signature(&owner, project, &sha).await;
    assert_eq!((v["verification_status"].as_str(), v["signer"].as_str()), (Some("verified"), Some("olivia")), "{v}");
    let batch = owner.ok_post(&format!("/api/v1/projects/{project}/repository/signatures"), json!({ "shas": [sha, head] })).await;
    assert_eq!(batch.as_object().unwrap().len(), 1, "unsigned commits are left out: {batch}");
    assert_eq!(batch[&sha]["verification_status"], "verified");
    // once the key expires, what it signed stays verified
    let expires = |sql: &'static str| sqlx::query(sql).execute(&srv.state.db);
    expires("UPDATE ssh_keys SET expires_at = now() - interval '1 day' WHERE title = 'laptop'").await.unwrap();
    assert_eq!(signature(&owner, project, &sha).await.1["verification_status"], "verified");
    expires("UPDATE ssh_keys SET expires_at = NULL WHERE title = 'laptop'").await.unwrap();

    // required: unsigned commits are refused, verified ones pass
    assert_eq!(owner.put(&format!("/api/v1/projects/{project}/push_rule"), json!({ "reject_unsigned_commits": true })).await.0, 200);
    must(git(&work, &["commit", "-q", "--allow-empty", "-m", "unsigned"]).await);
    let out = push(&srv, &work, "olivia", "main").await;
    assert!(!out.ok && out.stderr.contains("is not signed"), "{}", out.stderr);
    must(git(&work, &["reset", "-q", "--hard", "HEAD~1"]).await);
    // signed, but committed as someone else: not verified
    let mut env = OLIVIA.to_vec();
    env[3] = ("GIT_COMMITTER_EMAIL", "someone@example.com");
    env.extend([
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "gpg.format"),
        ("GIT_CONFIG_VALUE_0", "ssh"),
        ("GIT_CONFIG_KEY_1", "user.signingkey"),
    ]);
    env.push(("GIT_CONFIG_VALUE_1", Box::leak(private.clone().into_boxed_str())));
    must(git_env(&work, &["commit", "-q", "--allow-empty", "-S", "-m", "other email"], &env).await);
    let out = push(&srv, &work, "olivia", "main").await;
    assert!(!out.ok && out.stderr.contains("not verified (unverified)"), "{}", out.stderr);
    assert!(out.stderr.contains("commit with an email address confirmed on your profile"), "{}", out.stderr);
    must(git(&work, &["reset", "-q", "--hard", "HEAD~1"]).await);
    // signed with a key not added for signing: the refusal says how to add it
    let (other, _) = ssh_signing_key(&srv.work_dir("other-keys"));
    let mut env = OLIVIA.to_vec();
    env.extend([
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "gpg.format"),
        ("GIT_CONFIG_VALUE_0", "ssh"),
        ("GIT_CONFIG_KEY_1", "user.signingkey"),
    ]);
    env.push(("GIT_CONFIG_VALUE_1", Box::leak(other.into_boxed_str())));
    must(git_env(&work, &["commit", "-q", "--allow-empty", "-S", "-m", "unknown key"], &env).await);
    let out = push(&srv, &work, "olivia", "main").await;
    assert!(!out.ok && out.stderr.contains("not verified (unknown key)"), "{}", out.stderr);
    assert!(out.stderr.contains("an SSH key for signing (or for authentication and signing)"), "{}", out.stderr);
    must(git(&work, &["reset", "-q", "--hard", "HEAD~1"]).await);
    signed("feat: signed again").await;
    must(push(&srv, &work, "olivia", "main").await);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn gpg_signed_commits_are_verified(pool: sqlx::PgPool) {
    if std::process::Command::new("gpg").arg("--version").output().is_err() {
        eprintln!("skipped: no gpg");
        return;
    }
    let srv = TestServer::start(pool).await;
    let (owner, project, work) = setup(&srv).await;
    let home = srv.work_dir("gnupg");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gpg = |args: &[&str]| {
        let out = std::process::Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "gpg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    gpg(&["--quick-gen-key", "Olivia <olivia@example.com>", "ed25519", "sign", "never"]);
    let armored = gpg(&["--armor", "--export", "olivia@example.com"]);
    assert_eq!(owner.post("/api/v1/user/gpg_keys", json!({ "key": "not a key" })).await.0, 400);
    let key = owner.ok_post("/api/v1/user/gpg_keys", json!({ "key": armored })).await;
    assert_eq!(key["emails"], json!(["olivia@example.com"]));
    assert_eq!(owner.post("/api/v1/user/gpg_keys", json!({ "key": armored })).await.0, 409);

    let home_s = home.display().to_string();
    let mut env = OLIVIA.to_vec();
    env.extend([
        ("GNUPGHOME", home_s.as_str()),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "user.signingkey"),
        ("GIT_CONFIG_VALUE_0", "olivia@example.com"),
    ]);
    must(git_env(&work, &["commit", "-q", "--allow-empty", "-S", "-m", "gpg signed"], &env).await);
    let sha = must(git(&work, &["rev-parse", "HEAD"]).await);
    must(push(&srv, &work, "olivia", "main").await);
    let (s, v) = signature(&owner, project, &sha).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!((v["signature_type"].as_str(), v["verification_status"].as_str()), (Some("PGP"), Some("verified")), "{v}");
    assert_eq!(v["key_fingerprint"], key["fingerprint"]);
    // the key removed: the signature no longer ties to anyone
    assert_eq!(owner.delete(&format!("/api/v1/user/gpg_keys/{}", key["id"])).await.0, 200);
    assert_eq!(signature(&owner, project, &sha).await.1["verification_status"], "unknown_key");
}

/// The git-lfs locking API as the client calls it (basic auth).
async fn lfs(srv: &TestServer, user: &str, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
    let r = reqwest::Client::new()
        .request(method, format!("{}/{REPO}.git/info/lfs/{path}", srv.base))
        .basic_auth(user, Some(password(user)))
        .header("accept", "application/vnd.git-lfs+json")
        .json(&body)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn lfs_locks_keep_others_off_locked_files(pool: sqlx::PgPool) {
    use reqwest::Method as M;
    let srv = TestServer::start(pool).await;
    let (owner, project, work) = setup(&srv).await;
    for (u, role) in [("dave", "maintainer"), ("ed", "developer")] {
        srv.user(u, false).await;
        owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": u, "access_level": role })).await;
    }
    // Developers may push to main here: only the lock stands in ed's way
    owner
        .ok_post(&format!("/api/v1/projects/{project}/protected_branches"), json!({ "name": "main", "push_access_level": "developer" }))
        .await;

    let (s, lock) = lfs(&srv, "olivia", M::POST, "locks", json!({ "path": "src/main.rs", "ref": { "name": "refs/heads/main" } })).await;
    assert_eq!(s, 201, "{lock}");
    assert_eq!(lock["lock"]["path"], "src/main.rs");
    let (s, again) = lfs(&srv, "ed", M::POST, "locks", json!({ "path": "src/main.rs" })).await;
    assert_eq!(s, 409, "{again}");
    let (_, list) = lfs(&srv, "ed", M::GET, "locks", Value::Null).await;
    assert_eq!(list["locks"].as_array().unwrap().len(), 1);
    let (_, mine) = lfs(&srv, "olivia", M::POST, "locks/verify", json!({})).await;
    assert_eq!((mine["ours"].as_array().unwrap().len(), mine["theirs"].as_array().unwrap().len()), (1, 0));
    let (_, theirs) = lfs(&srv, "ed", M::POST, "locks/verify", json!({})).await;
    assert_eq!((theirs["ours"].as_array().unwrap().len(), theirs["theirs"].as_array().unwrap().len()), (0, 1));

    // ed cannot change the locked file on main; other files are fine
    commit_file(&work, "src/main.rs", "fn main() {}\n", "edit locked").await;
    let out = push(&srv, &work, "ed", "main").await;
    assert!(!out.ok && out.stderr.contains("src/main.rs (locked by @olivia)"), "{}", out.stderr);
    // ... on a branch it can
    must(push(&srv, &work, "ed", "main:ed-branch").await);
    // the lock owner can
    must(push(&srv, &work, "olivia", "main").await);

    // unlocking: not ed's; a Maintainer with force
    let id = lock["lock"]["id"].as_str().unwrap().to_string();
    let (s, _) = lfs(&srv, "ed", M::POST, &format!("locks/{id}/unlock"), json!({ "force": true })).await;
    assert_eq!(s, 403);
    let (s, _) = lfs(&srv, "dave", M::POST, &format!("locks/{id}/unlock"), json!({})).await;
    assert_eq!(s, 403, "someone else's lock needs force");
    let (s, body) = lfs(&srv, "dave", M::POST, &format!("locks/{id}/unlock"), json!({ "force": true })).await;
    assert_eq!(s, 200, "{body}");
    assert!(owner.ok_get(&format!("/api/v1/projects/{project}/lfs_locks")).await.as_array().unwrap().is_empty());
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn code_owners_must_approve_when_the_branch_says_so(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, project, work) = setup(&srv).await;
    for (u, role) in [("alice", "developer"), ("dave", "developer"), ("guest", "reporter")] {
        srv.user(u, false).await;
        owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": u, "access_level": role })).await;
    }
    let mut alice = Client::new(&srv.base);
    alice.login("alice", &password("alice")).await;
    commit_file(&work, ".gitlab/CODEOWNERS", "* @olivia\n/src/ @alice @guest\n\n^[Docs] @dave\n*.md\n", "owners").await;
    must(push(&srv, &work, "olivia", "main").await);
    owner
        .ok_post(
            &format!("/api/v1/projects/{project}/protected_branches"),
            json!({ "name": "main", "push_access_level": "maintainer", "merge_access_level": "developer", "code_owner_approval_required": true }),
        )
        .await;
    must(git(&work, &["checkout", "-q", "-b", "change"]).await);
    commit_file(&work, "src/main.rs", "fn main() { println!(\"owned\"); }\n", "src change").await;
    commit_file(&work, "README.md", "# Docs change\n", "docs change").await;
    must(push(&srv, &work, "olivia", "change").await);
    let mr =
        owner.ok_post(&format!("/api/v1/projects/{project}/merge_requests"), json!({ "source_branch": "change", "title": "Change" })).await;
    let path = format!("/api/v1/projects/{project}/merge_requests/{}", mr["iid"]);

    let co = owner.ok_get(&format!("{path}/code_owners")).await;
    assert_eq!(co["file"], ".gitlab/CODEOWNERS");
    assert_eq!(co["approval_required"], true);
    assert_eq!(co["file_owners"]["src/main.rs"], json!(["alice"]), "the guest is not Developer: {co}");
    assert_eq!(co["file_owners"]["README.md"], json!(["dave", "olivia"]));
    let default = co["sections"].as_array().unwrap().iter().find(|s| s["name"] == "").unwrap();
    assert_eq!(default["files"], json!(["README.md", "src/main.rs"]));
    assert_eq!(default["owners"], json!(["alice", "olivia"]));
    let docs = co["sections"].as_array().unwrap().iter().find(|s| s["name"] == "Docs").unwrap();
    assert_eq!(docs["optional"], true);
    let suggested: Vec<&str> = co["suggested_reviewers"].as_array().unwrap().iter().map(|u| u["username"].as_str().unwrap()).collect();
    assert_eq!(suggested, ["alice", "dave"], "the author is not suggested");

    // the default section needs one of its owners: olivia is the author, so alice
    let status = owner.ok_get(&format!("{path}/merge_status")).await;
    assert!(status["blockers"].as_array().unwrap().iter().any(|b| b.as_str().unwrap().starts_with("Code owners must approve")), "{status}");
    alice.ok_post(&format!("{path}/approve"), json!({})).await;
    let status = owner.ok_get(&format!("{path}/merge_status")).await;
    assert!(!status["blockers"].as_array().unwrap().iter().any(|b| b.as_str().unwrap().contains("Code owners")), "{status}");
    let rule = status["approval_rules"].as_array().unwrap().iter().find(|r| r["name"] == "Code owners").unwrap().clone();
    assert_eq!((rule["given"].as_i64(), rule["satisfied"].as_bool()), (Some(1), Some(true)));
}
