//! The MCP server's code mode (ADR-0025): `search_api` and `execute_api` run
//! JavaScript an assistant wrote, in QuickJS on a blocking thread. The script
//! has no files, network, timers or modules, at most `MEMORY` bytes and
//! `TIME`. `search_api` reads the OpenAPI document; `execute_api`'s one way
//! out is `forgust.request`, which calls the `/api/v1` router in-process as
//! the caller: the same handlers and checks as over HTTP, for a token with
//! the `read_api` scope (reads) or the `api` scope (writes too), each call
//! against the token's rate limit.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::rejection::RawPathParamsRejection;
use axum::extract::{ConnectInfo, MatchedPath, RawPathParams, Request};
use axum::http::{Method, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use rquickjs::{Context, Ctx, Function, Promise, Runtime};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::Caller;
use crate::auth::InProcess;
use crate::state::AppState;

const MEMORY: usize = 64 << 20;
/// Far under its thread's own stack (2 MB): `forgust.request` runs a whole API handler on
/// it, from however deep the script is in its recursion.
const STACK: usize = 256 << 10;
const TIME: Duration = Duration::from_secs(30);
/// API calls per run.
const CALLS: usize = 50;
/// One API answer, at most.
const BODY: usize = 4 << 20;
/// A run's result, as JSON, at most: the answer is cut at 64 KB anyway, and a
/// larger one would be copied and parsed for nothing.
const RESULT: usize = 1 << 20;
/// `console.log` output kept.
const LOGS: usize = 16 << 10;
/// What is said of a run that failed (what it threw, its stack), at most.
const ERROR: usize = 16 << 10;
/// Scripts running at once, on the whole server: each holds a thread and up
/// to `MEMORY` for up to `TIME`.
const RUNS: usize = 8;
/// Scripts one user runs at once (all their tokens): one cannot hold all `RUNS`.
const RUNS_PER_USER: usize = 2;

static RUNNING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(RUNS);
/// Scripts running, per user id (no entry: none).
static BY_USER: LazyLock<Mutex<HashMap<i64, usize>>> = LazyLock::new(Mutex::default);

fn by_user() -> MutexGuard<'static, HashMap<i64, usize>> {
    BY_USER.lock().unwrap_or_else(PoisonError::into_inner)
}

fn late() -> String {
    format!("the script ran longer than {} s", TIME.as_secs())
}

fn too_big() -> String {
    format!("the script used more than {} MB", MEMORY >> 20)
}

/// A running script's place: one of `RUNS`, one of its user's `RUNS_PER_USER`.
struct Slot {
    user: i64,
    _permit: tokio::sync::SemaphorePermit<'static>,
}

impl Slot {
    fn take(user: i64) -> Result<Slot, String> {
        let mut by_user = by_user();
        if by_user.get(&user).copied().unwrap_or(0) >= RUNS_PER_USER {
            return Err(format!("you already run {RUNS_PER_USER} scripts: wait for one to end"));
        }
        let Ok(permit) = RUNNING.try_acquire() else {
            return Err(format!("the server already runs {RUNS} scripts: try again in a moment"));
        };
        *by_user.entry(user).or_default() += 1;
        Ok(Slot { user, _permit: permit })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut by_user = by_user();
        if let Some(n) = by_user.get_mut(&self.user) {
            *n -= 1;
            if *n == 0 {
                by_user.remove(&self.user);
            }
        }
    }
}

/// `/api/v1`, as `execute_api` calls it, built once: each run only gives it
/// its state. Not the app's outer layers (metrics, compression, headers):
/// none of them is for an in-process call.
static ROUTER: LazyLock<axum::Router<AppState>> =
    LazyLock::new(|| axum::Router::new().nest("/api/v1", crate::api::router().route_layer(middleware::from_fn(keep_route))));

/// The API's OpenAPI document, as `search_api` gives it to scripts.
static SPEC: LazyLock<String> = LazyLock::new(|| serde_json::to_string(&crate::api::openapi::document()).unwrap_or_default());

