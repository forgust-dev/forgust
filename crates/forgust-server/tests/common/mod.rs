//! Test harness: a full Forgust server (all roles) on ephemeral ports against a
//! throwaway database from `#[sqlx::test]`, driven over HTTP, git and SSH.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use forgust_server::app;
use forgust_server::config::Config;
use forgust_server::state::AppState;
use serde_json::{Value, json};

pub struct TestServer {
    pub state: AppState,
    pub base: String,
    pub ssh_port: u16,
    pub tmp: tempfile::TempDir,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl TestServer {
    pub async fn start(pool: sqlx::PgPool) -> TestServer {
        Self::start_with(pool, |_| {}).await
    }

    pub async fn start_with(pool: sqlx::PgPool, tweak: impl FnOnce(&mut Config)) -> TestServer {
        let tmp = tempfile::tempdir().unwrap();
        let mut base = Config::default();
        // empty: the server's own address, set per port below (a tweak may set its own)
        base.server.external_url = String::new();
        base.server.ssh_listen = "127.0.0.1:0".into();
        base.server.data_dir = tmp.path().join("data");
        base.server.static_dir = tmp.path().join("no-web");
        base.server.hooks_exe = Some(PathBuf::from(env!("CARGO_BIN_EXE_forgust")));
        base.telemetry.metrics = false;
        tweak(&mut base);
        forgust_server::jobs::migrate(&pool).await.unwrap();
        // the free port can be taken by a parallel test before we bind it: try another
        for attempt in 1.. {
            let port = free_port();
            let mut cfg = base.clone();
            cfg.server.listen = format!("127.0.0.1:{port}");
            if cfg.server.external_url.is_empty() {
                cfg.server.external_url = format!("http://127.0.0.1:{port}");
            }
            let state = app::build_state(cfg, pool.clone(), None).await.unwrap();
            match app::start(state.clone()).await {
                Ok(running) => {
                    let ssh_port = running.ssh_addr.map(|a| a.port()).unwrap_or(0);
                    return TestServer { base: format!("http://{}", running.addr), state, ssh_port, tmp };
                }
                Err(e) if attempt < 5 && format!("{e:#}").contains("in use") => continue,
                Err(e) => panic!("test server did not start: {e:#}"),
            }
        }
        unreachable!()
    }

    pub fn host(&self) -> String {
        self.base.trim_start_matches("http://").to_string()
    }

    /// Create a user directly and return a signed-in API client.
    pub async fn user(&self, username: &str, admin: bool) -> Client {
        let mut tx = self.state.db.begin().await.unwrap();
        forgust_server::services::users::create(
            &mut tx,
            forgust_server::services::users::CreateUser {
                username,
                email: &format!("{username}@example.com"),
                name: "",
                password: Some(&password(username)),
                is_admin: admin,
                email_verified: true,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let mut c = Client::new(&self.base);
        c.login(username, &password(username)).await;
        c
    }

    /// `http://user:password@host/path.git`
    pub fn http_url(&self, user: &str, path: &str) -> String {
        format!("http://{user}:{}@{}/{path}.git", password(user), self.host())
    }

    pub fn http_url_token(&self, token: &str, path: &str) -> String {
        format!("http://oauth2:{token}@{}/{path}.git", self.host())
    }

    pub fn work_dir(&self, name: &str) -> PathBuf {
        let d = self.tmp.path().join("work").join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

pub fn password(user: &str) -> String {
    format!("{user}-password-123")
}

#[derive(Clone)]
pub struct Client {
    pub base: String,
    pub http: reqwest::Client,
    pub cookie: Option<String>,
    pub csrf: Option<String>,
    pub token: Option<String>,
}

impl Client {
    pub fn new(base: &str) -> Client {
        Client {
            base: base.to_string(),
            http: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap(),
            cookie: None,
            csrf: None,
            token: None,
        }
    }

    pub fn with_token(base: &str, token: &str) -> Client {
        let mut c = Client::new(base);
        c.token = Some(token.into());
        c
    }

    pub async fn login(&mut self, login: &str, password: &str) -> Value {
        let resp = self
            .http
            .post(format!("{}/api/v1/auth/login", self.base))
            .json(&json!({ "login": login, "password": password }))
            .send()
            .await
            .unwrap();
        self.take_session(resp).await
    }

    pub async fn take_session(&mut self, resp: reqwest::Response) -> Value {
        if let Some(c) = resp.headers().get("set-cookie") {
            self.cookie = Some(c.to_str().unwrap().split(';').next().unwrap().to_string());
        }
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "login failed: {status} {v}");
        if let Some(csrf) = v.pointer("/session/csrf_token").and_then(|x| x.as_str()) {
            self.csrf = Some(csrf.into());
        }
        v
    }

    pub fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(c) = &self.cookie {
            r = r.header("cookie", c);
        }
        if let Some(c) = &self.csrf {
            r = r.header("x-csrf-token", c);
        }
        if let Some(t) = &self.token {
            r = r.header("private-token", t);
        }
        r
    }

    /// POST a multipart body with one `file` part.
    pub async fn upload(&self, path: &str, filename: &str, data: &[u8]) -> (u16, Value) {
        let form =
            reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(data.to_vec()).file_name(filename.to_string()));
        let r = self.req(reqwest::Method::POST, path).multipart(form).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn get(&self, path: &str) -> (u16, Value) {
        let r = self.req(reqwest::Method::GET, path).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn get_bytes(&self, path: &str) -> (u16, Vec<u8>) {
        let r = self.req(reqwest::Method::GET, path).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.bytes().await.unwrap().to_vec())
    }

    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.req(reqwest::Method::POST, path).json(&body).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn put(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.req(reqwest::Method::PUT, path).json(&body).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    pub async fn delete(&self, path: &str) -> (u16, Value) {
        let r = self.req(reqwest::Method::DELETE, path).send().await.unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    /// POST that must succeed; returns the JSON body.
    pub async fn ok_post(&self, path: &str, body: Value) -> Value {
        let (s, v) = self.post(path, body.clone()).await;
        assert!((200..300).contains(&s), "POST {path} {body} -> {s} {v}");
        v
    }

    pub async fn ok_put(&self, path: &str, body: Value) -> Value {
        let (s, v) = self.put(path, body.clone()).await;
        assert!((200..300).contains(&s), "PUT {path} {body} -> {s} {v}");
        v
    }

    pub async fn ok_get(&self, path: &str) -> Value {
        let (s, v) = self.get(path).await;
        assert!((200..300).contains(&s), "GET {path} -> {s} {v}");
        v
    }
}

pub struct GitOut {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run git with an isolated environment.
pub async fn git(dir: &Path, args: &[&str]) -> GitOut {
    git_env(dir, args, &[]).await
}

pub async fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> GitOut {
    // async: the server under test shares this (single-threaded) runtime
    let mut c = tokio::process::Command::new("git");
    c.current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("HOME", dir);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().await.unwrap();
    GitOut {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into(),
        stderr: String::from_utf8_lossy(&out.stderr).into(),
    }
}

pub fn must(o: GitOut) -> String {
    assert!(o.ok, "git failed: {}\n{}", o.stdout, o.stderr);
    o.stdout.trim().to_string()
}

/// A local repo with two commits on `main`.
/// Write `file` (creating its folders) and commit it in the work tree `dir`.
pub async fn commit_file(dir: &Path, file: &str, content: &str, msg: &str) {
    if let Some(parent) = Path::new(file).parent() {
        std::fs::create_dir_all(dir.join(parent)).unwrap();
    }
    std::fs::write(dir.join(file), content).unwrap();
    must(git(dir, &["add", file]).await);
    must(git(dir, &["commit", "-q", "-m", msg]).await);
}

pub async fn seed_repo(dir: &Path) {
    must(git(dir, &["init", "-q", "-b", "main"]).await);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("README.md"), "# Demo\n\nSee [the code](src/main.rs).\n").unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {\n    println!(\"hello\");\n}\n").unwrap();
    must(git(dir, &["add", "."]).await);
    must(git(dir, &["commit", "-q", "-m", "initial commit"]).await);
    std::fs::write(dir.join("src/main.rs"), "fn main() {\n    println!(\"hello, world\");\n}\n").unwrap();
    must(git(dir, &["commit", "-q", "-am", "greet the world"]).await);
}

/// Org `acme` → subgroup `platform` → project `api`, created by `owner`.
pub async fn org_project(owner: &Client) -> (i64, i64, i64) {
    let org = owner.ok_post("/api/v1/groups", json!({ "name": "Acme", "path": "acme" })).await;
    let sub = owner.ok_post("/api/v1/groups", json!({ "name": "Platform", "path": "platform", "parent_id": org["id"] })).await;
    let proj = owner.ok_post("/api/v1/projects", json!({ "name": "API", "path": "api", "namespace_id": sub["id"] })).await;
    (org["id"].as_i64().unwrap(), sub["id"].as_i64().unwrap(), proj["id"].as_i64().unwrap())
}

pub fn has(cmd: &str) -> bool {
    Command::new(cmd).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Wait until the post-receive job for the latest push has run.
pub async fn wait_jobs(state: &AppState) {
    // up to 30 s: a full parallel suite slows the job queue down
    for _ in 0..600 {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM graphile_worker._private_jobs WHERE (attempts = 0 AND run_at <= now()) OR locked_at IS NOT NULL",
        )
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
        if n == 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
