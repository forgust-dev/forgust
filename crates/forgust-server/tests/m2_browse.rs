//! M2: code browsing, LFS, archives; plus backup/restore from M1.

mod common;

use common::*;
use serde_json::json;

async fn pushed_project(srv: &TestServer) -> (Client, i64, std::path::PathBuf) {
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, proj) = org_project(&owner).await;
    let work = srv.work_dir("browse");
    seed_repo(&work).await;
    must(git(&work, &["checkout", "-q", "-b", "feature/x"]).await);
    std::fs::write(work.join("NOTES.md"), "# Notes\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n<script>alert(1)</script>\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "add notes"]).await);
    must(git(&work, &["tag", "-a", "v1.0", "-m", "release 1.0", "main"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main", "feature/x", "v1.0"]).await);
    wait_jobs(&srv.state).await;
    (owner, proj, work)
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn browse_tree_blob_history(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (c, p, _work) = pushed_project(&srv).await;
    let base = format!("/api/v1/projects/{p}/repository");

    // ref + path resolution with slashes in the ref
    let r = c.ok_get(&format!("{base}/resolve?path=feature/x/src/main.rs")).await;
    assert_eq!(r["ref"], "feature/x");
    assert_eq!(r["path"], "src/main.rs");

    // tree: directories first, last commit for the directory
    let t = c.ok_get(&format!("{base}/tree?ref=feature/x")).await;
    let names: Vec<_> = t["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, vec!["src", "NOTES.md", "README.md"]);
    assert_eq!(t["last_commit"]["subject"], "add notes");
    let sub = c.ok_get(&format!("{base}/tree?ref=main&path=src")).await;
    assert_eq!(sub["entries"][0]["path"], "src/main.rs");

    // blob: highlighted, language detected; markdown rendered and sanitized
    let b = c.ok_get(&format!("{base}/blob?ref=main&path=src/main.rs")).await;
    assert_eq!(b["language"], "Rust");
    assert_eq!(b["lines"].as_array().unwrap().len(), 3);
    assert!(b["lines"][1].as_str().unwrap().contains("hl-"));
    let md = c.ok_get(&format!("{base}/blob?ref=feature/x&path=NOTES.md")).await;
    let html = md["markdown_html"].as_str().unwrap();
    assert!(html.contains("<table>") && !html.contains("<script"));
    let readme = c.ok_get(&format!("{base}/readme?ref=main")).await;
    assert!(readme["html"].as_str().unwrap().contains("/acme/platform/api/-/blob/main/src/main.rs"));

    // raw is never served as HTML
    let (s, body) = c.get_bytes(&format!("{base}/raw?ref=feature/x&path=NOTES.md")).await;
    assert_eq!(s, 200);
    assert!(String::from_utf8_lossy(&body).contains("<script>"));
    let r = c
        .http
        .get(format!("{}/acme/platform/api/-/raw/feature/x/NOTES.md", srv.base))
        .header("cookie", c.cookie.clone().unwrap())
        .send()
        .await
        .unwrap();
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/plain"));

    // history, single commit, diff
    let log = c.ok_get(&format!("{base}/commits?ref=main")).await;
    assert_eq!(log.as_array().unwrap().len(), 2);
    assert_eq!(log[0]["subject"], "greet the world");
    let sha = log[0]["id"].as_str().unwrap();
    let commit = c.ok_get(&format!("{base}/commits/{}", &sha[..8])).await;
    assert_eq!(commit["id"], sha);
    let diff = c.ok_get(&format!("{base}/commits/{sha}/diff")).await;
    assert_eq!(diff["total_files"], 1);
    assert_eq!(diff["files"][0]["additions"], 1);
    assert_eq!(diff["files"][0]["deletions"], 1);
    let root = c.ok_get(&format!("{base}/commits/{}/diff", log[1]["id"].as_str().unwrap())).await;
    assert_eq!(root["files"][0]["status"], "added");
    let path_log = c.ok_get(&format!("{base}/commits?ref=feature/x&path=NOTES.md")).await;
    assert_eq!(path_log.as_array().unwrap().len(), 1);

    // compare from the merge base
    let cmp = c.ok_get(&format!("{base}/compare?from=main&to=feature/x")).await;
    assert_eq!(cmp["commits"].as_array().unwrap().len(), 1);
    assert_eq!(cmp["diff"]["files"][0]["new_path"], "NOTES.md");

    // branches and tags
    let branches = c.ok_get(&format!("{base}/branches")).await;
    let main = branches.as_array().unwrap().iter().find(|b| b["name"] == "main").unwrap();
    assert_eq!(main["default"], true);
    assert_eq!(main["protected"], true);
    let tags = c.ok_get(&format!("{base}/tags")).await;
    assert_eq!(tags[0]["name"], "v1.0");
    assert_eq!(tags[0]["annotated"], true);

    // blame
    let blame = c.ok_get(&format!("{base}/blame?ref=main&path=src/main.rs")).await;
    assert_eq!(blame["lines"].as_array().unwrap().len(), 3);
    let hunks = blame["hunks"].as_array().unwrap();
    assert!(hunks.iter().any(|h| h["commit"]["subject"] == "greet the world"));
    assert!(hunks.iter().any(|h| h["commit"]["subject"] == "initial commit"));

    // archives
    let (s, tgz) = c.get_bytes(&format!("{base}/archive?ref=main&format=tar.gz")).await;
    assert_eq!(s, 200);
    assert_eq!(&tgz[..2], &[0x1f, 0x8b]);
    let (s, zip) = c.get_bytes(&format!("{base}/archive?ref=v1.0&format=zip")).await;
    assert_eq!(s, 200);
    assert_eq!(&zip[..2], b"PK");

    // results are cached by object id: a second read is a cache hit
    srv.state.cache.sync().await;
    let before = srv.state.cache.entry_count();
    assert!(before > 0);
    c.ok_get(&format!("{base}/blob?ref=main&path=src/main.rs")).await;
    srv.state.cache.sync().await;
    assert_eq!(srv.state.cache.entry_count(), before);

    // unknown refs and paths are 404; traversal is refused
    assert_eq!(c.get(&format!("{base}/tree?ref=nope")).await.0, 404);
    assert_eq!(c.get(&format!("{base}/blob?ref=main&path=missing.txt")).await.0, 404);
    assert_eq!(c.get(&format!("{base}/blob?ref=main&path=../etc/passwd")).await.0, 400);
    assert_eq!(c.get(&format!("{base}/tree?ref=--upload-pack=x")).await.0, 400);

    // other users cannot browse a private project
    let other = srv.user("otto", false).await;
    assert_eq!(other.get(&format!("{base}/tree?ref=main")).await.0, 404);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn reftable_project_browses_through_git_processes(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let p = owner.ok_post("/api/v1/projects", json!({ "name": "refs", "ref_format": "reftable" })).await;
    assert_eq!(p["ref_format"], "reftable");
    let work = srv.work_dir("reftable");
    seed_repo(&work).await;
    let path = p["full_path"].as_str().unwrap();
    must(git(&work, &["push", "-q", &srv.http_url("olivia", path), "main"]).await);
    let t = owner.ok_get(&format!("/api/v1/projects/{}/repository/tree?ref=main", p["id"])).await;
    assert_eq!(t["entries"].as_array().unwrap().len(), 2);
    let b = owner.ok_get(&format!("/api/v1/projects/{}/repository/blob?ref=main&path=src/main.rs", p["id"])).await;
    assert_eq!(b["lines"].as_array().unwrap().len(), 3);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn lfs_push_and_download(pool: sqlx::PgPool) {
    if !has("git-lfs") {
        eprintln!("skipping: git-lfs not installed");
        return;
    }
    let srv = TestServer::start(pool).await;
    let (c, p, work) = pushed_project(&srv).await;
    must(git(&work, &["checkout", "-q", "main"]).await);
    must(git(&work, &["lfs", "install", "--local"]).await);
    must(git(&work, &["lfs", "track", "*.bin"]).await);
    let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(work.join("model.bin"), &data).unwrap();
    must(git(&work, &["add", ".gitattributes", "model.bin"]).await);
    must(git(&work, &["commit", "-q", "-m", "add model"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main:lfs"]).await);

    // the blob view shows the pointer's object; raw serves the real content
    let b = c.ok_get(&format!("/api/v1/projects/{p}/repository/blob?ref=lfs&path=model.bin")).await;
    assert_eq!(b["lfs"]["size"], 200_000);
    assert_eq!(b["lfs"]["stored"], true);
    let (s, raw) = c.get_bytes(&format!("/api/v1/projects/{p}/repository/raw?ref=lfs&path=model.bin")).await;
    assert_eq!(s, 200);
    assert_eq!(raw, data);

    // a fresh clone gets the object through the batch API
    let clone = srv.work_dir("lfs-clone");
    must(git(&clone, &["lfs", "install"]).await);
    must(git(&clone, &["clone", "-q", "-b", "lfs", &srv.http_url("olivia", "acme/platform/api"), "c"]).await);
    assert_eq!(std::fs::read(clone.join("c/model.bin")).unwrap(), data);

    // a wrong hash is refused
    let r = c
        .http
        .put(format!("{}/acme/platform/api.git/info/lfs/objects/{}", srv.base, "a".repeat(64)))
        .basic_auth("olivia", Some(password("olivia")))
        .body("not matching")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 422);
}

/// Backup then restore into an empty database and repos dir: identical refs.
/// Needs `pg_dump`/`pg_restore` (set FORGUST_TEST_PG_DUMP / FORGUST_TEST_PG_RESTORE,
/// e.g. `docker exec -i forgust-pg pg_dump`) and the URL they reach the database at
/// (FORGUST_TEST_PG_URL, e.g. postgres://forgust:forgust@localhost:5432).
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn backup_and_restore(pool: sqlx::PgPool) {
    let (Ok(dump), Ok(restore), Ok(url_base)) =
        (std::env::var("FORGUST_TEST_PG_DUMP"), std::env::var("FORGUST_TEST_PG_RESTORE"), std::env::var("FORGUST_TEST_PG_URL"))
    else {
        eprintln!("skipping: FORGUST_TEST_PG_DUMP / FORGUST_TEST_PG_RESTORE / FORGUST_TEST_PG_URL not set");
        return;
    };
    let db_name: String = sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
    let srv = TestServer::start(pool.clone()).await;
    let (_c, _p, work) = pushed_project(&srv).await;
    let empty = srv.user("eve", false).await;
    empty.ok_post("/api/v1/projects", json!({ "name": "empty" })).await;
    let before = must(git(&work, &["ls-remote", &srv.http_url("olivia", "acme/platform/api")]).await);

    let mut cfg = srv.state.cfg.clone();
    cfg.backup.pg_dump = dump;
    cfg.backup.pg_restore = restore;
    let out = srv.tmp.path().join("backups");
    // SAFETY: tests in this binary that read this variable run sequentially here.
    unsafe { std::env::set_var("FORGUST_BACKUP_DATABASE_URL", format!("{url_base}/{db_name}")) };
    let dir = forgust_server::backup::backup(&cfg, srv.state.storage.as_ref(), &pool, &out).await.unwrap();

    // restore into a brand-new database and repos dir
    let fresh = format!("forgust_restore_{}", std::process::id());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{fresh}\""))).execute(&pool).await.unwrap();
    unsafe { std::env::set_var("FORGUST_BACKUP_DATABASE_URL", format!("{url_base}/{fresh}")) };
    cfg.storage.repos_dir = Some(srv.tmp.path().join("restored-repos"));
    let storage = forgust_server::app::storage_for(&cfg);
    let n = forgust_server::backup::restore(&cfg, storage.as_ref(), &dir).await.unwrap();
    assert_eq!(n, 2);

    // start a second server on the restored data and compare refs
    let local = std::env::var("DATABASE_URL").unwrap();
    let restored_url = format!("{}/{fresh}", local.rsplit_once('/').unwrap().0);
    let pool2 = sqlx::PgPool::connect(&restored_url).await.unwrap();
    let repos = srv.tmp.path().join("restored-repos");
    let srv2 = TestServer::start_with(pool2.clone(), |c| c.storage.repos_dir = Some(repos)).await;
    let after = must(git(&work, &["ls-remote", &srv2.http_url("olivia", "acme/platform/api")]).await);
    assert_eq!(before, after);
    pool2.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE \"{fresh}\" WITH (FORCE)"))).execute(&pool).await.unwrap();
}

/// M3.5: file list for the finder, Markdown preview, highlighted diff lines.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn file_finder_markdown_preview_and_highlighted_diffs(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (c, p, _work) = pushed_project(&srv).await;
    let base = format!("/api/v1/projects/{p}/repository");

    let f = c.ok_get(&format!("{base}/files?ref=feature/x")).await;
    let paths: Vec<_> = f["paths"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(paths, vec!["NOTES.md", "README.md", "src/main.rs"]);
    assert_eq!(f["truncated"], false);
    assert_eq!(c.get(&format!("{base}/files?ref=nope")).await.0, 404);

    // preview: same renderer and sanitizer as saved notes, links resolved in the project
    let md = c.ok_post(&format!("/api/v1/projects/{p}/markdown"), json!({ "text": "**hi** [code](src/main.rs) <script>x</script>" })).await;
    let html = md["html"].as_str().unwrap();
    assert!(html.contains("<strong>hi</strong>") && !html.contains("<script"), "{html}");

    // commit diff: every line carries highlighted HTML from its side's blob
    let head = c.ok_get(&format!("{base}/resolve?path=main")).await;
    let d = c.ok_get(&format!("{base}/commits/{}/diff", head["commit"].as_str().unwrap())).await;
    let file = &d["files"][0];
    assert_eq!(file["new_path"], "src/main.rs");
    assert!(file["old_oid"].is_string() && file["new_oid"].is_string());
    let lines = file["hunks"][0]["lines"].as_array().unwrap();
    let del = lines.iter().find(|l| l["kind"] == "del").unwrap();
    let add = lines.iter().find(|l| l["kind"] == "add").unwrap();
    assert!(del["html"].as_str().unwrap().contains("hl-") && del["html"].as_str().unwrap().contains("hello"));
    assert!(add["html"].as_str().unwrap().contains("hello, world"));

    // neither endpoint leaks to outsiders
    let other = srv.user("mallory", false).await;
    assert_eq!(other.get(&format!("{base}/files?ref=main")).await.0, 404);
    assert_eq!(other.post(&format!("/api/v1/projects/{p}/markdown"), json!({ "text": "x" })).await.0, 404);
}

/// The last commit of each tree entry, and size / language stats.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn tree_last_commits_and_repo_stats(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (c, p, _work) = pushed_project(&srv).await;
    let base = format!("/api/v1/projects/{p}/repository");
    // the commits' author has an account (ada@example.com)
    srv.user("ada", false).await;

    let lc = c.ok_get(&format!("{base}/last_commits?ref=feature/x")).await;
    assert_eq!(lc["complete"], true);
    assert_eq!(lc["entries"][0]["commit"]["author"]["username"], "ada", "{lc}");
    let log = c.ok_get(&format!("{base}/commits?ref=main")).await;
    assert_eq!(log[0]["committer"]["username"], "ada");
    assert_eq!(c.ok_get(&format!("{base}/tree?ref=main")).await["last_commit"]["author"]["username"], "ada");
    let by: std::collections::HashMap<String, String> = lc["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["path"].as_str().unwrap().to_string(), e["commit"]["subject"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(by["src"], "greet the world");
    assert_eq!(by["NOTES.md"], "add notes");
    assert_eq!(by["README.md"], "initial commit");
    // a subdirectory, and the cached answer is the same
    let sub = c.ok_get(&format!("{base}/last_commits?ref=main&path=src")).await;
    assert_eq!(sub["entries"][0]["path"], "src/main.rs");
    assert_eq!(sub["entries"][0]["commit"]["subject"], "greet the world");
    assert_eq!(c.ok_get(&format!("{base}/last_commits?ref=main&path=src")).await, sub);

    let st = c.ok_get(&format!("{base}/stats?ref=main")).await;
    assert!(st["repository_bytes"].as_u64().unwrap() > 0, "{st}");
    assert_eq!(st["lfs_objects"], 0);
    let main_rs = "fn main() {\n    println!(\"hello, world\");\n}\n".len() as u64;
    assert_eq!(st["languages"], json!([{ "name": "Rust", "color": "#dea584", "bytes": main_rs }]));
    assert_eq!(st["languages_truncated"], false);
    assert_eq!(st["commit_count"], 2);
    assert_eq!(c.ok_get(&format!("{base}/stats?ref=feature/x")).await["commit_count"], 3);
}
