//! The MCP server's code mode (ADR-0025): `search_api` and `execute_api` run
//! JavaScript an assistant wrote, in QuickJS on a blocking thread. The script
//! has no files, network, timers or modules, at most `MEMORY` bytes and
//! `TIME`. `search_api` reads the OpenAPI document; `execute_api`'s one way
//! out is `forgust.request`, which calls the `/api/v1` router in-process as
//! the caller: the same handlers and checks as over HTTP, writes only for a
//! token with the `api` scope, each call against the token's rate limit.

use std::cell::{Cell, RefCell};
use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, MatchedPath, Request};
use axum::http::{Method, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use rquickjs::{Context, Function, Promise, Runtime};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::Caller;
use crate::auth::{InProcess, Principal};
use crate::state::AppState;

const MEMORY: usize = 64 << 20;
const STACK: usize = 1 << 20;
const TIME: Duration = Duration::from_secs(30);
/// API calls per run.
const CALLS: usize = 50;
/// One API answer, at most.
const BODY: usize = 4 << 20;
/// `console.log` output kept.
const LOGS: usize = 16 << 10;
/// Scripts running at once, on the whole server: each holds a thread and up
/// to `MEMORY` for up to `TIME`.
const RUNS: usize = 8;

static RUNNING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(RUNS);

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
    let f;
    try {
      f = (0, eval)("(" + src + "\n)");
    } catch (e) {
      if (!(e instanceof SyntaxError)) throw e;
      f = (0, eval)("(async () => {\n" + src + "\n})");
    }
    const v = await (typeof f === "function" ? f() : f);
    return v === undefined ? "null" : JSON.stringify(v);
  };
}
"#;

pub async fn search(code: String) -> Result<Value, String> {
    sandbox(code, None).await
}

pub async fn execute(c: &Caller, code: String) -> Result<Value, String> {
    let host = Host {
        router: crate::api::router().route_layer(middleware::from_fn(keep_route)).with_state(c.state.clone()),
        state: c.state.clone(),
        principal: c.principal.clone(),
        write: c.write,
        api: c.api,
        ip: c.ip,
        token_id: c.token_id,
        rate_key: c.rate_key.clone(),
        handle: tokio::runtime::Handle::current(),
        deadline: Instant::now() + TIME,
        calls: Cell::new(0),
    };
    sandbox(code, Some(host)).await
}

/// The route a request matched, on its response: for the audit log, which
/// names routes, never paths (they can name people).
async fn keep_route(req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>().cloned();
    let mut resp = next.run(req).await;
    if let Some(r) = route {
        resp.extensions_mut().insert(r);
    }
    resp
}

/// Runs `code` on a blocking thread (QuickJS is not async and its runtime
/// stays on one thread); its result as JSON, with what it logged.
async fn sandbox(code: String, host: Option<Host>) -> Result<Value, String> {
    let Ok(permit) = RUNNING.try_acquire() else {
        return Err(format!("the server already runs {RUNS} scripts: try again in a moment"));
    };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let logs = Rc::new(RefCell::new(Vec::<String>::new()));
        let deadline = host.as_ref().map_or_else(|| Instant::now() + TIME, |h| h.deadline);
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
        let fail = |e: rquickjs::Error| -> String {
            if Instant::now() > deadline {
                return format!("the script ran longer than {} s", TIME.as_secs());
            }
            match e {
                rquickjs::Error::Exception => {
                    let thrown = ctx.catch();
                    match thrown.as_exception() {
                        Some(x) => {
                            let msg = x.message().unwrap_or_default();
                            if msg.contains("out of memory") {
                                return format!("the script used more than {} MB", MEMORY >> 20);
                            }
                            let name = x.get::<_, Option<String>>("name").ok().flatten().unwrap_or_else(|| "Error".into());
                            let stack = x.stack().unwrap_or_default();
                            format!("{name}: {msg}\n{stack}").trim_end().to_string()
                        }
                        None => match ctx.json_stringify(thrown) {
                            Ok(Some(s)) => format!("the script threw {}", s.to_string().unwrap_or_default()),
                            _ => "the script threw a value that is not JSON".into(),
                        },
                    }
                }
                rquickjs::Error::Allocation => format!("the script used more than {} MB", MEMORY >> 20),
                rquickjs::Error::WouldBlock => "the script waits for something that never comes (there are no timers or events)".into(),
                e => e.to_string(),
            }
        };
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
                let mut end = left;
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                line.truncate(end);
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
    })?;
    serde_json::from_str(&out).map_err(|e| format!("the result is not JSON: {e}"))
}

