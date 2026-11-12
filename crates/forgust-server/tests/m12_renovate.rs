//! ADR-0023: built-in Renovate. A Maintainer turns dependency
//! updates on; Forgust runs Renovate as a CI job of its own, as a bot of that
//! one project, with a token that lives while the job runs and none of the
//! project's CI variables.

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn request(srv: &TestServer, token: &str) -> Option<Value> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v4/jobs/request", srv.base))
        .json(&json!({ "token": token, "system_id": "s_rn", "info": { "version": "19.5.3", "executor": "docker" } }))
        .send()
        .await
        .unwrap();
    if r.status() == StatusCode::CREATED { Some(r.json().await.unwrap()) } else { None }
}

/// The next job of `project` for the instance runner: another project's due run
/// (the scheduler may queue it any time) is finished and skipped.
async fn request_for(srv: &TestServer, token: &str, project: i64) -> Option<Value> {
    for _ in 0..5 {
        let job = request(srv, token).await?;
        if var(&job, "CI_PROJECT_ID") == Some(project.to_string()) {
            return Some(job);
        }
        finish(srv, &job, "success").await;
    }
    None
}

async fn finish(srv: &TestServer, job: &Value, state: &str) {
    let r = reqwest::Client::new()
        .put(format!("{}/api/v4/jobs/{}", srv.base, job["id"]))
        .json(&json!({ "token": job["token"], "state": state, "exit_code": if state == "success" { 0 } else { 1 } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    wait_jobs(&srv.state).await;
}

fn var(job: &Value, k: &str) -> Option<String> {
    job["variables"].as_array().unwrap().iter().rev().find(|v| v["key"] == k).map(|v| v["value"].as_str().unwrap().to_string())
}

fn masked(job: &Value, k: &str) -> bool {
    job["variables"].as_array().unwrap().iter().any(|v| v["key"] == k && v["masked"] == true)
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn built_in_renovate_runs_as_a_bot_of_the_project(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| {
        c.ci.long_poll_secs = 1;
        c.renovate.tags = vec!["renovate".into()];
        c.renovate.github_com_token = Some("ghp_changelogs".into());
    })
    .await;
    let owner = srv.user("olivia", false).await;
    let (_org, sub, project) = org_project(&owner).await;
    let other =
        owner.ok_post("/api/v1/projects", json!({ "name": "Web", "path": "web", "namespace_id": sub })).await["id"].as_i64().unwrap();
    let work = srv.work_dir("rn");
    seed_repo(&work).await;
    std::fs::write(work.join(".gitlab-ci.yml"), "test:\n  script: [make test]\n").unwrap();
    must(git(&work, &["add", "."]).await);
    must(git(&work, &["commit", "-q", "-m", "ci"]).await);
    for path in ["acme/platform/api", "acme/platform/web"] {
        must(git(&work, &["push", "-q", &srv.http_url("olivia", path), "main"]).await);
    }
    wait_jobs(&srv.state).await;
    let main_head = srv.state.storage.resolve(project, "refs/heads/main").await.unwrap().unwrap();
    owner
        .ok_post(
            &format!("/api/v1/projects/{project}/protected_branches"),
            json!({ "name": "main", "push_access_level": "maintainer", "merge_access_level": "maintainer" }),
        )
        .await;
    owner
        .ok_post(
            &format!("/api/v1/projects/{project}/variables"),
            json!({ "key": "DEPLOY_SECRET", "value": "only-protected-value", "protected": true }),
        )
        .await;
    owner.ok_post(&format!("/api/v1/groups/{sub}/variables"), json!({ "key": "GROUP_SECRET", "value": "group-value-123" })).await;
    // Renovate jobs carry instance secrets: only instance runners take them,
    // even with the project's shared runners off
    let admin = srv.user("root", true).await;
    let runner = admin
        .ok_post("/api/v1/admin/runners", json!({ "description": "shared", "tags": ["renovate"], "run_untagged": false }))
        .await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let own_runner = owner
        .ok_post(
            &format!("/api/v1/projects/{project}/runners"),
            json!({ "description": "mine", "tags": ["renovate"], "run_untagged": false }),
        )
        .await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, b) = owner
        .put(
            &format!("/api/v1/projects/{project}/ci/settings"),
            json!({ "ci_config_path": ".gitlab-ci.yml", "instance_runners_enabled": false, "default_timeout_secs": 3600, "default_git_depth": 20,
                    "only_merge_if_pipeline_succeeds": false, "job_token_scope_enabled": true }),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let rn = format!("/api/v1/projects/{project}/renovate");

    // Maintainers only
    let dev = srv.user("dave", false).await;
    owner.ok_post(&format!("/api/v1/projects/{project}/members"), json!({ "username": "dave", "access_level": "developer" })).await;
    assert_eq!(dev.get(&rn).await.0, 403);
    assert_eq!(dev.put(&rn, json!({ "enabled": true })).await.0, 403);
    assert_eq!(owner.put(&rn, json!({ "enabled": true, "interval_hours": 0 })).await.0, 400);

    // on: a bot joins as a Developer, a first run is due now
    let (s, st) = owner.put(&rn, json!({ "enabled": true, "interval_hours": 24, "onboarding": false })).await;
    assert_eq!(s, 200, "{st}");
    let bot = format!("renovate-bot-{project}");
    assert_eq!(st["bot_username"], bot.as_str());
    assert!(st["next_run_at"].is_string());
    let members = owner.ok_get(&format!("/api/v1/projects/{project}/members")).await;
    let m = members.as_array().unwrap().iter().find(|m| m["username"] == bot.as_str()).unwrap_or_else(|| panic!("{members}"));
    assert_eq!(m["access_level"], "developer");
    assert!(owner.ok_get(&format!("/api/v1/projects/{project}/access_tokens")).await.as_array().unwrap().is_empty(), "not an access token");
    // the other project has it on too, with its own bot
    let (s, _) = owner.put(&format!("/api/v1/projects/{other}/renovate"), json!({ "enabled": true })).await;
    assert_eq!(s, 200);
    // its first run waits until the sweep below (the scheduler runs in the background)
    let hold = |when: &'static str| sqlx::query(when).bind(other).execute(&srv.state.db);
    hold("UPDATE renovate_settings SET next_run_at = now() + interval '1 day' WHERE project_id = $1").await.unwrap();

    // Run now: one at a time
    let started = owner.ok_post(&format!("{rn}/run"), json!({})).await;
    let iid = started["pipeline_iid"].as_i64().expect("a pipeline");
    assert_eq!(owner.post(&format!("{rn}/run"), json!({})).await.0, 409);

    assert!(request(&srv, &own_runner).await.is_none(), "not for a project's own runner");
    let job = request_for(&srv, &runner, project).await.expect("the renovate job");
    assert_eq!(job["image"]["name"], "renovate/renovate:44.226.3");
    assert_eq!(var(&job, "CI_PIPELINE_SOURCE").as_deref(), Some("renovate"));
    assert_eq!(var(&job, "GIT_STRATEGY").as_deref(), Some("none"));
    assert_eq!(var(&job, "RENOVATE_PLATFORM").as_deref(), Some("gitlab"));
    assert_eq!(var(&job, "RENOVATE_ENDPOINT"), Some(format!("{}/api/v4/", srv.base)));
    assert_eq!(var(&job, "RENOVATE_REPOSITORIES").as_deref(), Some("acme/platform/api"));
    assert_eq!(var(&job, "RENOVATE_ONBOARDING").as_deref(), Some("false"));
    assert_eq!(var(&job, "RENOVATE_REQUIRE_CONFIG").as_deref(), Some("optional"));
    assert!(var(&job, "RENOVATE_GIT_AUTHOR").unwrap().contains(&bot));
    assert!(masked(&job, "RENOVATE_TOKEN") && masked(&job, "RENOVATE_GITHUB_COM_TOKEN") && masked(&job, "RENOVATE_HOST_RULES"));
    assert_eq!(var(&job, "GITLAB_USER_LOGIN").as_deref(), Some(bot.as_str()), "runs as the bot, not who clicked");
    // none of the project's or groups' CI variables
    assert!(var(&job, "DEPLOY_SECRET").is_none() && var(&job, "GROUP_SECRET").is_none(), "no CI variables");

    // the token: this project, as the bot; not the other project
    let token = var(&job, "RENOVATE_TOKEN").unwrap();
    let as_bot = Client::with_token(&srv.base, &token);
    let (s, me) = as_bot.get("/api/v4/user").await;
    assert_eq!((s, me["username"].as_str()), (200, Some(bot.as_str())));
    assert_eq!(as_bot.get(&format!("/api/v4/projects/{project}")).await.0, 200);
    assert_eq!(as_bot.get(&format!("/api/v4/projects/{other}")).await.0, 404, "one project only");
    // it opens the Dependency Dashboard, as Renovate does
    let dash = as_bot
        .ok_post(
            &format!("/api/v1/projects/{project}/issues"),
            json!({ "title": "Dependency Dashboard", "description": "- [ ] <!-- rebase-all -->Rebase all" }),
        )
        .await;

    // never the commit's or the branch's pipeline
    let (_, pls) = owner.get(&format!("/api/v4/projects/{project}/pipelines?ref=main")).await;
    assert!(pls.as_array().unwrap().iter().all(|p| p["source"] != "renovate"), "{pls}");
    let all = owner.ok_get(&format!("/api/v1/projects/{project}/pipelines")).await;
    let listed = serde_json::to_string(&all).unwrap();
    assert!(listed.contains("\"renovate\""), "listed with the project's pipelines: {listed}");
    let (_, commit) = owner.get(&format!("/api/v4/projects/{project}/repository/commits/{main_head}")).await;
    assert_ne!(commit["last_pipeline"]["iid"], iid);

    // the run ends: its token goes
    finish(&srv, &job, "success").await;
    assert_eq!(as_bot.get("/api/v4/user").await.0, 401, "revoked with the run");
    let st = owner.ok_get(&rn).await;
    assert_eq!(st["last_run"]["pipeline_iid"], iid);
    assert_eq!(st["last_run"]["status"], "success");

    // a person retries the run: it still runs as the bot, with a bot token
    let (s, b) = owner.post(&format!("/api/v1/projects/{project}/jobs/{}/retry", job["id"]), json!({})).await;
    assert_eq!(s, 200, "{b}");
    let again = request_for(&srv, &runner, project).await.expect("the retried job");
    assert_eq!(var(&again, "GITLAB_USER_LOGIN").as_deref(), Some(bot.as_str()));
    let (s, me) = Client::with_token(&srv.base, &var(&again, "RENOVATE_TOKEN").unwrap()).get("/api/v4/user").await;
    assert_eq!((s, me["username"].as_str()), (200, Some(bot.as_str())), "never a token of who retried");
    finish(&srv, &again, "success").await;

    // a push to main brings the next run forward, but not sooner than an hour
    // after the last run (a busy project does not hold the runners)
    sqlx::query("UPDATE renovate_settings SET next_run_at = now() + interval '1 day' WHERE project_id = $1")
        .bind(project)
        .execute(&srv.state.db)
        .await
        .unwrap();
    commit_file(&work, "README.md", "moved on\n", "docs").await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;
    let window: (bool, bool) = sqlx::query_as(
        "SELECT next_run_at > now() + interval '50 minutes', next_run_at < now() + interval '2 hours' FROM renovate_settings WHERE project_id = $1",
    )
    .bind(project)
    .fetch_one(&srv.state.db)
    .await
    .unwrap();
    assert_eq!(window, (true, true), "an hour after the last run");

    // a person ticks a box in the bot's dashboard: a run is due now (the last
    // run was a while ago)
    sqlx::query("UPDATE ci_pipelines SET created_at = now() - interval '2 hours' WHERE source = 'renovate'")
        .execute(&srv.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE renovate_settings SET next_run_at = now() + interval '1 day' WHERE project_id = $1")
        .bind(project)
        .execute(&srv.state.db)
        .await
        .unwrap();
    let (s, _) = owner
        .put(&format!("/api/v1/projects/{project}/issues/{}", dash["iid"]), json!({ "description": "- [x] <!-- rebase-all -->Rebase all" }))
        .await;
    assert_eq!(s, 200);
    let due: bool = sqlx::query_scalar("SELECT next_run_at <= now() FROM renovate_settings WHERE project_id = $1")
        .bind(project)
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert!(due, "a ticked box brings the run forward");

    // the sweep starts due runs, at most max_concurrent (2) at once: here and
    // in the other project (its first run is due too)
    hold("UPDATE renovate_settings SET next_run_at = now() WHERE project_id = $1").await.unwrap();
    forgust_server::services::renovate::run_due(&srv.state).await.unwrap();
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ci_pipelines WHERE source = 'renovate' AND status NOT IN ('success', 'failed', 'canceled', 'skipped')",
    )
    .fetch_one(&srv.state.db)
    .await
    .unwrap();
    assert_eq!(active, 2);
    let next: bool = sqlx::query_scalar("SELECT next_run_at > now() + interval '23 hours' FROM renovate_settings WHERE project_id = $1")
        .bind(project)
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert!(next, "the next run a day later");

    // runs no runner takes do not hold the slots for good
    sqlx::query("UPDATE ci_pipelines SET created_at = now() - interval '3 days' WHERE source = 'renovate'")
        .execute(&srv.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE renovate_settings SET next_run_at = now() + interval '1 day'").execute(&srv.state.db).await.unwrap();
    forgust_server::services::renovate::run_due(&srv.state).await.unwrap();
    wait_jobs(&srv.state).await;
    assert_eq!(forgust_db::renovate::active_runs(&srv.state.db, None).await.unwrap(), 0, "stale runs canceled");

    // off: the bot leaves; its issue stays
    let (s, st) = owner.put(&rn, json!({ "enabled": false })).await;
    assert_eq!(s, 200, "{st}");
    let members = owner.ok_get(&format!("/api/v1/projects/{project}/members")).await;
    assert!(members.as_array().unwrap().iter().all(|m| m["username"] != bot.as_str()));
    assert_eq!(owner.post(&format!("{rn}/run"), json!({})).await.0, 409);
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{project}/issues/{}", dash["iid"])).await["title"], "Dependency Dashboard");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn renovate_can_be_off_on_the_instance(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.renovate.enabled = false).await;
    let owner = srv.user("olivia", false).await;
    let (_org, _sub, project) = org_project(&owner).await;
    let st = owner.ok_get(&format!("/api/v1/projects/{project}/renovate")).await;
    assert_eq!(st["available"], false);
    assert_eq!(owner.put(&format!("/api/v1/projects/{project}/renovate"), json!({ "enabled": true })).await.0, 409);
}

/// The rest of An administrator's settings over the config file, a
/// project's host rules, Renovate's cache, a group default, the bot's picture.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn renovate_admin_host_rules_and_group_default(pool: sqlx::PgPool) {
    let srv = TestServer::start_with(pool, |c| c.ci.long_poll_secs = 1).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let (org, _sub, project) = org_project(&owner).await;

    // admin: the settings in effect, changed over the config file
    assert_eq!(owner.get("/api/v1/admin/renovate").await.0, 403);
    let st = admin.ok_get("/api/v1/admin/renovate").await;
    assert_eq!(st["image"], "renovate/renovate:44.226.3");
    assert_eq!(st["github_com_token_set"], false);
    let mut body = st.clone();
    body["image"] = json!("renovate/renovate:44.300.0");
    body["tags"] = json!(["deps"]);
    body["onboarding_config"] = json!("not json");
    assert_eq!(admin.put("/api/v1/admin/renovate", body.clone()).await.0, 400);
    body["onboarding_config"] = json!(r#"{"extends":["config:recommended"]}"#);
    body["extra_env"] = json!({ "RENOVATE_PR_HOURLY_LIMIT": "4" });
    body["github_com_token"] = json!("ghp_no_scopes");
    body["image_pull_username"] = json!("puller");
    body["image_pull_password"] = json!("pull-secret");
    let (s, st) = admin.put("/api/v1/admin/renovate", body).await;
    assert_eq!(s, 200, "{st}");
    assert_eq!(st["github_com_token_set"], true);
    assert_eq!((st["image_pull_username"].as_str(), st["image_pull_password_set"].as_bool()), (Some("puller"), Some(true)));
    assert!(st.get("image_pull_password").is_none(), "never returned");
    assert!(st.get("github_com_token").is_none(), "never returned");
    let raw: Option<String> =
        sqlx::query_scalar("SELECT github_com_token_enc FROM renovate_instance").fetch_one(&srv.state.db).await.unwrap();
    assert!(!raw.unwrap().contains("ghp_no_scopes"), "stored encrypted");

    // host rules: write-only passwords
    let hr = format!("/api/v1/projects/{project}/renovate/host_rules");
    assert_eq!(owner.post(&hr, json!({ "match_host": "bad host", "password": "x" })).await.0, 400);
    let rules = owner
        .ok_post(&hr, json!({ "match_host": "npm.example.com", "host_type": "npm", "username": "ci", "password": "npm-secret-token" }))
        .await;
    assert_eq!(rules[0]["match_host"], "npm.example.com");
    assert!(!serde_json::to_string(&rules).unwrap().contains("npm-secret-token"));
    let gone = owner.ok_post(&hr, json!({ "match_host": "old.example.com", "password": "x1" })).await[1]["id"].as_i64().unwrap();
    assert_eq!(owner.delete(&format!("{hr}/{gone}")).await.0, 204);
    // the instance's, for every project: a project's own rule comes after (it wins)
    let ihr = "/api/v1/admin/renovate/host_rules";
    assert_eq!(owner.post(ihr, json!({ "match_host": "x.example.com", "password": "x" })).await.0, 403);
    admin.ok_post(ihr, json!({ "match_host": "npm.example.com", "host_type": "npm", "username": "org", "password": "org-token" })).await;
    let all = admin.ok_post(ihr, json!({ "match_host": "maven.example.com", "password": "mvn-token" })).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert!(owner.ok_get(&hr).await.as_array().unwrap().iter().all(|r| r["match_host"] != "maven.example.com"), "not the project's");
    assert_eq!(owner.delete(&format!("{ihr}/{}", all[1]["id"])).await.0, 403);
    assert_eq!(owner.delete(&format!("{hr}/{}", all[1]["id"])).await.0, 404, "not a project rule");

    // a group default turns it on for the projects that never chose…
    let gr = format!("/api/v1/groups/{org}/renovate");
    let (s, g) = owner.put(&gr, json!({ "enabled": true })).await;
    assert_eq!(s, 200, "{g}");
    assert_eq!(g["turned_on"], 1);
    let rn = owner.ok_get(&format!("/api/v1/projects/{project}/renovate")).await;
    assert_eq!(rn["enabled"], true);
    assert_eq!(rn["image"], "renovate/renovate:44.300.0", "the administrator's image");
    // …and for new projects below it
    let sub2 = owner.ok_post("/api/v1/groups", json!({ "name": "Tools", "path": "tools", "parent_id": org })).await["id"].as_i64().unwrap();
    let newp =
        owner.ok_post("/api/v1/projects", json!({ "name": "CLI", "path": "cli", "namespace_id": sub2 })).await["id"].as_i64().unwrap();
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{newp}/renovate")).await["enabled"], true);
    // the bot has its picture
    let bot = rn["bot_username"].as_str().unwrap().to_string();
    assert_eq!(owner.get_bytes(&format!("/api/v1/users/{bot}/avatar")).await.0, 200);

    // a run: the admin's settings, the host rules, the cache
    let work = srv.work_dir("rn2");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", "acme/platform/api"), "main"]).await);
    wait_jobs(&srv.state).await;
    let runner =
        admin.ok_post("/api/v1/admin/runners", json!({ "description": "deps", "tags": ["deps"], "run_untagged": false })).await["token"]
            .as_str()
            .unwrap()
            .to_string();
    owner.ok_post(&format!("/api/v1/projects/{project}/renovate/run"), json!({})).await;
    let job = request_for(&srv, &runner, project).await.expect("the run");
    assert_eq!(job["image"]["name"], "renovate/renovate:44.300.0");
    assert_eq!(var(&job, "RENOVATE_GITHUB_COM_TOKEN").as_deref(), Some("ghp_no_scopes"));
    assert_eq!(var(&job, "RENOVATE_PR_HOURLY_LIMIT").as_deref(), Some("4"));
    assert_eq!(var(&job, "RENOVATE_ONBOARDING_CONFIG").as_deref(), Some(r#"{"extends":["config:recommended"]}"#));
    let rules: Value = serde_json::from_str(&var(&job, "RENOVATE_HOST_RULES").unwrap()).unwrap();
    assert!(rules.as_array().unwrap().iter().any(|r| r["matchHost"] == "maven.example.com" && r["password"] == "mvn-token"), "{rules}");
    // the project's rule for npm.example.com comes last: Renovate keeps it
    let npm = rules.as_array().unwrap().iter().rfind(|r| r["matchHost"] == "npm.example.com").unwrap_or_else(|| panic!("{rules}"));
    assert_eq!(
        (npm["hostType"].as_str(), npm["username"].as_str(), npm["password"].as_str()),
        (Some("npm"), Some("ci"), Some("npm-secret-token"))
    );
    assert!(rules.as_array().unwrap().iter().all(|r| r["matchHost"] != "old.example.com"));
    let auth: Value = serde_json::from_str(&var(&job, "DOCKER_AUTH_CONFIG").expect("image pull credentials")).unwrap();
    use base64::Engine as _;
    let pair =
        base64::engine::general_purpose::STANDARD.decode(auth["auths"]["https://index.docker.io/v1/"]["auth"].as_str().unwrap()).unwrap();
    assert_eq!(pair, b"puller:pull-secret");
    assert!(masked(&job, "DOCKER_AUTH_CONFIG"));
    assert_eq!(job["cache"][0]["paths"][0], ".renovate-cache/");
    assert_eq!(job["cache"][0]["key"], "renovate-forgust_renovate", "a key no project job can name");
    assert_eq!(var(&job, "RENOVATE_CACHE_DIR").as_deref(), Some("$CI_PROJECT_DIR/.renovate-cache"));
    finish(&srv, &job, "success").await;

    // off on the group: new projects no longer get it; existing ones keep it
    owner.put(&gr, json!({ "enabled": false })).await;
    let later =
        owner.ok_post("/api/v1/projects", json!({ "name": "Later", "path": "later", "namespace_id": sub2 })).await["id"].as_i64().unwrap();
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{later}/renovate")).await["enabled"], false);
    assert_eq!(owner.ok_get(&format!("/api/v1/projects/{project}/renovate")).await["enabled"], true);
}
