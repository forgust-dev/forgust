//! HTTP for importers: JSON over a pinned connection to the source, `Link`
//! pagination, and patience with rate limits.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use url::Url;

use super::model::{Result, SourceError};

/// Longest single wait for a rate limit to reset (GitHub and gitlab.com reset
/// hourly); beyond that the job is retried later.
const MAX_WAIT: Duration = Duration::from_secs(65 * 60);
const PER_PAGE: usize = 100;
/// Safety stop for runaway pagination.
const MAX_PAGES: usize = 2000;

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    api: Url,
    pub addrs: Vec<SocketAddr>,
    /// When a rate-limit wait ends (epoch seconds), 0 when not waiting.
    pub waiting: Arc<tokio::sync::watch::Sender<i64>>,
}

/// The source's URL after the SSRF check: http(s), no credentials, and the
/// addresses it resolved to (pinned for the API client and for git).
pub async fn checked_url(raw: &str, allow_local: bool) -> Result<(Url, Vec<SocketAddr>)> {
    let u = Url::parse(raw.trim()).map_err(|_| SourceError::Refused("invalid URL".into()))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() || !u.username().is_empty() || u.password().is_some() {
        return Err(SourceError::Refused("the URL must be http(s), without credentials".into()));
    }
    let addrs = crate::services::webhooks::safe_addrs(&u, allow_local).await.map_err(SourceError::Refused)?;
    Ok((u, addrs))
}

