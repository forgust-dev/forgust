//! ADR-0026: the Forgust bot. A Maintainer turns it on; `forgust-bot` joins
//! the project and carries out the commands written in comments
//! (`/label ~bug`), for those who could do the same from the page and those
//! `.forgust/bot.toml` adds. What it refuses, it says in a reply.

mod common;

use common::*;
use serde_json::{Value, json};

const REPO: &str = "acme/platform/api";
const BOT: &str = "forgust-bot";

/// Owner (olivia), a Developer (dave), a Reporter (rita), a pushed `main`.
async fn setup(srv: &TestServer) -> (Client, Client, Client, i64, i64, std::path::PathBuf) {
    let owner = srv.user("olivia", false).await;
    let dev = srv.user("dave", false).await;
    let rep = srv.user("rita", false).await;
    let (org, _sub, proj) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    owner.ok_post(&format!("/api/v1/groups/{org}/members"), json!({ "username": "rita", "access_level": "reporter" })).await;
    let work = srv.work_dir("bot");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    (owner, dev, rep, org, proj, work)
}

fn names(v: &Value, key: &str) -> Vec<String> {
    let mut out: Vec<String> = v.as_array().unwrap().iter().map(|x| x[key].as_str().unwrap().to_string()).collect();
    out.sort();
    out
}

/// The bot's newest comment on `path` (an issue or a merge request).
async fn last_reply(c: &Client, path: &str) -> String {
    let notes = c.ok_get(&format!("{path}/notes")).await;
    notes
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|n| n["author_username"] == BOT && n["system"] == false)
        .map(|n| n["body"].as_str().unwrap().to_string())
        .unwrap_or_default()
}

async fn replies(c: &Client, path: &str) -> usize {
    let notes = c.ok_get(&format!("{path}/notes")).await;
    notes.as_array().unwrap().iter().filter(|n| n["author_username"] == BOT && n["system"] == false).count()
}