/// Set up before the assistant's code: `console`, `forgust` (execute_api
/// only) and `__run`, which takes the code as an async function, an
/// expression, or the body of an async function.
const PRELUDE: &str = r#"
"use strict";
{
  const text = (x) => { try { return typeof x === "string" ? x : String(JSON.stringify(x)); } catch { return String(x); } };
  const log = (...a) => __log(a.map(text).join(" "));
  globalThis.console = { log, info: log, warn: log, error: log, debug: log };
  if (typeof __request === "function") {
    const request = async (method, path, body) => {
      const r = JSON.parse(__request(String(method), String(path), body === undefined ? null : JSON.stringify(body)));
      if (!r.ok) {
        const e = new Error(r.message);
        e.status = r.status;
        e.body = r.body;
        throw e;
      }
      return r.body;
    };
    const verb = (m) => (path, body) => request(m, path, body);
    globalThis.forgust = Object.freeze({
      request, get: verb("GET"), post: verb("POST"), put: verb("PUT"), patch: verb("PATCH"), delete: verb("DELETE"),
    });
  }
  globalThis.__run = async (src) => {
    // only compiled here, never run: a SyntaxError is the code's form, not
    // one it threw (which must not run it a second time)
    let f, expression = true;
    try {
      f = (0, eval)("(async () => (" + src + "\n))");
    } catch (e) {
      if (!(e instanceof SyntaxError)) throw e;
      f = (0, eval)("(async () => {\n" + src + "\n})");
      expression = false;
    }
    let v = await f();
    if (expression && typeof v === "function") v = await v();
    if (v === undefined) return "null";
    const json = JSON.stringify(v);
    if (typeof json !== "string") throw new TypeError("the result has no JSON form (a " + typeof v + "): return data");
    return json;
  };
}
"#;

pub async fn search(c: &Caller, code: String) -> Result<Value, String> {
    sandbox(c, code, false).await
}

pub async fn execute(c: &Caller, code: String) -> Result<Value, String> {
    sandbox(c, code, true).await
}

/// What a request matched, kept on its response for the audit log: the
/// route (never the path: it can name people) and its `{id}`.
#[derive(Clone)]
struct Matched {
    route: MatchedPath,
    id: Option<String>,
}

async fn keep_route(params: Result<RawPathParams, RawPathParamsRejection>, req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>().cloned();
    let id = params.ok().and_then(|p| p.iter().find(|(k, _)| *k == "id").map(|(_, v)| v.to_string()));
    let mut resp = next.run(req).await;
    if let Some(route) = route {
        resp.extensions_mut().insert(Matched { route, id });
    }
    resp
}

/// Runs `code` for the caller on a blocking thread (QuickJS is not async and its runtime
/// stays on one thread), with the API (`forgust`) when `api`; its result as JSON, with what
/// it logged.
async fn sandbox(c: &Caller, code: String, api: bool) -> Result<Value, String> {
    let slot = Slot::take(c.user.id)?;
    let deadline = Instant::now() + TIME;
    let host = api.then(|| Host { router: ROUTER.clone().with_state(c.state.clone()), caller: c.clone(), deadline, calls: Cell::new(0) });
    let task = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        let logs = Rc::new(RefCell::new(Vec::<String>::new()));
        let answer = run_js(&code, host, deadline, &logs);
        let logs = logs.take();
        match answer {
            Ok(result) => Ok(json!({ "result": result, "logs": logs })),
            Err(e) if logs.is_empty() => Err(e),
            Err(e) => Err(format!("{e}\nconsole output:\n{}", logs.join("\n"))),
        }
    });
    task.await.map_err(|e| format!("the script stopped: {e}"))?
}

