//! Code search: the index follows each project's default branch, and a
//! search finds text only in code the caller may read.

mod common;
use common::*;
use forgust_server::code_search::index::Filter;
use forgust_server::code_search::query::plan;
use serde_json::json;

/// Paths the index holds for `text`, as `project:path`.
fn indexed(srv: &TestServer, text: &str) -> Vec<String> {
    let index = srv.state.code_index.as_ref().expect("code search is on");
    let filter = Filter { need: plan(text, false, false).unwrap().0, projects: None, path: None, filename: None, lang: None };
    index.candidates(&filter).unwrap().0.into_iter().map(|c| format!("{}:{}", c.project_id, c.path)).collect()
}

async fn push(srv: &TestServer, work: &std::path::Path, user: &str, path: &str, refs: &[&str]) {
    let url = srv.http_url(user, path);
    let mut args = vec!["push", "-q", url.as_str()];
    args.extend(refs);
    must(git(work, &args).await);
    wait_jobs(&srv.state).await;
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn the_index_follows_the_default_branch(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    let work = srv.work_dir("index");
    seed_repo(&work).await;
    push(&srv, &work, "olivia", "acme/platform/api", &["main"]).await;
    assert_eq!(indexed(&srv, "hello, world"), vec![format!("{p}:src/main.rs")]);
    assert_eq!(indexed(&srv, "the code"), vec![format!("{p}:README.md")]);

    // a changed file, a new one, a moved one and a deleted one, in one push
    commit_file(&work, "src/main.rs", "fn main() {\n    println!(\"goodbye\");\n}\n", "say goodbye").await;
    commit_file(&work, "docs/guide.txt", "How to say hello, world\n", "guide").await;
    must(git(&work, &["mv", "README.md", "ABOUT.md"]).await);
    must(git(&work, &["commit", "-q", "-m", "rename"]).await);
    push(&srv, &work, "olivia", "acme/platform/api", &["main"]).await;
    assert_eq!(indexed(&srv, "hello, world"), vec![format!("{p}:docs/guide.txt")]);
    assert_eq!(indexed(&srv, "goodbye"), vec![format!("{p}:src/main.rs")]);
    assert_eq!(indexed(&srv, "the code"), vec![format!("{p}:ABOUT.md")]);
    must(git(&work, &["rm", "-q", "docs/guide.txt"]).await);
    must(git(&work, &["commit", "-q", "-m", "no guide"]).await);
    push(&srv, &work, "olivia", "acme/platform/api", &["main"]).await;
    assert!(indexed(&srv, "hello, world").is_empty());

    // other branches are not searched
    must(git(&work, &["checkout", "-q", "-b", "feature"]).await);
    commit_file(&work, "feature.txt", "only on the feature branch\n", "feature").await;
    push(&srv, &work, "olivia", "acme/platform/api", &["feature"]).await;
    assert!(indexed(&srv, "feature branch").is_empty());
    // until one becomes the default branch
    owner
        .ok_put(
            &format!("/api/v1/projects/{p}"),
            json!({ "name": "API", "description": "", "visibility": "private", "default_branch": "feature" }),
        )
        .await;
    wait_jobs(&srv.state).await;
    assert_eq!(indexed(&srv, "feature branch"), vec![format!("{p}:feature.txt")]);

    // a history rewritten under the index is indexed from the new head
    must(git(&work, &["checkout", "-q", "--orphan", "fresh"]).await);
    must(git(&work, &["rm", "-rqf", "."]).await);
    commit_file(&work, "new.txt", "a fresh start\n", "fresh").await;
    push(&srv, &work, "olivia", "acme/platform/api", &["+fresh:feature"]).await;
    assert_eq!(indexed(&srv, "fresh start"), vec![format!("{p}:new.txt")]);
    assert!(indexed(&srv, "goodbye").is_empty());

    // a deleted project leaves the index
    owner.delete(&format!("/api/v1/projects/{p}")).await;
    wait_jobs(&srv.state).await;
    assert!(indexed(&srv, "fresh start").is_empty());
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM code_index_state").fetch_one(&srv.state.db).await.unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn what_is_not_text_is_not_indexed(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |cfg| cfg.code_search.max_file_bytes = 64).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    let work = srv.work_dir("kinds");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    std::fs::write(work.join("small.txt"), "findme in a small file\n").unwrap();
    std::fs::write(work.join("large.txt"), format!("findme in a large file\n{}", "x".repeat(100))).unwrap();
    std::fs::write(work.join("binary.bin"), b"findme\0in a binary file").unwrap();
    std::fs::write(
        work.join("big.iso"),
        "version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("findme-target", work.join("link")).unwrap();
    std::fs::write(work.join("latin1.txt"), b"findme caf\xe9\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "kinds"]).await);
    push(&srv, &work, "olivia", "acme/platform/api", &["main"]).await;
    assert_eq!(indexed(&srv, "findme"), vec![format!("{p}:latin1.txt"), format!("{p}:small.txt")]);
    assert!(indexed(&srv, "git-lfs").is_empty());

    // a file that stops being text leaves the index
    std::fs::write(work.join("small.txt"), b"findme\0now binary").unwrap();
    must(git(&work, &["commit", "-q", "-am", "binary now"]).await);
    push(&srv, &work, "olivia", "acme/platform/api", &["main"]).await;
    assert_eq!(indexed(&srv, "findme"), vec![format!("{p}:latin1.txt")]);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn the_sweep_and_reindex_fill_what_is_missing(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let admin = srv.user("root", true).await;
    let (_org, sub, p) = org_project(&owner).await;
    let empty =
        owner.ok_post("/api/v1/projects", json!({ "name": "Empty", "path": "empty", "namespace_id": sub })).await["id"].as_i64().unwrap();
    let work = srv.work_dir("sweep");
    seed_repo(&work).await;
    push(&srv, &work, "olivia", "acme/platform/api", &["main"]).await;

    // an empty repository is recorded as looked at, not queued for ever
    assert_eq!(forgust_server::code_search::indexer::queue_due(&srv.state).await.unwrap(), 1);
    wait_jobs(&srv.state).await;
    assert_eq!(forgust_server::code_search::indexer::queue_due(&srv.state).await.unwrap(), 0);
    let commit: Option<String> = sqlx::query_scalar("SELECT indexed_commit FROM code_index_state WHERE project_id = $1")
        .bind(empty)
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(commit, None);

    // what the index lost comes back with "Reindex code search"
    let index = srv.state.code_index.as_ref().unwrap();
    let w = index.writer().unwrap();
    w.delete_project(p);
    w.commit().unwrap();
    assert!(indexed(&srv, "hello, world").is_empty());
    let tasks = admin.ok_get("/api/v1/admin/tasks").await;
    assert!(tasks.as_array().unwrap().iter().any(|t| t["task"] == "reindex_code"));
    admin.ok_post("/api/v1/admin/tasks/reindex_code", json!({})).await;
    wait_jobs(&srv.state).await;
    wait_jobs(&srv.state).await;
    assert_eq!(indexed(&srv, "hello, world"), vec![format!("{p}:src/main.rs")]);

    // a project deleted behind the index's back (its row stays) is swept out
    sqlx::query("DELETE FROM routes WHERE source_type = 'project' AND source_id = $1").bind(p).execute(&srv.state.db).await.unwrap();
    sqlx::query("DELETE FROM projects WHERE id = $1").bind(p).execute(&srv.state.db).await.unwrap();
    assert_eq!(forgust_server::code_search::indexer::queue_due(&srv.state).await.unwrap(), 1);
    wait_jobs(&srv.state).await;
    assert!(indexed(&srv, "hello, world").is_empty());
}