impl Http {
    /// `api` is the API root (`https://gitlab.example.com/api/v4/`); `auth` the
    /// headers that carry the token.
    pub fn new(api: Url, addrs: Vec<SocketAddr>, auth: HeaderMap) -> Result<Http> {
        let host = api.host_str().unwrap_or_default().to_string();
        let mut headers = auth;
        headers.insert(reqwest::header::USER_AGENT, HeaderValue::from_static("Forgust-importer"));
        headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("application/json"));
        let client = reqwest::Client::builder()
            .resolve_to_addrs(&host, &addrs)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .default_headers(headers)
            .build()
            .map_err(|e| SourceError::Other(e.to_string()))?;
        Ok(Http { client, api, addrs, waiting: Arc::new(tokio::sync::watch::Sender::new(0)) })
    }

    pub fn header(name: &'static str, value: &str) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        let mut v = HeaderValue::from_str(value).map_err(|_| SourceError::Refused("the token has invalid characters".into()))?;
        v.set_sensitive(true);
        h.insert(HeaderName::from_static(name), v);
        Ok(h)
    }

    pub fn url(&self, path: &str) -> Result<Url> {
        self.api.join(path.trim_start_matches('/')).map_err(|e| SourceError::Other(e.to_string()))
    }

    /// One request, waiting out rate limits (429, or 403 with an exhausted quota).
    async fn send(&self, method: Method, url: Url) -> Result<reqwest::Response> {
        self.send_with(method, url, true).await
    }

    /// Like [`Http::get`], but a rate limit is an error at once instead of a
    /// wait: for lookups the import can do without.
    pub async fn get_no_wait<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        let mut url = self.url(path)?;
        url.query_pairs_mut().extend_pairs(query);
        let resp = self.send_with(Method::GET, url.clone(), false).await?;
        resp.json::<T>().await.map_err(|e| SourceError::Other(format!("{}: unexpected answer: {e}", url.path())))
    }

    async fn send_with(&self, method: Method, url: Url, wait_out_limits: bool) -> Result<reqwest::Response> {
        let mut tries = 0;
        loop {
            tries += 1;
            let resp = match self.client.request(method.clone(), url.clone()).send().await {
                Ok(r) => r,
                Err(e) if tries < 4 && (e.is_timeout() || e.is_connect()) => {
                    tokio::time::sleep(Duration::from_secs(2 * tries)).await;
                    continue;
                }
                Err(e) => return Err(SourceError::Transient(format!("{}: {e}", url.path()))),
            };
            let status = resp.status();
            // GitHub: 403 once the quota is spent, or with retry-after (secondary limits)
            let limited = status == StatusCode::TOO_MANY_REQUESTS
                || (status == StatusCode::FORBIDDEN
                    && (resp.headers().get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()) == Some("0")
                        || resp.headers().contains_key("retry-after")));
            if limited && !wait_out_limits {
                return Err(SourceError::Transient(format!("rate limited by the source on {}", url.path())));
            }
            if limited {
                let wait = wait_for(resp.headers());
                let which =
                    resp.headers().get("ratelimit-name").and_then(|v| v.to_str().ok()).map(|n| format!(", {n}")).unwrap_or_default();
                if tries >= 4 || wait > MAX_WAIT {
                    return Err(SourceError::Transient(format!(
                        "rate limited by the source on {}{which} (try again in {}s)",
                        url.path(),
                        wait.as_secs()
                    )));
                }
                tracing::info!(path = url.path(), wait_secs = wait.as_secs(), "import: rate limited{which}, waiting");
                let until = chrono::Utc::now().timestamp() + wait.as_secs() as i64;
                self.waiting.send_if_modified(|v| (until > *v).then(|| *v = until).is_some());
                tokio::time::sleep(wait).await;
                self.waiting.send_if_modified(|v| (*v == until).then(|| *v = 0).is_some());
                continue;
            }
            if status.is_server_error() && tries < 4 {
                tokio::time::sleep(Duration::from_secs(3 * tries)).await;
                continue;
            }
            return match status {
                s if s.is_success() => Ok(resp),
                StatusCode::UNAUTHORIZED => Err(SourceError::Refused("the source wants a token it accepts (401)".into())),
                StatusCode::FORBIDDEN => Err(SourceError::Refused(format!("the token may not read {} (403)", url.path()))),
                StatusCode::NOT_FOUND => Err(SourceError::NotFound(url.path().to_string())),
                s if s.is_redirection() => Err(SourceError::Refused(format!(
                    "the source redirected to {} (use the address it redirects to)",
                    resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("another URL")
                ))),
                s if s.is_server_error() => Err(SourceError::Transient(format!("{}: {s}", url.path()))),
                s => Err(SourceError::Other(format!("{}: {s}", url.path()))),
            };
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        let mut url = self.url(path)?;
        url.query_pairs_mut().extend_pairs(query);
        let resp = self.send(Method::GET, url.clone()).await?;
        resp.json::<T>().await.map_err(|e| SourceError::Other(format!("{}: unexpected answer: {e}", url.path())))
    }

    /// Raw body (files), None when not found.
    pub async fn get_text(&self, path: &str, query: &[(&str, &str)]) -> Result<Option<String>> {
        let mut url = self.url(path)?;
        url.query_pairs_mut().extend_pairs(query);
        match self.send(Method::GET, url).await {
            Ok(r) => Ok(Some(r.text().await.map_err(|e| SourceError::Transient(e.to_string()))?)),
            Err(SourceError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Every page of a list, following `Link: <…>; rel="next"`.
    pub async fn all<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<Vec<T>> {
        let mut url = self.url(path)?;
        let per = PER_PAGE.to_string();
        url.query_pairs_mut().extend_pairs(query).append_pair("per_page", &per);
        let mut out = Vec::new();
        for _ in 0..MAX_PAGES {
            let resp = self.send(Method::GET, url.clone()).await?;
            let next = next_link(resp.headers());
            let page: Vec<T> = resp.json().await.map_err(|e| SourceError::Other(format!("{}: unexpected answer: {e}", url.path())))?;
            out.extend(page);
            match next {
                // only follow links on the same host (pinned client)
                Some(n) if n.host_str() == self.api.host_str() => url = n,
                _ => return Ok(out),
            }
        }
        Ok(out)
    }

    /// How many items a list has, from one small page: `X-Total` (GitLab) or the
    /// `last` page of a one-per-page list (GitHub).
    pub async fn count(&self, path: &str, query: &[(&str, &str)]) -> Result<Option<i64>> {
        let mut url = self.url(path)?;
        url.query_pairs_mut().extend_pairs(query).append_pair("per_page", "1");
        let resp = match self.send(Method::GET, url).await {
            Ok(r) => r,
            Err(SourceError::NotFound(_) | SourceError::Refused(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        if let Some(t) = resp.headers().get("x-total").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()) {
            return Ok(Some(t));
        }
        if let Some(last) = link(resp.headers(), "last") {
            return Ok(last.query_pairs().find(|(k, _)| k == "page").and_then(|(_, v)| v.parse().ok()));
        }
        let items: Vec<serde_json::Value> = resp.json().await.unwrap_or_default();
        Ok(Some(items.len() as i64))
    }
}

fn wait_for(h: &HeaderMap) -> Duration {
    let num = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if let Some(s) = num("retry-after") {
        return Duration::from_secs(s.max(1));
    }
    // GitHub and GitLab: epoch seconds when the quota resets
    if let Some(reset) = num("x-ratelimit-reset").or(num("ratelimit-reset")) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        return Duration::from_secs(reset.saturating_sub(now).max(1) + 1);
    }
    Duration::from_secs(60)
}

fn link(h: &HeaderMap, rel: &str) -> Option<Url> {
    let raw = h.get("link")?.to_str().ok()?;
    raw.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let is_rel = params.split(';').any(|p| {
            let p = p.trim();
            p == format!("rel=\"{rel}\"") || p == format!("rel={rel}")
        });
        if !is_rel {
            return None;
        }
        Url::parse(target.trim().trim_start_matches('<').trim_end_matches('>')).ok()
    })
}

fn next_link(h: &HeaderMap) -> Option<Url> {
    link(h, "next")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_link_headers() {
        let mut h = HeaderMap::new();
        h.insert(
            "link",
            HeaderValue::from_static(r#"<https://api.github.com/repos/a/b/issues?page=2>; rel="next", <https://api.github.com/repos/a/b/issues?page=34>; rel="last""#),
        );
        assert_eq!(next_link(&h).unwrap().as_str(), "https://api.github.com/repos/a/b/issues?page=2");
        assert_eq!(link(&h, "last").unwrap().query_pairs().find(|(k, _)| k == "page").unwrap().1, "34");
        assert!(link(&h, "prev").is_none());
    }

    #[test]
    fn rate_limit_waits() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", HeaderValue::from_static("7"));
        assert_eq!(wait_for(&h), Duration::from_secs(7));
        assert_eq!(wait_for(&HeaderMap::new()), Duration::from_secs(60));
    }

    /// GitHub's spent quota (403, remaining 0) is waited out, and the wait is
    /// published while it lasts.
    #[tokio::test]
    async fn waits_out_a_spent_quota() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let app = axum::Router::new().fallback(move || {
            let seen = seen.clone();
            async move {
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    (StatusCode::FORBIDDEN, [("x-ratelimit-remaining", "0"), ("retry-after", "1")], "{}")
                } else {
                    (StatusCode::OK, [("x-ratelimit-remaining", "59"), ("content-type", "application/json")], "[1, 2]")
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = Http::new(Url::parse(&format!("http://{addr}/api/")).unwrap(), vec![addr], HeaderMap::new()).unwrap();
        let mut waiting = http.waiting.subscribe();
        let watcher = tokio::spawn(async move {
            let mut seen = Vec::new();
            while waiting.changed().await.is_ok() {
                seen.push(*waiting.borrow_and_update());
                if seen.len() == 2 {
                    break;
                }
            }
            seen
        });
        let got: Vec<i64> = http.get("items", &[]).await.unwrap();
        assert_eq!(got, vec![1, 2]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let seen = watcher.await.unwrap();
        assert!(seen[0] > chrono::Utc::now().timestamp() - 5, "{seen:?}");
        assert_eq!(seen[1], 0, "cleared once the wait is over");
    }
}