fn run_js(code: &str, host: Option<Host>, deadline: Instant, logs: &Rc<RefCell<Vec<String>>>) -> Result<Value, String> {
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    rt.set_memory_limit(MEMORY);
    rt.set_max_stack_size(STACK);
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let ctx = Context::full(&rt).map_err(|e| e.to_string())?;
    let out = ctx.with(|ctx| {
        let fail = |e| describe(&ctx, e, deadline);
        let g = ctx.globals();
        let kept = logs.clone();
        let size = Cell::new(0usize);
        let log = Function::new(ctx.clone(), move |mut line: String| {
            let left = LOGS.saturating_sub(size.get());
            if left == 0 {
                return;
            }
            let mut l = kept.borrow_mut();
            if line.len() >= left {
                line.truncate(line.floor_char_boundary(left));
                l.push(line);
                l.push(format!("[console output cut at {} KB]", LOGS >> 10));
                size.set(LOGS);
                return;
            }
            size.set(size.get() + line.len());
            l.push(line);
        })
        .map_err(fail)?;
        g.set("__log", log).map_err(fail)?;
        if let Some(host) = host {
            let request =
                Function::new(ctx.clone(), move |method: String, path: String, body: Option<String>| host.request(&method, &path, body))
                    .map_err(fail)?;
            g.set("__request", request).map_err(fail)?;
        } else {
            let spec = ctx.json_parse(SPEC.as_str()).map_err(fail)?;
            g.set("spec", spec).map_err(fail)?;
        }
        ctx.eval::<(), _>(PRELUDE).map_err(fail)?;
        let run: Function = g.get("__run").map_err(fail)?;
        let promise: Promise = run.call((code,)).map_err(fail)?;
        promise.finish::<String>().map_err(fail)
    });
    // a script can throw a value as large as its memory
    let out = out.map_err(|mut e| {
        if e.len() > ERROR {
            e.truncate(e.floor_char_boundary(ERROR));
            e.push_str(&format!("\n[error cut at {} KB]", ERROR >> 10));
        }
        e
    })?;
    if out.len() > RESULT {
        return Err(format!("the result is larger than {} MB of JSON: return less (a count, the fields you need)", RESULT >> 20));
    }
    serde_json::from_str(&out).map_err(|e| format!("the result cannot be read ({e}): JSON at most 128 levels deep"))
}

/// What went wrong in a run, for the assistant.
fn describe(ctx: &Ctx<'_>, e: rquickjs::Error, deadline: Instant) -> String {
    if Instant::now() > deadline {
        return late();
    }
    match e {
        rquickjs::Error::Exception => thrown(ctx),
        rquickjs::Error::Allocation => too_big(),
        rquickjs::Error::WouldBlock => "the script waits for something that never comes (there are no timers or events)".into(),
        e => e.to_string(),
    }
}

/// What the script threw: an error's name, message and stack, or the value.
fn thrown(ctx: &Ctx<'_>) -> String {
    let thrown = ctx.catch();
    let Some(x) = thrown.as_exception() else {
        return match ctx.json_stringify(thrown) {
            Ok(Some(s)) => format!("the script threw {}", s.to_string().unwrap_or_default()),
            _ => "the script threw a value that is not JSON".into(),
        };
    };
    let msg = x.message().unwrap_or_default();
    if msg.contains("out of memory") {
        return too_big();
    }
    let name = x.get::<_, Option<String>>("name").ok().flatten().unwrap_or_else(|| "Error".into());
    let stack = x.stack().unwrap_or_default();
    format!("{name}: {msg}\n{stack}").trim_end().to_string()
}

/// What `forgust.request` reaches: the API as the caller.
struct Host {
    router: axum::Router,
    caller: Caller,
    deadline: Instant,
    calls: Cell<usize>,
}

impl Host {
    /// One API call, as JSON for the prelude: `{ ok, status, body, message }`.
    fn request(&self, method: &str, path: &str, body: Option<String>) -> String {
        let answer = match self.call(method, path, body) {
            Ok((status, body)) if status < 400 => json!({ "ok": true, "status": status, "body": body }),
            Ok((status, body)) => {
                json!({ "ok": false, "status": status, "message": super::tools::api_message(status, &body), "body": body })
            }
            Err((status, message)) => json!({ "ok": false, "status": status, "message": message }),
        };
        answer.to_string()
    }

