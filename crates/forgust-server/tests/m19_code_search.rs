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

// ------------------------------------------------------------ searching

/// A private project (`acme/platform/api`) and a public one (`open/site`),
/// both with code. Returns (owner, private id, public id).
async fn two_projects(srv: &TestServer) -> (Client, i64, i64) {
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, private) = org_project(&owner).await;
    let work = srv.work_dir("private");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    let lib = (1..=40)
        .map(|n| if n == 20 { "pub fn launch_rocket() {}".to_string() } else { format!("// line {n}") })
        .collect::<Vec<_>>()
        .join("\n");
    commit_file(&work, "src/lib.rs", &format!("{lib}\n"), "lib").await;
    commit_file(&work, "docs/Launch.md", "# Launch\n\nCall launch_rocket <now> & wait.\nThen LAUNCH_ROCKET again.\n", "docs").await;
    commit_file(&work, "scripts/deploy.py", "def launch_rocket():\n    pass\n", "script").await;
    push(srv, &work, "olivia", "acme/platform/api", &["main"]).await;

    let group =
        owner.ok_post("/api/v1/groups", json!({ "name": "Open", "path": "open", "visibility": "public" })).await["id"].as_i64().unwrap();
    let public = owner
        .ok_post("/api/v1/projects", json!({ "name": "Site", "path": "site", "namespace_id": group, "visibility": "public" }))
        .await["id"]
        .as_i64()
        .unwrap();
    let work = srv.work_dir("public");
    must(git(&work, &["init", "-q", "-b", "main"]).await);
    commit_file(&work, "index.html", "<h1>launch_rocket soon</h1>\n", "site").await;
    push(srv, &work, "olivia", "open/site", &["main"]).await;
    (owner, private, public)
}

/// `project_path:path` of each file found.
fn files(v: &serde_json::Value) -> Vec<String> {
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| format!("{}:{}", f["project_path"].as_str().unwrap(), f["path"].as_str().unwrap()))
        .collect()
}