async fn bot_level(srv: &TestServer, proj: i64) -> Option<i16> {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM users WHERE username = $1").bind(BOT).fetch_optional(&srv.state.db).await.unwrap();
    forgust_db::members::level_of(&srv.state.db, "project", proj, id?).await.unwrap()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn commands_on_issues(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, _org, proj, work) = setup(&srv).await;
    let base = format!("/api/v1/projects/{proj}");
    for l in ["bug", "needs info", "ui"] {
        owner.ok_post(&format!("{base}/labels"), json!({ "name": l })).await;
    }
    owner.ok_post(&format!("{base}/milestones"), json!({ "title": "1.0 beta" })).await;
    rep.ok_post(&format!("{base}/issues"), json!({ "title": "It crashes", "description": "" })).await;
    let issue = format!("{base}/issues/1");

    // off: a command is a comment like any other, and the composer offers none
    assert_eq!(rep.ok_get(&format!("{base}/bot/commands")).await, json!([]));
    assert_eq!(owner.ok_get(&format!("{base}/bot")).await["enabled"], false);
    dev.ok_post(&format!("{issue}/notes"), json!({ "body": "/close" })).await;
    assert_eq!(dev.ok_get(&issue).await["state"], "opened");
    assert_eq!(bot_level(&srv, proj).await, None);

    // Maintainers turn it on; the bot joins as a Developer
    let (code, _) = dev.put(&format!("{base}/bot"), json!({ "enabled": true })).await;
    assert_eq!(code, 403);
    let s = owner.ok_put(&format!("{base}/bot"), json!({ "enabled": true })).await;
    assert_eq!((s["enabled"].clone(), s["username"].clone(), s["config_error"].clone()), (json!(true), json!(BOT), Value::Null), "{s}");
    assert_eq!(bot_level(&srv, proj).await, Some(forgust_db::access::DEVELOPER));
    assert_eq!(srv.audit_events("bot_enabled").await.len(), 1);

    // what the composer suggests after a `/` (anyone who can comment asks)
    let offered = rep.ok_get(&format!("{base}/bot/commands")).await;
    assert_eq!(offered.as_array().unwrap().len(), 14);
    assert_eq!(offered[2], json!({ "name": "label", "arguments": "~label …", "description": "Add labels", "merge_request_only": false }));

    // GitLab's way of writing arguments; the comment stays as written
    let body = "Looks real.\n/label ~bug ~\"needs info\"\n/assign @rita me\n/milestone %\"1.0 beta\"";
    dev.ok_post(&format!("{issue}/notes"), json!({ "body": body })).await;
    let d = dev.ok_get(&issue).await;
    assert_eq!(names(&d["labels"], "name"), vec!["bug", "needs info"]);
    assert_eq!(names(&d["assignees"], "username"), vec!["dave", "rita"]);
    assert_eq!(d["milestone_title"], "1.0 beta");
    let notes = dev.ok_get(&format!("{issue}/notes")).await;
    let notes = notes.as_array().unwrap();
    assert!(notes.iter().any(|n| n["body"] == body && n["author_username"] == "dave"), "the comment is kept");
    // carried out by the bot: its system notes, and no reply when all went well
    assert!(
        notes
            .iter()
            .any(|n| n["system"] == true && n["author_username"] == BOT && n["body"].as_str().unwrap().starts_with("added ~\"bug\""))
    );
    assert_eq!(replies(&dev, &issue).await, 0);
    let audit = srv.audit_events("bot_command").await;
    assert_eq!(audit.len(), 3, "{audit:?}");
    let dave = srv.user_id("dave").await;
    assert!(audit.iter().all(|(kind, target, d)| kind == "bot"
        && *target == Some(proj)
        && d["by"] == dave
        && d["outcome"] == "done"
        && d["issue"] == 1));
    assert_eq!(audit[0].2["command"], "label");

    // a Reporter assigns (as on the page) but does not label; code is no command
    rep.ok_post(&format!("{issue}/notes"), json!({ "body": "/unassign @dave\n/unlabel ~bug\n```\n/close\n```\n/frobnicate" })).await;
    let d = rep.ok_get(&issue).await;
    assert_eq!(names(&d["assignees"], "username"), vec!["rita"]);
    assert_eq!(names(&d["labels"], "name"), vec!["bug", "needs info"]);
    assert_eq!(d["state"], "opened");
    let reply = last_reply(&rep, &issue).await;
    assert!(reply.starts_with("@rita "), "{reply}");
    assert!(reply.contains("- `/unlabel ~bug`: you are not allowed to run `/unlabel` here"), "{reply}");
    assert!(reply.contains("- `/frobnicate`: there is no such command"), "{reply}");
    assert!(!reply.contains("`/unassign @dave`") && reply.contains("The commands are `/assign`"), "{reply}");
    let refused: Vec<_> = srv.audit_events("bot_command").await.into_iter().filter(|(_, _, d)| d["outcome"] == "refused").collect();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].2["command"], "unlabel");

    // the services' refusals come back as answers; one reply per comment, and
    // the bot does not answer itself
    dev.ok_post(&format!("{issue}/notes"), json!({ "body": "/label ~nope\n/hold\n/milestone %2.0\n/assign @nobody" })).await;
    let reply = last_reply(&dev, &issue).await;
    for says in [
        "`/label ~nope`: no label `nope` in this project",
        "`/hold`: `/hold` works on merge requests only",
        "`/milestone %2.0`: there is no milestone `2.0`",
        "`/assign @nobody`: there is no user `nobody`",
    ] {
        assert!(reply.contains(says), "{says}: {reply}");
    }
    assert_eq!(replies(&dev, &issue).await, 2);

    // an answer to an internal comment is internal
    rep.ok_post(&format!("{issue}/notes"), json!({ "body": "/close\n/label ~ui", "internal": true })).await;
    let notes = rep.ok_get(&format!("{issue}/notes")).await;
    let last = notes.as_array().unwrap().iter().rev().find(|n| n["author_username"] == BOT && n["system"] == false).unwrap().clone();
    assert_eq!(last["internal"], true, "{last}");
    // (rita wrote the issue: she closes it)
    assert_eq!(rep.ok_get(&issue).await["state"], "closed");
    dev.ok_post(&format!("{issue}/notes"), json!({ "body": "/reopen\n/remove_milestone\n/unassign" })).await;
    let d = dev.ok_get(&issue).await;
    assert_eq!((d["state"].clone(), d["milestone_title"].clone()), (json!("opened"), Value::Null));
    assert!(d["assignees"].as_array().unwrap().is_empty());

    // the project's rules, from the default branch: authors label their own
    // issues, nobody closes by command
    commit_file(&work, ".forgust/bot.toml", "[commands.label]\nallow = [\"author\"]\n\n[commands.close]\nenabled = false\n", "bot rules")
        .await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    rep.ok_post(&format!("{issue}/notes"), json!({ "body": "/label ~ui\n/unlabel ~bug" })).await;
    assert_eq!(names(&rep.ok_get(&issue).await["labels"], "name"), vec!["bug", "needs info", "ui"]);
    assert!(last_reply(&rep, &issue).await.contains("not allowed to run `/unlabel`"));
    owner.ok_post(&format!("{issue}/notes"), json!({ "body": "/close" })).await;
    assert_eq!(owner.ok_get(&issue).await["state"], "opened");
    assert!(last_reply(&owner, &issue).await.contains("`/close` is turned off in this project (`.forgust/bot.toml`)"));
    assert!(!names(&rep.ok_get(&format!("{base}/bot/commands")).await, "name").contains(&"close".to_string()));

    // rules that cannot be read: nothing runs, and the reply says why
    commit_file(&work, ".forgust/bot.toml", "[commands.lable]\nallow = [\"author\"]\n", "typo").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&format!("{issue}/notes"), json!({ "body": "/unlabel" })).await;
    assert_eq!(names(&dev.ok_get(&issue).await["labels"], "name"), vec!["bug", "needs info", "ui"]);
    let reply = last_reply(&dev, &issue).await;
    assert!(reply.contains("no command was run") && reply.contains("no `/lable` command"), "{reply}");
    assert!(owner.ok_get(&format!("{base}/bot")).await["config_error"].as_str().unwrap().contains("lable"));

    // off again: the bot leaves, comments are comments
    let n = replies(&dev, &issue).await;
    owner.ok_put(&format!("{base}/bot"), json!({ "enabled": false })).await;
    assert_eq!(bot_level(&srv, proj).await, None);
    dev.ok_post(&format!("{issue}/notes"), json!({ "body": "/unlabel" })).await;
    assert_eq!(replies(&dev, &issue).await, n);
    assert_eq!(srv.audit_events("bot_disabled").await.len(), 1);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn commands_on_merge_requests(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, rep, _org, proj, work) = setup(&srv).await;
    let base = format!("/api/v1/projects/{proj}");
    owner.ok_put(&format!("{base}/bot"), json!({ "enabled": true })).await;
    owner.ok_post(&format!("{base}/labels"), json!({ "name": "bug" })).await;
    // only Maintainers merge into main
    owner
        .ok_post(
            &format!("{base}/protected_branches"),
            json!({ "name": "main", "push_access_level": "maintainer", "merge_access_level": "maintainer" }),
        )
        .await;

    // a merge request that hands everyone `/label`: its own rules do not count
    must(git(&work, &["checkout", "-q", "-b", "feature/a"]).await);
    commit_file(&work, ".forgust/bot.toml", "[commands.label]\nallow = [\"anyone\"]\n", "let everyone label").await;
    must(git(&work, &["push", "-q", &srv.http_url("dave", REPO), "feature/a"]).await);
    wait_jobs(&srv.state).await;
    dev.ok_post(&format!("{base}/merge_requests"), json!({ "source_branch": "feature/a", "title": "A" })).await;
    let mr = format!("{base}/merge_requests/1");
    rep.ok_post(&format!("{mr}/notes"), json!({ "body": "/label ~bug" })).await;
    assert!(dev.ok_get(&mr).await["labels"].as_array().unwrap().is_empty());
    assert!(last_reply(&rep, &mr).await.contains("not allowed to run `/label`"));

    // triage, by its author
    dev.ok_post(&format!("{mr}/notes"), json!({ "body": "/label bug\n/cc @olivia\n/assign\n/hold" })).await;
    let d = dev.ok_get(&mr).await;
    assert_eq!(names(&d["labels"], "name"), vec!["bug"]);
    assert_eq!(names(&d["reviewers"], "username"), vec!["olivia"]);
    assert_eq!(names(&d["assignees"], "username"), vec!["dave"]);
    assert_eq!(replies(&dev, &mr).await, 1, "only rita's refusal so far");

    // on hold: it does not merge, whoever asks
    let status = owner.ok_get(&format!("{mr}/merge_status")).await;
    assert_eq!(status["can_merge"], false);
    assert!(status["blockers"].to_string().contains("put on hold by @dave"), "{status}");
    // (`/merge` then waits for the hold to be lifted, as for any other check:
    // canceled here, to see each step)
    owner.ok_post(&format!("{mr}/notes"), json!({ "body": "/merge" })).await;
    assert_eq!(owner.ok_get(&mr).await["state"], "opened");
    assert!(dev.ok_get(&format!("{mr}/notes")).await.to_string().contains("set auto-merge"));
    owner.delete(&format!("{mr}/auto_merge")).await;
    dev.ok_post(&format!("{mr}/notes"), json!({ "body": "/hold" })).await;
    assert!(last_reply(&dev, &mr).await.contains("already on hold"));
    dev.ok_post(&format!("{mr}/notes"), json!({ "body": "/unhold" })).await;
    let notes = dev.ok_get(&format!("{mr}/notes")).await.to_string();
    assert!(notes.contains("put this merge request on hold") && notes.contains("took this merge request off hold"));

    // `/rebase`, `/retest` and `/merge` run as the commenter, with their rights
    must(git(&work, &["checkout", "-q", "main"]).await);
    commit_file(&work, "main.txt", "m\n", "main moves").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    assert_eq!(owner.ok_get(&format!("{mr}/merge_status")).await["fast_forward"], false);
    rep.ok_post(&format!("{mr}/notes"), json!({ "body": "/rebase\n/merge" })).await;
    let reply = last_reply(&rep, &mr).await;
    assert!(reply.contains("`/rebase`:") && reply.contains("`/merge`: you are not allowed to merge into `main`"), "{reply}");
    dev.ok_post(&format!("{mr}/notes"), json!({ "body": "/rebase\n/retest" })).await;
    wait_jobs(&srv.state).await;
    assert_eq!(dev.ok_get(&format!("{mr}/merge_status")).await["fast_forward"], true);
    assert!(last_reply(&dev, &mr).await.contains("`/retest`: no pipeline ran for the latest commit"));
    dev.ok_post(&format!("{mr}/notes"), json!({ "body": "/merge" })).await;
    assert!(last_reply(&dev, &mr).await.contains("`/merge`: you are not allowed to merge into `main`"));
    assert_eq!(dev.ok_get(&mr).await["state"], "opened");

    // no merge train here: `/merge` merges once every check passes, as the
    // person who wrote it
    owner.ok_post(&format!("{mr}/notes"), json!({ "body": "LGTM\n/merge" })).await;
    wait_jobs(&srv.state).await;
    let d = owner.ok_get(&mr).await;
    assert_eq!(d["state"], "merged", "{}", last_reply(&owner, &mr).await);
    assert_eq!(d["merged_by"], srv.user_id("olivia").await);
    // and the rules it brought apply from now on
    dev.ok_post(&format!("{base}/merge_requests"), json!({ "source_branch": "feature/a", "title": "A again" })).await;
    let outcomes: Vec<String> = srv
        .audit_events("bot_command")
        .await
        .iter()
        .map(|(_, _, d)| format!("{} {}", d["command"].as_str().unwrap(), d["outcome"].as_str().unwrap()))
        .collect();
    assert!(outcomes.contains(&"merge done".to_string()) && outcomes.contains(&"merge refused".to_string()), "{outcomes:?}");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn groups_turn_it_on_for_their_projects(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let (owner, dev, _rep, org, proj, _work) = setup(&srv).await;
    let (code, _) = dev.put(&format!("/api/v1/groups/{org}/bot"), json!({ "enabled": true })).await;
    assert_eq!(code, 403);
    let s = owner.ok_put(&format!("/api/v1/groups/{org}/bot"), json!({ "enabled": true })).await;
    assert_eq!((s["enabled"].clone(), s["turned_on"].clone()), (json!(true), json!(1)), "{s}");
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{proj}/bot")).await["enabled"], true);
    // new projects below it too; a project that chose keeps its choice
    owner.ok_put(&format!("/api/v1/projects/{proj}/bot"), json!({ "enabled": false })).await;
    let web = owner.ok_post("/api/v1/projects", json!({ "name": "Web", "path": "web", "namespace_id": org })).await["id"].as_i64().unwrap();
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{web}/bot")).await["enabled"], true);
    assert_eq!(bot_level(&srv, web).await, Some(forgust_db::access::DEVELOPER));
    let s = owner.ok_put(&format!("/api/v1/groups/{org}/bot"), json!({ "enabled": true })).await;
    assert_eq!(s["turned_on"], 0);
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{proj}/bot")).await["enabled"], false);
    assert_eq!(srv.audit_events("bot_group_default").await.len(), 2);
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn an_instance_can_do_without(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.bot.enabled = false).await;
    let (owner, _dev, _rep, _org, proj, _work) = setup(&srv).await;
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{proj}/bot")).await["available"], false);
    let (code, v) = owner.put(&format!("/api/v1/projects/{proj}/bot"), json!({ "enabled": true })).await;
    assert_eq!(code, 409, "{v}");
}