    fn call(&self, method: &str, path: &str, body: Option<String>) -> Result<(u16, Value), (u16, String)> {
        let method = match method.to_ascii_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            "DELETE" => Method::DELETE,
            m => return Err((405, format!("method {m}: GET, POST, PUT, PATCH or DELETE"))),
        };
        // the paths of search_api's spec, or the same without /api/v1
        let path = path.trim();
        if !path.starts_with('/') || path.starts_with("//") {
            return Err((400, format!("{path}: a path of the API, as /api/v1/projects/...")));
        }
        let path = match path.strip_prefix("/api/v1") {
            Some(rest) if rest.is_empty() || rest.starts_with(['/', '?']) => path.to_string(),
            _ => format!("/api/v1{path}"),
        };
        let n = self.calls.get() + 1;
        self.calls.set(n);
        if n > CALLS {
            return Err((429, format!("at most {CALLS} API calls a run")));
        }
        let c = &self.caller;
        if !super::limiter().allow(&c.rate_key) {
            return Err((429, format!("rate limited: at most {} calls a minute per token", super::RATE)));
        }
        let mut req = Request::builder()
            .method(method.clone())
            .uri(&path)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map_or_else(Body::empty, Body::from))
            .map_err(|e| (400, format!("{path}: {e}")))?;
        req.extensions_mut().insert(InProcess(c.principal.clone()));
        if let Some(ip) = c.ip {
            req.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip, 0)));
        }
        let change = method != Method::GET;
        // a project named by its path, found before the change: a rename, a transfer or a
        // removal would hide it from its own audit row
        let named = if change { block_on(self.project_named(&path)) } else { None };
        let sent = self.router.clone().oneshot(req);
        let resp = if change {
            // a change once begun is not cut short at the deadline: half of it would stay,
            // with no audit row
            if Instant::now() > self.deadline {
                return Err((504, late()));
            }
            block_on(sent)
        } else {
            self.within(sent)?
        }
        .unwrap_or_else(|e| match e {});
        let status = resp.status();
        // a change is audited once it is made, whatever becomes of its answer
        if status.is_success()
            && change
            && let Some(m) = resp.extensions().get::<Matched>()
        {
            let project = project_of(m, named);
            let details = json!({
                "tool": "execute_api", "via": "mcp", "token_id": c.token_id, "method": method.as_str(), "route": m.route.as_str(),
            });
            let ip = c.ip.map(|ip| ip.to_string());
            block_on(crate::services::audit(
                &c.state,
                &c.principal,
                "mcp_tool_call",
                project.map(|id| ("project", id)),
                details,
                ip.as_deref(),
            ));
        }
        // a stream (server-sent events) never ends: refused, not waited for
        if resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/event-stream")) {
            return Err((400, "a stream of events: execute_api reads whole answers only".to_string()));
        }
        let bytes = self.within(axum::body::to_bytes(resp.into_body(), BODY))?.map_err(|e| {
            match e.into_inner().downcast::<http_body_util::LengthLimitError>() {
                Ok(_) => (413, format!("the answer is larger than {} MB: ask for less (a page, a path)", BODY >> 20)),
                Err(e) => (502, format!("the answer could not be read: {e}")),
            }
        })?;
        let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| match std::str::from_utf8(&bytes) {
            Ok(s) => Value::String(s.to_string()),
            Err(_) => Value::String(format!("({} bytes of binary data)", bytes.len())),
        });
        Ok((status.as_u16(), body))
    }

    /// `f`, if it ends before the run's deadline.
    fn within<F: Future>(&self, f: F) -> Result<F::Output, (u16, String)> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        block_on(tokio::time::timeout(left, f)).map_err(|_| (504, late()))
    }

    /// The project a call to `/api/v1/projects/<its path>/...` names, when not by its number.
    async fn project_named(&self, path: &str) -> Option<i64> {
        let id = path.strip_prefix("/api/v1/projects/")?.split(['/', '?']).next()?;
        if id.is_empty() || id.parse::<i64>().is_ok() {
            return None;
        }
        let id = percent_encoding::percent_decode_str(id).decode_utf8().ok()?;
        crate::api::load_project(&self.caller.state, &id).await.ok().map(|p| p.id)
    }
}

/// The project a `/projects/{id}/...` call changed, for its audit row (the project's log
/// shows it): its number, or the one its path `named` before the call.
fn project_of(m: &Matched, named: Option<i64>) -> Option<i64> {
    if !m.route.as_str().starts_with("/api/v1/projects/{id}") {
        return None;
    }
    m.id.as_deref()?.parse().ok().or(named)
}