const ALL: [&str; 4] =
    ["acme/platform/api:docs/Launch.md", "acme/platform/api:scripts/deploy.py", "acme/platform/api:src/lib.rs", "open/site:index.html"];

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn code_search_shows_only_code_the_caller_may_read(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, private, _public) = two_projects(&srv).await;
    let reporter = srv.user("rita", false).await;
    let guest = srv.user("gus", false).await;
    let stranger = srv.user("sam", false).await;
    let admin = srv.user("root", true).await;
    owner.ok_post(&format!("/api/v1/projects/{private}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    owner.ok_post(&format!("/api/v1/projects/{private}/members"), json!({ "username": "gus", "access_level": "guest" })).await;
    let q = "/api/v1/search/code?q=launch_rocket";

    for c in [&owner, &reporter, &admin] {
        assert_eq!(files(&c.ok_get(q).await), ALL);
    }
    // a Guest of a private project does not read its code; nor do strangers
    // or anonymous people. Nothing says there was more.
    for c in [&guest, &stranger, &Client::new(&srv.base)] {
        let r = c.ok_get(q).await;
        assert_eq!(files(&r), vec!["open/site:index.html"], "{r}");
        assert_eq!(r["next"], json!(null));
        assert_eq!(r["partial"], false);
        assert!(!r.to_string().contains("acme"));
        // one file per page: still nothing after the one they may read
        assert_eq!(c.ok_get(&format!("{q}&per_page=1")).await["next"], json!(null));
        // asking for the private project or its group finds nothing either
        let (code, body) = c.get(&format!("{q}&project_id={private}")).await;
        assert!(matches!(code, 401 | 403 | 404), "{code} {body}");
        let (code, body) = c.get(&format!("{q}&group_id=acme")).await;
        assert!(matches!(code, 401 | 403 | 404) || files(&body).is_empty(), "{code} {body}");
    }
    // tokens: reading the API is enough, and a token sees what its user sees
    let read =
        owner.ok_post("/api/v1/user/tokens", json!({ "name": "r", "scopes": ["read_api"] })).await["token"].as_str().unwrap().to_string();
    assert_eq!(files(&Client::with_token(&srv.base, &read).ok_get(q).await), ALL);
    let other =
        owner.ok_post("/api/v1/user/tokens", json!({ "name": "u", "scopes": ["read_user"] })).await["token"].as_str().unwrap().to_string();
    assert_eq!(Client::with_token(&srv.base, &other).get(q).await.0, 403);

    // made private, the public project's code is gone for the others at once
    owner.ok_put(&format!("/api/v1/projects/{_public}"), json!({ "name": "Site", "description": "", "visibility": "private" })).await;
    assert!(files(&stranger.ok_get(q).await).is_empty());
    assert_eq!(files(&owner.ok_get(q).await), ALL);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn code_search_text_regex_filters_and_pages(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, private, _public) = two_projects(&srv).await;
    let get = |extra: &'static str| {
        let owner = owner.clone();
        async move { owner.ok_get(&format!("/api/v1/search/code?{extra}")).await }
    };

    // the lines: context around the match, real line numbers, marks
    let r = get("q=launch_rocket&path=src/").await;
    assert_eq!(files(&r), vec!["acme/platform/api:src/lib.rs"]);
    let f = &r["files"][0];
    assert_eq!((f["ref"].as_str(), f["language"].as_str(), f["matches"].as_i64()), (Some("main"), Some("Rust"), Some(1)));
    assert_eq!(f["commit"].as_str().unwrap().len(), 40);
    assert_eq!(f["chunks"][0]["start"], 18);
    let lines = f["chunks"][0]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 5);
    assert!(lines[2].as_str().unwrap().contains("<mark>launch_rocket</mark>"), "{lines:?}");
    assert!(lines[2].as_str().unwrap().contains("hl-"), "syntax colours: {lines:?}");
    // text is escaped around the marks
    let md = get("q=launch_rocket%20%3Cnow%3E%20%26").await;
    let line = md["files"][0]["chunks"][0]["lines"][2].as_str().unwrap().to_string();
    assert!(!line.contains("<mark></mark>"), "{line}");
    // without the syntax spans, the marks are one run of escaped text
    let spans = regex::Regex::new("</?span[^>]*>").unwrap();
    assert!(spans.replace_all(&line, "").replace("</mark><mark>", "").contains("<mark>launch_rocket &lt;now&gt; &amp;</mark>"), "{line}");

    // any case by default; exact case on request
    assert_eq!(files(&get("q=LAUNCH_rocket").await), ALL);
    assert_eq!(files(&get("q=LAUNCH_ROCKET&case_sensitive=true").await), vec!["acme/platform/api:docs/Launch.md"]);
    assert_eq!(get("q=launch_rocket&path=docs").await["files"][0]["matches"], 2);
    // regular expressions
    assert_eq!(files(&get("q=fn%20launch_\\w%2B\\(\\)&regex=true").await), vec!["acme/platform/api:src/lib.rs"]);
    assert_eq!(
        files(&get("q=%5E(def|pub%20fn)%20launch&regex=true").await),
        vec!["acme/platform/api:scripts/deploy.py", "acme/platform/api:src/lib.rs"]
    );
    // plain text is not a pattern
    assert!(files(&get("q=launch_\\w%2B").await).is_empty());

    // filters
    let languages = owner.ok_get("/api/v1/search/code/languages").await;
    assert!(languages.as_array().unwrap().iter().any(|l| l == "Python"), "{languages}");
    assert_eq!(languages.as_array().unwrap().iter().filter(|l| *l == "Rust").count(), 1);
    assert_eq!(files(&get("q=launch_rocket&language=python").await), vec!["acme/platform/api:scripts/deploy.py"]);
    assert_eq!(files(&get("q=launch_rocket&filename=LAUNCH.md").await), vec!["acme/platform/api:docs/Launch.md"]);
    assert_eq!(
        files(&get("q=launch_rocket&path=s/").await),
        vec!["acme/platform/api:docs/Launch.md", "acme/platform/api:scripts/deploy.py"]
    );
    assert_eq!(files(&get("q=launch_rocket&project_id=open/site").await), vec!["open/site:index.html"]);
    assert_eq!(files(&get("q=launch_rocket&group_id=acme").await), ALL[..3]);
    assert_eq!(files(&owner.ok_get(&format!("/api/v1/search/code?q=launch_rocket&project_id={private}&language=rust")).await), ALL[2..3]);

    // pages follow `next`, and end
    let mut seen = Vec::new();
    let mut after = String::new();
    for _ in 0..5 {
        let r = owner.ok_get(&format!("/api/v1/search/code?q=launch_rocket&per_page=3{after}")).await;
        seen.extend(files(&r));
        match r["next"].as_str() {
            Some(next) => after = format!("&after={}", next.replace('/', "%2F").replace(':', "%3A")),
            None => break,
        }
    }
    assert_eq!(seen, ALL);
    // exactly a page: no `next` that leads to nothing
    assert_eq!(get("q=launch_rocket&per_page=4").await["next"], json!(null));

    // refused searches say why; the other scopes are unchanged
    for (bad, why) in [
        ("q=la", "at least 3"),
        ("q=\\w%2B&regex=true", "plain text"),
        ("q=launch(&regex=true", "regular expression"),
        ("q=launch_rocket&after=nope", "after"),
    ] {
        let (code, body) = owner.get(&format!("/api/v1/search/code?{bad}")).await;
        assert_eq!(code, 400, "{bad}");
        assert!(body.to_string().contains(why), "{bad}: {body}");
    }
    // title search is as it was: code is not one of its scopes
    assert_eq!(owner.ok_get("/api/v1/search?q=site").await["counts"]["projects"], 1);
    assert_eq!(owner.get("/api/v1/search?q=site&scope=code").await.0, 400);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn anonymous_code_searches_are_limited(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |cfg| cfg.limits.anonymous_heavy_reads_per_minute = 2).await;
    let (owner, _private, _public) = two_projects(&srv).await;
    let anon = Client::new(&srv.base);
    for path in ["/api/v1/search/code?q=launch_rocket", "/api/v4/search?scope=blobs&search=launch_rocket"] {
        assert_eq!(anon.get(path).await.0, 200, "{path}");
    }
    assert_eq!(anon.get("/api/v1/search/code?q=launch_rocket").await.0, 429);
    assert_eq!(anon.get("/api/v4/search?scope=blobs&search=launch_rocket").await.0, 429);
    // titles are not counted, nor are people who signed in
    assert_eq!(anon.get("/api/v1/search?q=site").await.0, 200);
    for _ in 0..4 {
        assert_eq!(owner.get("/api/v1/search/code?q=launch_rocket").await.0, 200);
    }
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn code_search_can_be_turned_off(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |cfg| cfg.code_search.enabled = false).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, _p) = org_project(&owner).await;
    let work = srv.work_dir("off");
    seed_repo(&work).await;
    push(&srv, &work, "olivia", "acme/platform/api", &["main"]).await;
    assert!(!srv.state.cfg.code_index_dir().exists());
    assert_eq!(owner.get("/api/v1/search/code?q=hello").await.0, 503);
    // the rest of search still answers
    assert_eq!(owner.ok_get("/api/v1/search?q=api").await["counts"]["projects"], 1);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn gitlab_api_searches_blobs(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, private, public) = two_projects(&srv).await;
    let stranger = srv.user("sam", false).await;
    let paths = |v: &serde_json::Value| v.as_array().unwrap().iter().map(|b| b["path"].as_str().unwrap().to_string()).collect::<Vec<_>>();

    let all = owner.ok_get("/api/v4/search?scope=blobs&search=launch_rocket").await;
    assert_eq!(paths(&all), vec!["docs/Launch.md", "scripts/deploy.py", "src/lib.rs", "index.html"]);
    let lib = &all[2];
    assert_eq!(
        (lib["basename"].as_str(), lib["filename"].as_str(), lib["ref"].as_str(), lib["startline"].as_i64(), lib["project_id"].as_i64()),
        (Some("lib"), Some("src/lib.rs"), Some("main"), Some(18), Some(private))
    );
    assert_eq!(lib["data"], "// line 18\n// line 19\npub fn launch_rocket() {}\n// line 21\n// line 22\n");
    assert_eq!(lib["id"], json!(null));

    // by project and by group, with GitLab's pages
    assert_eq!(
        paths(&owner.ok_get(&format!("/api/v4/projects/{public}/search?scope=blobs&search=launch_rocket")).await),
        vec!["index.html"]
    );
    assert_eq!(paths(&owner.ok_get("/api/v4/projects/acme%2Fplatform%2Fapi/search?scope=blobs&search=launch_rocket").await).len(), 3);
    assert_eq!(
        paths(&owner.ok_get("/api/v4/groups/acme/search?scope=blobs&search=launch_rocket&per_page=2&page=2").await),
        vec!["src/lib.rs"]
    );
    let r = reqwest::Client::new()
        .get(format!("{}/api/v4/groups/acme/search?scope=blobs&search=launch_rocket&per_page=2", srv.base))
        .header("cookie", owner.cookie.clone().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["x-next-page"], "2");

    // only what the caller may read
    assert_eq!(paths(&stranger.ok_get("/api/v4/search?scope=blobs&search=launch_rocket").await), vec!["index.html"]);
    assert_eq!(stranger.get(&format!("/api/v4/projects/{private}/search?scope=blobs&search=launch_rocket")).await.0, 404);
    // the other scopes are not served
    assert_eq!(owner.get("/api/v4/search?scope=issues&search=launch").await.0, 400);
    assert_eq!(owner.get("/api/v4/search?search=launch").await.0, 400);
}
