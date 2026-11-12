//! M5: minimal issues and labels (the importer moves both).

mod common;

use common::*;
use serde_json::json;

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn issues_labels_and_comments(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let guest = srv.user("gina", false).await;
    let outsider = srv.user("otto", false).await;
    let (org, _sub, proj) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "gina", "access_level": "guest" })).await;
    let base = format!("/api/v1/projects/{proj}");

    // labels: Developer+ manage them; colors are validated, names unique
    let bug = dev.ok_post(&format!("{base}/labels"), json!({ "name": "bug", "color": "#D73A4A" })).await;
    assert_eq!(bug["color"], "#d73a4a");
    let ui = dev.ok_post(&format!("{base}/labels"), json!({ "name": "ui" })).await;
    assert!(ui["color"].as_str().unwrap().starts_with('#'), "a color is picked");
    assert_eq!(dev.post(&format!("{base}/labels"), json!({ "name": "bug" })).await.0, 409);
    assert_eq!(dev.post(&format!("{base}/labels"), json!({ "name": "x", "color": "red" })).await.0, 400);
    assert_eq!(guest.post(&format!("{base}/labels"), json!({ "name": "nope" })).await.0, 403);

    // a guest opens an issue (numbered #1) but cannot set labels
    assert_eq!(guest.post(&format!("{base}/issues"), json!({ "title": "Crash", "labels": ["bug"] })).await.0, 403);
    let i1 = guest.ok_post(&format!("{base}/issues"), json!({ "title": "Crash on start", "description": "It *crashes*." })).await;
    assert_eq!(i1["iid"], 1);
    assert_eq!(i1["author_username"], "gina");
    let i2 = dev.ok_post(&format!("{base}/issues"), json!({ "title": "Dark mode", "labels": ["ui"] })).await;
    assert_eq!(i2["iid"], 2);
    assert_eq!(i2["labels"][0]["name"], "ui");
    assert_eq!(dev.post(&format!("{base}/issues"), json!({ "title": "x", "labels": ["missing"] })).await.0, 400);
    assert_eq!(dev.post(&format!("{base}/issues"), json!({ "title": "  " })).await.0, 400);

    // outsiders do not see the private project's issues
    assert_eq!(outsider.get(&format!("{base}/issues")).await.0, 404);

    // list: counts, label filter, search by title or #iid
    let list = owner.ok_get(&format!("{base}/issues")).await;
    assert_eq!(list["opened"], 2);
    assert_eq!(list["issues"].as_array().unwrap().len(), 2);
    let ui_only = owner.ok_get(&format!("{base}/issues?label=ui")).await;
    assert_eq!(ui_only["issues"].as_array().unwrap().len(), 1);
    assert_eq!(owner.ok_get(&format!("{base}/issues?search=crash")).await["issues"][0]["iid"], 1);
    assert_eq!(owner.ok_get(&format!("{base}/issues?search=%232")).await["issues"][0]["iid"], 2);

    // detail renders Markdown and says what the viewer may do
    let d = guest.ok_get(&format!("{base}/issues/1")).await;
    assert!(d["description_html"].as_str().unwrap().contains("<em>crashes</em>"));
    assert_eq!(d["can_edit"], true, "the author may edit");
    assert_eq!(d["can_label"], false);
    let d2 = guest.ok_get(&format!("{base}/issues/2")).await;
    assert_eq!(d2["can_edit"], false);

    // comments; the guest cannot close someone else's issue
    guest.ok_post(&format!("{base}/issues/2/notes"), json!({ "body": "Please!" })).await;
    assert_eq!(guest.put(&format!("{base}/issues/2"), json!({ "state_event": "close" })).await.0, 403);
    assert_eq!(dev.post(&format!("{base}/issues/2/notes"), json!({ "body": " " })).await.0, 400);

    // a Developer relabels and closes: system notes record both
    let r = dev.put(&format!("{base}/issues/1"), json!({ "labels": ["bug", "ui"], "state_event": "close" })).await;
    assert_eq!(r.0, 200, "{}", r.1);
    assert_eq!(r.1["state"], "closed");
    let notes = dev.ok_get(&format!("{base}/issues/1/notes")).await;
    let bodies: Vec<&str> = notes.as_array().unwrap().iter().map(|n| n["body"].as_str().unwrap()).collect();
    assert!(bodies.contains(&"closed"), "{bodies:?}");
    assert!(bodies.iter().any(|b| b.starts_with("added ~\"bug\"")), "{bodies:?}");
    let list = owner.ok_get(&format!("{base}/issues?state=closed")).await;
    assert_eq!((list["opened"].as_i64(), list["closed"].as_i64()), (Some(1), Some(1)));
    assert_eq!(list["issues"][0]["labels"].as_array().unwrap().len(), 2);

    // the author edits their own comment; others cannot; system notes are fixed
    let mine = guest.ok_get(&format!("{base}/issues/2/notes")).await[0].clone();
    let path = format!("{base}/issues/2/notes/{}", mine["id"]);
    assert_eq!(guest.put(&path, json!({ "body": "Please, soon!" })).await.0, 200);
    assert_eq!(dev.put(&path, json!({ "body": "hijack" })).await.0, 403);
    assert_eq!(dev.delete(&path).await.0, 403, "only the author or a Maintainer deletes");
    assert_eq!(owner.delete(&path).await.0, 200);
    let system = notes.as_array().unwrap().iter().find(|n| n["system"] == true).unwrap();
    assert_eq!(dev.delete(&format!("{base}/issues/1/notes/{}", system["id"])).await.0, 403);

    // labels report their open usage; deleting one detaches it
    let labels = owner.ok_get(&format!("{base}/labels")).await;
    let ui_row = labels.as_array().unwrap().iter().find(|l| l["name"] == "ui").unwrap();
    assert_eq!(ui_row["open_issues"], 1, "issue #1 is closed");
    let renamed = dev.put(&format!("{base}/labels/{}", ui["id"]), json!({ "name": "frontend" })).await;
    assert_eq!(renamed.0, 200);
    assert_eq!(renamed.1["color"], ui["color"], "the color is kept");
    assert_eq!(dev.put(&format!("{base}/labels/{}", ui["id"]), json!({ "name": "bug" })).await.0, 409);
    dev.delete(&format!("{base}/labels/{}", bug["id"])).await;
    let i1 = owner.ok_get(&format!("{base}/issues/1")).await;
    assert_eq!(i1["labels"].as_array().unwrap().len(), 1);
    assert_eq!(i1["labels"][0]["name"], "frontend");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn merge_request_labels(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, proj) = org_project(&owner).await;
    let work = srv.work_dir("labels");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    must(git(&work, &["checkout", "-q", "-b", "topic"]).await);
    std::fs::write(work.join("a.txt"), "a\n").unwrap();
    must(git(&work, &["add", "a.txt"]).await);
    must(git(&work, &["commit", "-q", "-m", "a"]).await);
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "topic"]).await);
    wait_jobs(&srv.state).await;
    let base = format!("/api/v1/projects/{proj}");
    owner.ok_post(&format!("{base}/labels"), json!({ "name": "backend" })).await;
    owner.ok_post(&format!("{base}/merge_requests"), json!({ "source_branch": "topic", "title": "Topic" })).await;

    let r = owner.put(&format!("{base}/merge_requests/1/labels"), json!({ "labels": ["backend"] })).await;
    assert_eq!(r.0, 200, "{}", r.1);
    assert_eq!(owner.ok_get(&format!("{base}/merge_requests")).await[0]["labels"][0]["name"], "backend");
    assert_eq!(owner.ok_get(&format!("{base}/merge_requests/1")).await["labels"][0]["name"], "backend");
    let notes = owner.ok_get(&format!("{base}/merge_requests/1/notes")).await;
    assert!(notes.as_array().unwrap().iter().any(|n| n["system"] == true && n["body"] == "added ~\"backend\""));
    assert_eq!(owner.put(&format!("{base}/merge_requests/1/labels"), json!({ "labels": ["nope"] })).await.0, 400);
    let labels = owner.ok_get(&format!("{base}/labels")).await;
    assert_eq!(labels[0]["open_merge_requests"], 1);
}