/// Runs `f` to its end from the script's thread (a blocking one, in the runtime).
fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Handle::current().block_on(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A script with no API (search_api's sandbox).
    fn js(code: &str, time: Duration) -> Result<Value, String> {
        let logs = Rc::new(RefCell::new(Vec::new()));
        run_js(code, None, Instant::now() + time, &logs).map(|v| json!({ "result": v, "logs": logs.take() }))
    }

    fn ok(code: &str) -> Value {
        js(code, TIME).unwrap_or_else(|e| panic!("{code}: {e}"))["result"].clone()
    }

    #[test]
    fn takes_a_function_an_expression_or_a_body() {
        assert_eq!(ok("async () => 1 + 1"), json!(2));
        assert_eq!(ok("() => ({ a: [1, 'x'] })"), json!({ "a": [1, "x"] }));
        assert_eq!(ok("40 + 2"), json!(42));
        assert_eq!(ok("const x = await Promise.resolve(3);\nreturn x * 2;"), json!(6));
        assert_eq!(ok("let y = 1"), Value::Null, "a body that returns nothing");
        assert_eq!(ok("spec.openapi"), json!("3.1.0"));
        assert!(ok("Object.keys(spec.paths).filter((p) => p.endsWith('/issues'))").as_array().is_some_and(|l| !l.is_empty()));
    }

    #[test]
    fn runs_the_code_once() {
        // a SyntaxError thrown while running is not a parse error
        let logs = Rc::new(RefCell::new(Vec::new()));
        let e = run_js("[console.log('ran'), JSON.parse('{')]", None, Instant::now() + TIME, &logs).unwrap_err();
        assert!(e.starts_with("SyntaxError"), "{e}");
        assert_eq!(logs.take(), ["ran"]);
        assert_eq!(ok("await Promise.resolve(5)"), json!(5), "an expression may await");
    }

    #[test]
    fn one_user_cannot_hold_every_run() {
        let user = -1;
        let held: Vec<Slot> = (0..RUNS_PER_USER).map(|_| Slot::take(user).unwrap()).collect();
        assert!(Slot::take(user).is_err_and(|e| e.contains("you already run")));
        drop(Slot::take(-2).expect("another user still runs one"));
        drop(held);
        drop(Slot::take(user).expect("free again"));
        assert!(!BY_USER.lock().unwrap().contains_key(&user), "no entry left behind");
    }

    #[test]
    fn keeps_console_output() {
        let v = js("console.log('a', { b: 1 }); console.error(2); return 0;", TIME).unwrap();
        assert_eq!(v["logs"], json!(["a {\"b\":1}", "2"]));
        let v = js("console.log('x'.repeat(1 << 20)); console.log('more'); return 0;", TIME).unwrap();
        let logs = v["logs"].as_array().unwrap();
        assert_eq!(logs.len(), 2, "cut, then nothing more");
        assert_eq!(logs[0].as_str().unwrap().len(), LOGS);
    }

    #[test]
    fn errors_say_what_happened() {
        let e = js("throw new Error('boom')", TIME).unwrap_err();
        assert!(e.starts_with("Error: boom"), "{e}");
        let e = js("throw { code: 7 }", TIME).unwrap_err();
        assert!(e.contains(r#"{"code":7}"#), "{e}");
        let e = js("return new Promise(() => {})", TIME).unwrap_err();
        assert!(e.contains("never comes"), "{e}");
        let e = js("return Symbol()", TIME).unwrap_err();
        assert!(e.contains("no JSON form (a symbol)"), "{e}");
        let e = js("let v = 1; for (let i = 0; i < 200; i++) v = [v]; return v;", TIME).unwrap_err();
        assert!(e.contains("128 levels"), "{e}");
        let e = js("return (", TIME).unwrap_err();
        assert!(e.contains("SyntaxError"), "{e}");
    }

    #[test]
    fn has_no_way_out() {
        assert_eq!(
            ok("[typeof fetch, typeof require, typeof process, typeof setTimeout, typeof std, typeof os, typeof forgust]"),
            json!(["undefined", "undefined", "undefined", "undefined", "undefined", "undefined", "undefined"])
        );
        let e = js("return import('os')", TIME).unwrap_err();
        assert!(!e.is_empty());
    }

    #[test]
    fn stops_at_its_limits() {
        let start = Instant::now();
        let e = js("while (true) {}", Duration::from_millis(200)).unwrap_err();
        assert!(e.contains("ran longer"), "{e}");
        assert!(start.elapsed() < Duration::from_secs(5));
        let e = js("const a = []; while (true) a.push('x'.repeat(1 << 20) + a.length);", TIME).unwrap_err();
        assert!(e.contains("MB"), "{e}");
        let e = js("return 'x'.repeat(2 << 20)", TIME).unwrap_err();
        assert!(e.contains("larger than 1 MB"), "{e}");
        let e = js("throw 'x'.repeat(8 << 20)", TIME).unwrap_err();
        assert!(e.len() < ERROR + 64 && e.ends_with("KB]"), "{}", e.len());
        let e = js("const f = (n) => f(n + 1); f(0)", TIME).unwrap_err();
        assert!(e.contains("stack") || e.contains("RangeError") || e.contains("InternalError"), "{e}");
    }
}
