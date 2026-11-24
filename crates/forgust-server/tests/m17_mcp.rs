//! The built-in MCP server (ADR-0025). An MCP client's view: tokens
//! and scopes, the tools acting with the token user's permissions, writes
//! audited, and the OAuth sign-in (registration, consent, PKCE, refresh).

mod common;

use base64::Engine;
use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const REPO: &str = "acme/platform/api";

async fn token(c: &Client, name: &str, scopes: &[&str]) -> String {
    c.ok_post("/api/v1/user/tokens", json!({ "name": name, "scopes": scopes })).await["token"].as_str().unwrap().to_string()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn tools_act_as_the_token_user(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let guest = srv.user("gus", false).await;
    let stranger = srv.user("sam", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    let work = srv.work_dir("mcp");
    seed_repo(&work).await;
    must(git(&work, &["push", "-q", &srv.http_url("olivia", REPO), "main"]).await);
    wait_jobs(&srv.state).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "gus", "access_level": "guest" })).await;

    // no token: 401 pointing at the OAuth metadata
    let r = Mcp::new(&srv, None).raw(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })).await;
    assert_eq!(r.status(), 401);
    let www = r.headers()["www-authenticate"].to_str().unwrap().to_string();
    assert!(www.contains("/.well-known/oauth-protected-resource/api/v4/mcp"), "{www}");
    let meta = owner.ok_get("/.well-known/oauth-protected-resource/api/v4/mcp").await;
    assert_eq!(meta["resource"], format!("{}/api/v4/mcp", srv.state.cfg.server.external_url));

    let read = Mcp::new(&srv, Some(&token(&owner, "read", &["read_api"]).await));
    let write = Mcp::new(&srv, Some(&token(&owner, "write", &["api"]).await));

    // the handshake; notifications get 202
    let init = read
        .rpc("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } }))
        .await;
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "Forgust");
    assert!(init["instructions"].as_str().unwrap().contains("@olivia"));
    assert_eq!(read.raw(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.status(), 202);
    let old = read.rpc("initialize", json!({ "protocolVersion": "2024-11-05" })).await;
    assert_eq!(old["protocolVersion"], "2025-06-18", "an unknown version gets ours");

    // read tokens see read tools only
    let names = read.tools().await;
    assert!(names.contains(&"get_merge_request".to_string()) && !names.contains(&"create_issue".to_string()), "{names:?}");
    assert!(write.tools().await.contains(&"add_commit".to_string()));
    let (err, text) = read.call("create_issue", json!({ "project_id": p, "title": "x" })).await;
    assert!(err && text.contains("read-only"), "{text}");

    // code, marked as untrusted data
    let text = read.ok("get_repository_file", json!({ "project_id": REPO, "file_path": "src/main.rs" })).await;
    assert!(text.contains("hello, world") && text.contains("untrusted"), "{text}");
    let text = read.ok("get_repository_file", json!({ "project_id": p, "file_path": "src/main.rs", "max_lines": 1 })).await;
    assert!(text.contains("\"offset_line\": 2") && !text.contains("println"), "paged: {text}");
    let text = read.ok("get_project", json!({ "url": format!("{}/{REPO}", srv.state.cfg.server.external_url) })).await;
    assert!(text.contains("# Demo"), "the README: {text}");
    let text = read.ok("list_repository_tree", json!({ "project_id": p, "recursive": true })).await;
    assert!(text.contains("src/main.rs"), "{text}");
    // code search: the matching lines with their numbers; a Guest of the
    // private project finds nothing in it
    let text = read.ok("search_code", json!({ "query": "HELLO, world" })).await;
    assert!(text.contains(r#"2:     println!(\"hello, world\");"#) && text.contains("src/main.rs") && text.contains("untrusted"), "{text}");
    assert!(text.contains(&format!("/{REPO}/-/blob/main/src/main.rs")), "{text}");
    let text = read.ok("search_code", json!({ "query": r"print\w+!", "regex": true, "project_id": REPO, "language": "rust" })).await;
    assert!(text.contains("\"matching_lines\": 1"), "{text}");
    let (err, text) = read.call("search_code", json!({ "query": "he" })).await;
    assert!(err && text.contains("at least 3"), "{text}");
    let as_guest = Mcp::new(&srv, Some(&token(&guest, "g", &["read_api"]).await));
    let text = as_guest.ok("search_code", json!({ "query": "hello, world" })).await;
    assert!(text.contains("\"files\": []"), "{text}");
    let (err, _) = as_guest.call("search_code", json!({ "query": "hello, world", "project_id": REPO })).await;
    assert!(err);
    let text = read.ok("list_commits", json!({ "project_id": p })).await;
    assert!(text.contains("greet the world"), "{text}");
    let head: Value = serde_json::from_str(text.split("<forgust-data>").nth(1).unwrap().split("</forgust-data>").next().unwrap()).unwrap();
    let sha = head[0]["id"].as_str().unwrap();
    let text = read.ok("get_commit", json!({ "project_id": p, "sha": sha, "include": ["diff"] })).await;
    assert!(text.contains(r#"+    println!(\"hello, world\");"#), "{text}");
    let text = read.ok("get_blame", json!({ "project_id": p, "file_path": "src/main.rs", "start_line": 2, "end_line": 2 })).await;
    assert!(text.contains("greet the world"), "{text}");

    // writes: issues and comments, audited with the token
    let text = write
        .ok("create_issue", json!({ "project_id": p, "title": "Crash on start", "description": "steps", "assignee_usernames": ["olivia"] }))
        .await;
    assert!(text.contains("Done, as @olivia"), "{text}");
    write.ok("save_note", json!({ "project_id": p, "issue_iid": 1, "body": "looking" })).await;
    write.ok("update_issue", json!({ "project_id": p, "issue_iid": 1, "title": "Crash on start-up" })).await;
    let text = read.ok("get_issue", json!({ "project_id": p, "issue_iid": 1, "include": ["notes"] })).await;
    assert!(text.contains("Crash on start-up") && text.contains("looking"), "{text}");
    let text = read.ok("list_issues", json!({ "project_id": p })).await;
    assert!(text.contains("Crash on start-up"), "{text}");
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'mcp_tool_call' AND details->>'via' = 'mcp' AND details->>'token_id' IS NOT NULL",
    )
    .fetch_one(&srv.state.db)
    .await
    .unwrap();
    assert_eq!(audited, 3);

    // a merge request: a branch and a commit, opened, read with its diff, commented on a line
    write.ok("add_branch", json!({ "project_id": p, "branch": "fix", "ref": "main" })).await;
    write
        .ok(
            "add_commit",
            json!({ "project_id": p, "branch": "fix", "commit_message": "say bye", "actions": [{ "action": "update", "file_path": "src/main.rs", "content": "fn main() {\n    println!(\"bye\");\n}\n" }] }),
        )
        .await;
    write.ok("save_merge_request", json!({ "project_id": p, "source_branch": "fix", "title": "Say bye" })).await;
    let text = read.ok("get_merge_request", json!({ "project_id": p, "merge_request_iid": 1, "include": ["diffs", "commits"] })).await;
    assert!(text.contains(r#"+    println!(\"bye\");"#) && text.contains("say bye"), "{text}");
    assert!(text.contains(&format!("/{REPO}/-/merge_requests/1")), "{text}");
    write
        .ok("save_note", json!({ "project_id": p, "merge_request_iid": 1, "body": "why?", "file_path": "src/main.rs", "new_line": 2 }))
        .await;
    let text = read.ok("get_merge_request", json!({ "project_id": p, "merge_request_iid": 1, "include": ["discussions"] })).await;
    assert!(text.contains("why?") && text.contains("\"new_line\": 2"), "{text}");
    let r = read.rpc("resources/read", json!({ "uri": format!("forgust://projects/{p}/merge_requests/1/diff") })).await;
    assert!(r["contents"][0]["text"].as_str().unwrap().contains("+++ b/src/main.rs"), "{r}");

    // a Guest reads issues, not code; a stranger sees nothing
    let g = Mcp::new(&srv, Some(&token(&guest, "g", &["mcp"]).await));
    assert!(!g.tools().await.contains(&"create_issue".to_string()), "mcp alone is read-only");
    g.ok("list_issues", json!({ "project_id": p })).await;
    let (err, _) = g.call("get_repository_file", json!({ "project_id": p, "file_path": "README.md" })).await;
    assert!(err, "guests cannot read code");
    let s = Mcp::new(&srv, Some(&token(&stranger, "s", &["api"]).await));
    let (err, text) = s.call("get_project", json!({ "project_id": p })).await;
    assert!(err, "{text}");
    let (err, _) = s.call("create_issue", json!({ "project_id": p, "title": "spam" })).await;
    assert!(err);

    // batches: at most 10 messages, each counted
    let batch = |n: usize| Value::Array((0..n).map(|i| json!({ "jsonrpc": "2.0", "id": i, "method": "ping" })).collect());
    let r: Value = read.raw(batch(11)).await.json().await.unwrap();
    assert_eq!(r["error"]["code"], -32600, "{r}");
    let r: Value = read.raw(batch(3)).await.json().await.unwrap();
    assert_eq!(r.as_array().unwrap().len(), 3);

    // people see which tokens used MCP
    let list = owner.ok_get("/api/v1/user/tokens").await;
    let used = |name: &str| list.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["mcp_used_at"].is_string();
    assert!(used("read") && used("write"));

    // administrators can turn it off
    let mut settings = admin.ok_get("/api/v1/admin/settings").await;
    assert_eq!(settings["mcp_enabled"], true);
    settings["mcp_enabled"] = json!(false);
    admin.ok_put("/api/v1/admin/settings", settings).await;
    assert_eq!(read.raw(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await.status(), 403);
}

fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn query(url: &str) -> std::collections::HashMap<String, String> {
    url::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn oauth_sign_in(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let http = reqwest::Client::new();
    let base = &srv.base;

    let meta = owner.ok_get("/.well-known/oauth-authorization-server").await;
    assert_eq!(meta["code_challenge_methods_supported"], json!(["S256"]));

    // registration: public clients with safe redirect addresses
    let reg = |body: Value| http.post(format!("{base}/oauth/register")).json(&body).send();
    assert_eq!(reg(json!({ "redirect_uris": ["http://evil.example/cb"] })).await.unwrap().status(), 400);
    // anyone may register: what is kept stays small
    let long = format!("http://127.0.0.1/{}", "a".repeat(2000));
    assert_eq!(reg(json!({ "redirect_uris": [long] })).await.unwrap().status(), 400, "a redirect address too long");
    let big = json!({ "redirect_uris": ["http://127.0.0.1/cb"], "client_name": "x".repeat(20_000) });
    assert_eq!(reg(big).await.unwrap().status(), 413, "a request too large");
    let r = reg(json!({ "client_name": "Test client", "redirect_uris": ["http://127.0.0.1/cb"] })).await.unwrap();
    assert_eq!(r.status(), 201);
    let client: Value = r.json().await.unwrap();
    let client_id = client["client_id"].as_str().unwrap();

    // the consent page's questions, then the person's answer
    let verifier = "a".repeat(20) + &"b".repeat(30);
    let auth = json!({ "client_id": client_id, "redirect_uri": "http://127.0.0.1:4567/cb", "response_type": "code",
                       "code_challenge": challenge(&verifier), "code_challenge_method": "S256", "scope": "mcp api" });
    let q: Vec<(String, String)> = auth.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
    let info: Value = owner.req(reqwest::Method::GET, "/api/v1/oauth/authorize").query(&q).send().await.unwrap().json().await.unwrap();
    assert_eq!((info["client_name"].as_str(), info["write_requested"].as_bool()), (Some("Test client"), Some(true)), "{info}");
    assert_eq!(owner.post("/api/v1/oauth/authorize", json!({ "allow": true, "state": "s", "client_id": client_id, "redirect_uri": "https://elsewhere.example/cb", "response_type": "code", "code_challenge": challenge(&verifier), "code_challenge_method": "S256" })).await.0, 400);
    let mut deny = auth.clone();
    deny["allow"] = json!(false);
    deny["state"] = json!("xyz");
    let r = owner.ok_post("/api/v1/oauth/authorize", deny).await;
    let back = query(r["redirect"].as_str().unwrap());
    assert_eq!((back["error"].as_str(), back["state"].as_str()), ("access_denied", "xyz"));
    let mut allow = auth.clone();
    allow["allow"] = json!(true);
    allow["write"] = json!(false);
    allow["state"] = json!("xyz");
    let r = owner.ok_post("/api/v1/oauth/authorize", allow).await;
    let redirect = r["redirect"].as_str().unwrap();
    assert!(redirect.starts_with("http://127.0.0.1:4567/cb?"), "{redirect}");
    let code = query(redirect)["code"].clone();

    // the token endpoint: PKCE checked, codes work once
    let token = |form: Vec<(&str, String)>| http.post(format!("{base}/oauth/token")).form(&form).send();
    let exchange = |v: &str| {
        vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.clone()),
            ("client_id", client_id.to_string()),
            ("redirect_uri", "http://127.0.0.1:4567/cb".to_string()),
            ("code_verifier", v.to_string()),
        ]
    };
    let r = token(exchange(&"c".repeat(50))).await.unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(r.json::<Value>().await.unwrap()["error"], "invalid_grant");
    // (a failed try used the code up: get another)
    let mut allow = auth.clone();
    allow["allow"] = json!(true);
    let code = query(owner.ok_post("/api/v1/oauth/authorize", allow).await["redirect"].as_str().unwrap())["code"].clone();
    let exchange = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.clone()),
        ("client_id", client_id.to_string()),
        ("redirect_uri", "http://127.0.0.1:4567/cb".to_string()),
        ("code_verifier", verifier.clone()),
    ];
    let r = token(exchange.clone()).await.unwrap();
    assert_eq!(r.status(), 200);
    let t: Value = r.json().await.unwrap();
    assert_eq!((t["token_type"].as_str(), t["scope"].as_str(), t["expires_in"].as_i64()), (Some("Bearer"), Some("mcp"), Some(3600)), "{t}");
    assert_eq!(token(exchange).await.unwrap().status(), 400, "a code works once");

    // the access token: read-only, as the person consented
    let access = t["access_token"].as_str().unwrap();
    let m = Mcp::new(&srv, Some(access));
    assert!(m.tools().await.contains(&"list_projects".to_string()));
    assert!(!m.tools().await.contains(&"create_issue".to_string()));

    // refresh: a new secret for the same token, the old one stops working
    let refresh = |rt: &str| {
        vec![("grant_type", "refresh_token".to_string()), ("refresh_token", rt.to_string()), ("client_id", client_id.to_string())]
    };
    let t2: Value = token(refresh(t["refresh_token"].as_str().unwrap())).await.unwrap().json().await.unwrap();
    let access2 = t2["access_token"].as_str().unwrap();
    assert_eq!(Mcp::new(&srv, Some(access)).raw(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await.status(), 401);
    Mcp::new(&srv, Some(access2)).rpc("ping", json!({})).await;
    assert_eq!(token(refresh(t["refresh_token"].as_str().unwrap())).await.unwrap().status(), 400, "refresh tokens work once");
    let list = owner.ok_get("/api/v1/user/tokens").await;
    let mine: Vec<&Value> = list.as_array().unwrap().iter().filter(|x| x["name"] == "MCP: Test client").collect();
    assert_eq!(mine.len(), 1, "one entry per sign-in: {list}");

    // revoking it in the token list ends the client's access, refresh included
    assert_eq!(owner.delete(&format!("/api/v1/user/tokens/{}", mine[0]["id"])).await.0, 200);
    let r = token(refresh(t2["refresh_token"].as_str().unwrap())).await.unwrap();
    assert_eq!(r.status(), 400);

    // allowing changes: an MCP-only write scope, never `api` (REST refuses it)
    let mut allow = auth.clone();
    allow["allow"] = json!(true);
    allow["write"] = json!(true);
    let code = query(owner.ok_post("/api/v1/oauth/authorize", allow).await["redirect"].as_str().unwrap())["code"].clone();
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("client_id", client_id.to_string()),
        ("redirect_uri", "http://127.0.0.1:4567/cb".to_string()),
        ("code_verifier", verifier.clone()),
    ];
    let w: Value = token(form.clone()).await.unwrap().json().await.unwrap();
    assert_eq!(w["scope"], "mcp mcp_write", "{w}");
    let wt = w["access_token"].as_str().unwrap();
    assert!(Mcp::new(&srv, Some(wt)).tools().await.contains(&"create_issue".to_string()));
    let rest = Client::with_token(base, wt);
    assert_eq!(rest.get("/api/v1/user").await.0, 403, "REST refuses MCP tokens");
    assert_eq!(rest.post("/api/v1/projects", json!({ "name": "x" })).await.0, 403);

    // a refresh token used twice: someone has a copy, so the grant ends
    let rt = w["refresh_token"].as_str().unwrap().to_string();
    let w2: Value = token(refresh(&rt)).await.unwrap().json().await.unwrap();
    assert_eq!(token(refresh(&rt)).await.unwrap().status(), 400);
    assert_eq!(
        Mcp::new(&srv, Some(w2["access_token"].as_str().unwrap()))
            .raw(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
            .await
            .status(),
        401,
        "the reuse revoked the live token"
    );
    assert_eq!(token(refresh(w2["refresh_token"].as_str().unwrap())).await.unwrap().status(), 400, "and its refresh");
    form.clear();
}

/// Anyone may register an OAuth client: the ones nobody ever let in are
/// forgotten after a day; one that was stays, its tokens run out or not.
#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn unused_oauth_clients_are_forgotten(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let owner = srv.user("olivia", false).await;
    let uid = owner.ok_get("/api/v1/user").await["id"].as_i64().unwrap();
    let db = &srv.state.db;
    let uris = ["http://127.0.0.1/cb".to_string()];
    for id in ["mcp-idle", "mcp-asking", "mcp-granted", "mcp-new"] {
        forgust_db::oauth::create_client(db, id, id, &uris).await.unwrap();
    }
    let later = chrono::Utc::now() + chrono::Duration::minutes(10);
    forgust_db::oauth::create_code(db, b"hash", "mcp-asking", uid, &uris[0], "challenge", &["mcp".to_string()], later).await.unwrap();
    forgust_db::oauth::granted(db, "mcp-granted").await.unwrap();
    sqlx::query("UPDATE oauth_clients SET created_at = now() - interval '40 days' WHERE client_id <> 'mcp-new'").execute(db).await.unwrap();
    assert_eq!(forgust_db::oauth::forget_unused(db).await.unwrap(), 1);
    let kept = |id: &'static str| async move { forgust_db::oauth::find_client(db, id).await.unwrap().is_some() };
    assert!(!kept("mcp-idle").await);
    assert!(kept("mcp-asking").await, "a consent is under way");
    assert!(kept("mcp-granted").await, "someone let it in: it may come back for consent with the same id");
    assert!(kept("mcp-new").await, "just registered");
}

#[sqlx::test(migrator = "forgust_db::MIGRATOR")]
async fn typed_answers_and_code_mode(pool: sqlx::PgPool) {
    let srv = TestServer::start(pool).await;
    let admin = srv.user("root", true).await;
    let owner = srv.user("olivia", false).await;
    let guest = srv.user("gus", false).await;
    let (_org, _sub, p) = org_project(&owner).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/members"), json!({ "username": "gus", "access_level": "guest" })).await;
    owner.ok_post(&format!("/api/v1/projects/{p}/issues"), json!({ "title": "First" })).await;
    let read = Mcp::new(&srv, Some(&token(&owner, "read", &["read_api"]).await));
    let write = Mcp::new(&srv, Some(&token(&owner, "write", &["api"]).await));
    let call = |m: &Mcp, name: &str, args: Value| {
        let (m, name) = (Mcp { base: m.base.clone(), token: m.token.clone(), http: m.http.clone() }, name.to_string());
        async move { m.rpc("tools/call", json!({ "name": name, "arguments": args })).await }
    };

    // typed answers: an output schema per tool, structuredContent next to the text
    let list = read.rpc("tools/list", json!({})).await;
    let schema = |name: &str| list["tools"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["outputSchema"].clone();
    assert_eq!(schema("list_issues")["properties"]["items"]["type"], "array");
    assert_eq!(schema("get_issue")["type"], "object");
    let r = call(&read, "list_issues", json!({ "project_id": p })).await;
    assert_eq!(r["structuredContent"]["items"][0]["title"], "First", "{r}");
    let r = call(&read, "get_mcp_server_version", json!({})).await;
    assert_eq!(r["structuredContent"]["user"], "olivia", "{r}");
    let r = call(&write, "create_issue", json!({ "project_id": p, "title": "Second" })).await;
    assert_eq!(r["structuredContent"]["iid"], 2, "{r}");

    // code mode: off until an administrator turns it on
    assert!(!read.tools().await.contains(&"execute_api".to_string()));
    let (err, text) = read.call("execute_api", json!({ "code": "1" })).await;
    assert!(err && text.contains("code mode is off"), "{text}");
    let mut settings = admin.ok_get("/api/v1/admin/settings").await;
    assert_eq!(settings["mcp_code_mode"], false);
    settings["mcp_code_mode"] = json!(true);
    admin.ok_put("/api/v1/admin/settings", settings).await;
    let names = read.tools().await;
    assert!(names.contains(&"search_api".to_string()) && names.contains(&"get_issue".to_string()), "{names:?}");
    // `?tools=code`: those two only
    let only: Value = reqwest::Client::new()
        .post(format!("{}/api/v4/mcp?tools=code", srv.base))
        .bearer_auth(read.token.as_deref().unwrap())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let only: Vec<&str> = only["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(only, ["search_api", "execute_api"]);

    // search_api: scripts over the OpenAPI document
    let r = call(&read, "search_api", json!({ "code": "Object.keys(spec.paths).filter((p) => p.endsWith('/issues'))" })).await;
    assert!(r["structuredContent"]["result"].as_array().unwrap().iter().any(|x| x == "/api/v1/projects/{id}/issues"), "{r}");

    // an `mcp` token (what OAuth clients get) has no execute_api: /api/v1 needs read_api or api
    let mcp_only = Mcp::new(&srv, Some(&token(&owner, "mcp", &["mcp"]).await));
    let names = mcp_only.tools().await;
    assert!(names.contains(&"search_api".to_string()) && !names.contains(&"execute_api".to_string()), "{names:?}");
    let (err, text) = mcp_only.call("execute_api", json!({ "code": format!("forgust.get('/projects/{p}/variables')") })).await;
    assert!(err && text.contains("read_api"), "{text}");

    // execute_api: many calls, one answer, as the token's user (a `read_api` token reads)
    let code = format!(
        "const r = await forgust.get('/api/v1/projects/{p}/issues?state=all');\nconst list = r.issues ?? r.items ?? r;\nconsole.log(list.length);\n\
         const me = await forgust.get('/user');\nreturn {{ me: me.username, titles: list.map((i) => i.title).sort() }};"
    );
    let r = call(&read, "execute_api", json!({ "code": code })).await;
    assert_eq!(r["structuredContent"]["result"], json!({ "me": "olivia", "titles": ["First", "Second"] }), "{r}");
    assert_eq!(r["structuredContent"]["logs"], json!(["2"]));
    assert!(r["content"][0]["text"].as_str().unwrap().contains("untrusted"), "{r}");

    // ...but does not write; a failed call throws with its status
    let code =
        format!("try {{ await forgust.post('/projects/{p}/issues', {{ title: 'x' }}); }} catch (e) {{ return [e.status, e.message]; }}");
    let r = call(&read, "execute_api", json!({ "code": code })).await;
    assert_eq!(r["structuredContent"]["result"][0], 403, "{r}");
    let (err, text) = read.call("execute_api", json!({ "code": "forgust.get('/projects/999999')" })).await;
    assert!(err && text.contains("Error"), "{text}");
    let (err, text) = read.call("execute_api", json!({ "code": "forgust.get('https://example.com/')" })).await;
    assert!(err && text.contains("a path of the API"), "{text}");
    // a stream of events fails at once, not at the end of the run
    let start = std::time::Instant::now();
    let code = format!("try {{ await forgust.get('/events?projects={p}'); }} catch (e) {{ return [e.status, e.message]; }}");
    let r = call(&read, "execute_api", json!({ "code": code })).await;
    assert_eq!(r["structuredContent"]["result"][0], 400, "{r}");
    assert!(start.elapsed() < std::time::Duration::from_secs(10), "waited for the stream");

    // the same checks as the API: a Guest reads no code
    let g = Mcp::new(&srv, Some(&token(&guest, "g", &["read_api"]).await));
    let (err, text) = g.call("execute_api", json!({ "code": format!("forgust.get('/projects/{p}/repository/commits')") })).await;
    assert!(err, "{text}");
    // nor the admin API without admin_mode
    let a = Mcp::new(&srv, Some(&token(&admin, "a", &["api"]).await));
    let (err, text) = a.call("execute_api", json!({ "code": "forgust.get('/admin/settings')" })).await;
    assert!(err && text.contains("admin_mode"), "{text}");

    // `api` tokens change things, each change audited by route, in the project's log
    let code = "const i = await forgust.post('/api/v1/projects/acme%2Fplatform%2Fapi/issues', { title: 'From a script' });\nreturn i.iid;";
    let r = call(&write, "execute_api", json!({ "code": code })).await;
    assert_eq!(r["structuredContent"]["result"], 3, "{r}");
    let (details, target_type, target_id): (Value, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT details, target_type, target_id FROM audit_events WHERE action = 'mcp_tool_call' AND details->>'tool' = 'execute_api'",
    )
    .fetch_one(&srv.state.db)
    .await
    .unwrap();
    assert_eq!((target_type.as_deref(), target_id), (Some("project"), Some(p)));
    assert_eq!(details["method"], "POST");
    assert_eq!(details["route"], "/projects/{id}/issues", "{details}");
    assert!(details["token_id"].is_number(), "{details}");

    // at most 50 calls a run
    let code = "for (let i = 0; i < 60; i++) { try { await forgust.get('/user'); } catch (e) { return [i, e.status]; } }";
    let r = call(&write, "execute_api", json!({ "code": code })).await;
    assert_eq!(r["structuredContent"]["result"], json!([50, 429]), "{r}");
}