/// What `forgust.request` reaches: the API as the caller.
struct Host {
    router: axum::Router,
    state: AppState,
    principal: Principal,
    write: bool,
    api: bool,
    ip: Option<IpAddr>,
    token_id: Option<i64>,
    rate_key: String,
    handle: tokio::runtime::Handle,
    deadline: Instant,
    calls: Cell<usize>,
}

impl Host {
    /// One API call, as JSON for the prelude: `{ ok, status, body, message }`.
    fn request(&self, method: &str, path: &str, body: Option<String>) -> String {
        let answer = match self.call(method, path, body) {
            Ok((status, body)) if status < 400 => json!({ "ok": true, "status": status, "body": body }),
            Ok((status, body)) => json!({ "ok": false, "status": status, "message": message(status, &body), "body": body }),
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
        let path = match path.strip_prefix("/api/v1") {
            Some(rest) if rest.is_empty() || rest.starts_with(['/', '?']) => rest,
            _ => path,
        };
        if !path.starts_with('/') || path.starts_with("//") {
            return Err((400, format!("{path}: a path of the API, as /api/v1/projects/...")));
        }
        if method != Method::GET && !self.api {
            let why = if self.write {
                "this token changes things through the write tools only: execute_api writes need a token with the api scope"
            } else {
                "this token is read-only"
            };
            return Err((403, why.into()));
        }
        let n = self.calls.get() + 1;
        self.calls.set(n);
        if n > CALLS {
            return Err((429, format!("at most {CALLS} API calls a run")));
        }
        if !super::limiter().allow(&self.rate_key) {
            return Err((429, format!("rate limited: at most {} calls a minute per token", super::RATE)));
        }
        let mut req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map_or_else(Body::empty, Body::from))
            .map_err(|e| (400, format!("{path}: {e}")))?;
        req.extensions_mut().insert(InProcess(self.principal.clone()));
        if let Some(ip) = self.ip {
            req.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip, 0)));
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        let call = async {
            let resp = self.router.clone().oneshot(req).await.map_err(|e| (500, e.to_string()))?;
            let status = resp.status();
            // a stream (server-sent events) never ends: refused, not waited for
            if resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/event-stream")) {
                return Err((400, "a stream of events: execute_api reads whole answers only".to_string()));
            }
            let route = resp.extensions().get::<MatchedPath>().map_or("(no route)", MatchedPath::as_str).to_string();
            let bytes = axum::body::to_bytes(resp.into_body(), BODY)
                .await
                .map_err(|_| (413, format!("the answer is larger than {} MB: ask for less (a page, a path)", BODY >> 20)))?;
            if status.is_success() && method != Method::GET {
                crate::services::audit(
                    &self.state,
                    &self.principal,
                    "mcp_tool_call",
                    None,
                    json!({ "tool": "execute_api", "via": "mcp", "token_id": self.token_id, "method": method.as_str(), "route": route }),
                    self.ip.map(|ip| ip.to_string()).as_deref(),
                )
                .await;
            }
            let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| match std::str::from_utf8(&bytes) {
                Ok(s) => Value::String(s.to_string()),
                Err(_) => Value::String(format!("({} bytes of binary data)", bytes.len())),
            });
            Ok((status.as_u16(), body))
        };
        self.handle
            .block_on(tokio::time::timeout(left, call))
            .unwrap_or_else(|_| Err((504, format!("the script ran longer than {} s", TIME.as_secs()))))
    }
}

/// A failed call's message, as the API's errors give it.
fn message(status: u16, body: &Value) -> String {
    body["message"]
        .as_str()
        .or(body["error_description"].as_str())
        .or(body["error"].as_str())
        .or(body.as_str().filter(|s| !s.is_empty() && s.len() < 500))
        .map(str::to_string)
        .unwrap_or_else(|| format!("HTTP {status}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A script with no API (search_api's sandbox, without the spec when `bare`).
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
        let e = js("const f = (n) => f(n + 1); f(0)", TIME).unwrap_err();
        assert!(e.contains("stack") || e.contains("RangeError") || e.contains("InternalError"), "{e}");
    }
}
