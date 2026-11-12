//! An archived project is read-only. Every way to write to it
//! is refused (a table, as each path checks rights its own way); reading,
//! cloning and the Owner's lifecycle actions (unarchive, transfer, delete) are not.

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";

/// The actions a registry token grants on `name`.
fn granted(t: &Value, name: &str) -> Vec<String> {
    use base64::Engine;
    let body = t["token"].as_str().unwrap().split('.').nth(1).unwrap();
    let claims: Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body).unwrap()).unwrap();
    claims["access"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["name"] == name)
        .flat_map(|a| a["actions"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()))
        .collect()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn an_archived_project_is_read_only(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let guest = srv.user("gus", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "gus", "access_level": "guest" })).await;
    let work = srv.work_dir("arch");
    seed_repo(&work).await;
    std::fs::write(work.join(".gitlab-ci.yml"), "t:\n  script: [x]\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "ci"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    commit_file(&work, "t.txt", "t\n", "topic").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "topic"]).await);
    wait_jobs(&srv.state).await;
    let base = format!("/api/v1/projects/{p}");
    dev.ok_post(&format!("{base}/merge_requests"), json!({ "source_branch": "topic", "title": "Topic" })).await;
    let issue = guest.ok_post(&format!("{base}/issues"), json!({ "title": "By a guest" })).await;
    let schedule =
        owner.ok_post(&format!("{base}/pipeline_schedules"), json!({ "description": "nightly", "ref": "main", "cron": "0 3 * * *" })).await;
    let pipelines_before = owner.ok_get(&format!("{base}/pipelines")).await.as_array().unwrap().len();

    // only an Owner archives
    assert_eq!(dev.post(&format!("{base}/archive"), json!({})).await.0, 403);
    let a = owner.ok_post(&format!("{base}/archive"), json!({})).await;
    assert!(a["archived_at"].is_string(), "{a}");
    let shown = dev.ok_get(&base).await;
    assert_eq!(
        (shown["archived"].as_bool(), shown["can_push"].as_bool(), shown["can_admin"].as_bool()),
        (Some(true), Some(false), Some(false))
    );

    // every write: refused (whoever tries, through whichever way)
    let iid = issue["iid"].as_i64().unwrap();
    let writes: Vec<(&Client, &str, String, Value)> = vec![
        (&owner, "PUT", base.clone(), json!({ "name": "API", "description": "changed", "visibility": "private" })),
        (&owner, "POST", format!("{base}/variables"), json!({ "key": "X", "value": "y" })),
        (&owner, "POST", format!("{base}/members"), json!({ "username": "gus", "access_level": "developer" })),
        (&owner, "POST", format!("{base}/labels"), json!({ "name": "bug" })),
        (&owner, "POST", format!("{base}/pipelines"), json!({ "ref": "main" })),
        (&owner, "POST", format!("{base}/releases"), json!({ "tag_name": "v1", "ref": "main" })),
        (&dev, "POST", format!("{base}/repository/branches"), json!({ "branch": "new", "ref": "main" })),
        (
            &dev,
            "POST",
            format!("{base}/repository/commits"),
            json!({ "branch": "main", "commit_message": "web edit", "actions": [{ "action": "create", "file_path": "w.txt", "content": "w" }] }),
        ),
        (&dev, "POST", format!("{base}/merge_requests"), json!({ "source_branch": "topic", "title": "Again" })),
        (&dev, "POST", format!("{base}/merge_requests/1/notes"), json!({ "body": "a comment" })),
        (&owner, "POST", format!("{base}/merge_requests/1/approve"), json!({})),
        (&owner, "POST", format!("{base}/merge_requests/1/request_changes"), json!({ "body": "no" })),
        (&dev, "PUT", format!("{base}/merge_requests/1"), json!({ "title": "Renamed" })),
        (&guest, "POST", format!("{base}/issues"), json!({ "title": "Another" })),
        (&guest, "POST", format!("{base}/issues/{iid}/notes"), json!({ "body": "a comment" })),
        (&guest, "PUT", format!("{base}/issues/{iid}"), json!({ "title": "Edited by its author" })),
        (&guest, "POST", format!("{base}/issues/{iid}/reactions"), json!({ "name": "thumbsup" })),
        (&owner, "PUT", format!("{base}/issues/{iid}"), json!({ "assignee_ids": [] })),
    ];
    for (who, method, path, body) in writes {
        let (s, b) = match method {
            "PUT" => who.put(&path, body).await,
            _ => who.post(&path, body).await,
        };
        assert_eq!(s, 403, "{method} {path} should be refused: {b}");
        assert!(b.to_string().contains("archived"), "{method} {path}: {b}");
    }
    // merging: refused with the reason
    let (s, b) = dev.post(&format!("{base}/merge_requests/1/merge"), json!({})).await;
    assert!(s == 403 || s == 409, "{s} {b}");
    assert!(dev.ok_get(&format!("{base}/merge_requests/1/merge_status")).await["blockers"].to_string().contains("archived"));
    // /api/v4 goes through the same services
    let tok = owner.ok_post("/api/v1/user/tokens", json!({ "name": "v4", "scopes": ["api"] })).await["token"].as_str().unwrap().to_string();
    let v4 = Client::with_token(&srv.base, &tok);
    assert_eq!(v4.post(&format!("/api/v4/projects/{p}/issues"), json!({ "title": "v4" })).await.0, 403);
    assert_eq!(v4.ok_get(&format!("/api/v4/projects/{p}")).await["archived"], true, "Renovate and glab skip archived projects");
    // git push: refused; clone still works
    commit_file(&work, "late.txt", "late\n", "late").await;
    let pushed = git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "topic"]).await;
    assert!(!pushed.ok && pushed.stderr.contains("archived"), "{}", pushed.stderr);
    let clone = srv.work_dir("clone");
    must(git(&clone, &["clone", "-q", &srv.http_url("dave", REPO), "."]).await);
    // LFS uploads and registry pushes: read-only
    let lfs = reqwest::Client::new()
        .post(format!("{}/{REPO}.git/info/lfs/objects/batch", srv.base))
        .basic_auth("dave", Some(password("dave")))
        .header("accept", "application/vnd.git-lfs+json")
        .json(&json!({ "operation": "upload", "objects": [{ "oid": "a".repeat(64), "size": 1 }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(lfs.status(), StatusCode::FORBIDDEN);
    let t: Value = reqwest::Client::new()
        .get(format!("{}/jwt/auth", srv.base))
        .query(&[("service", "container_registry"), ("scope", &format!("repository:{REPO}:pull,push"))])
        .basic_auth("olivia", Some(password("olivia")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(granted(&t, REPO), vec!["pull"], "{t}");
    // schedules wait (not turned off): due now, nothing runs
    sqlx::query("UPDATE ci_pipeline_schedules SET next_run_at = now() - interval '1 minute'").execute(&srv.state.db).await.unwrap();
    forgust_server::ci::sweep(&srv.state).await.unwrap();
    wait_jobs(&srv.state).await;
    assert_eq!(owner.ok_get(&format!("{base}/pipelines")).await.as_array().unwrap().len(), pipelines_before);
    // (its settings are closed while archived: read the schedule directly)
    let active: bool = sqlx::query_scalar("SELECT active FROM ci_pipeline_schedules WHERE id = $1")
        .bind(schedule["id"].as_i64().unwrap())
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert!(active, "still on, it waits");

    // reading stays open, and the Owner unarchives
    owner.ok_get(&format!("{base}/issues/{iid}")).await;
    owner.ok_get(&format!("{base}/merge_requests/1")).await;
    assert_eq!(dev.post(&format!("{base}/unarchive"), json!({})).await.0, 403);
    let u = owner.ok_post(&format!("{base}/unarchive"), json!({})).await;
    assert!(u["archived_at"].is_null());
    guest.ok_post(&format!("{base}/issues"), json!({ "title": "Writable again" })).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "topic"]).await);
}
