use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use forgust_storage::Storage;
use metrics_exporter_prometheus::PrometheusHandle;
use moka::future::Cache;
use sqlx::PgPool;
use tokio::sync::RwLock;

use crate::blob::BlobStore;
use crate::config::Config;
use crate::crypto::SecretBox;
use crate::events::{Event, EventBus};
use crate::jobs::JobQueue;

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

pub struct Inner {
    pub cfg: Config,
    pub db: PgPool,
    pub storage: Arc<dyn Storage>,
    pub blobs: Arc<dyn BlobStore>,
    pub events: Arc<dyn EventBus>,
    pub jobs: Arc<dyn JobQueue>,
    pub cache: ResponseCache,
    pub secrets: SecretBox,
    pub repo_locks: RepoLocks,
    pub hooks: HookConfig,
    pub metrics: Option<PrometheusHandle>,
    pub highlighter: crate::render::Highlighter,
    /// Proxy for Markdown images from other sites (None: `security.image_proxy` off).
    pub images: Option<crate::render::ImageProxy>,
    pub oidc: crate::auth::oidc::OidcClients,
    pub mailer: Arc<dyn crate::services::notify::Mailer>,
    /// One-time token for creating the first admin (None once set up).
    pub setup_token: Mutex<Option<String>>,
    pub login_limiter: RateLimiter,
    /// Failed password checks per account (by id), from any client. Past it,
    /// only addresses the account signed in from keep trying.
    pub login_failures: RateLimiter,
    /// Wrong second-factor codes per account.
    pub totp_failures: RateLimiter,
    /// Failed password checks per client, whatever the account (one password
    /// tried against many names).
    pub login_ip_failures: RateLimiter,
    /// Blame, history and archives asked for by anonymous visitors, per client.
    pub anon_heavy_reads: RateLimiter,
    /// The key CI `id_tokens` are signed with, loaded on first use.
    pub oidc_key: tokio::sync::OnceCell<std::sync::Arc<crate::ci::oidc::SigningKey>>,
}

/// Fixed-window limiter for sign-in attempts, per key (IP + login).
pub struct RateLimiter {
    max: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (u32, std::time::Instant)>>,
}

impl RateLimiter {
    pub fn new(max: u32, window: Duration) -> Self {
        RateLimiter { max, window, hits: Mutex::new(HashMap::new()) }
    }

    pub fn allow(&self, key: &str) -> bool {
        let mut hits = self.hits.lock().unwrap();
        let now = std::time::Instant::now();
        if hits.len() > 100_000 {
            hits.retain(|_, (_, t)| now.duration_since(*t) < self.window);
        }
        let e = hits.entry(key.to_string()).or_insert((0, now));
        if now.duration_since(e.1) >= self.window {
            *e = (0, now);
        }
        e.0 += 1;
        e.0 <= self.max
    }

    /// Over the limit, without counting this call.
    pub fn exceeded(&self, key: &str) -> bool {
        self.hits.lock().unwrap().get(key).is_some_and(|(n, t)| t.elapsed() < self.window && *n >= self.max)
    }

    pub fn forget(&self, key: &str) {
        self.hits.lock().unwrap().remove(key);
    }
}

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppState")
    }
}

/// Where git hooks find the internal API.
#[derive(Clone, Debug)]
pub struct HookConfig {
    pub hooks_dir: PathBuf,
    pub socket: PathBuf,
    pub secret: String,
}

/// Per-repo read/write locks: pushes share, maintenance is exclusive (ADR-0004).
/// In-process, so it covers the single-binary install (ADR-0012).
#[derive(Default)]
pub struct RepoLocks {
    locks: Mutex<HashMap<i64, Arc<RwLock<()>>>>,
}

impl RepoLocks {
    pub fn get(&self, repo: i64) -> Arc<RwLock<()>> {
        let mut m = self.locks.lock().unwrap();
        if m.len() > 10_000 {
            m.retain(|_, l| Arc::strong_count(l) > 1);
        }
        m.entry(repo).or_default().clone()
    }
}

/// In-process cache of rendered JSON, keyed by immutable object IDs (see
/// architecture: Caching). Keys start with `"{project_id}:"` so a whole project
/// can be dropped on delete or restore.
#[derive(Clone)]
pub struct ResponseCache {
    inner: Cache<String, Bytes>,
}

impl ResponseCache {
    pub fn new(max_bytes: u64) -> Self {
        let inner = Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|k: &String, v: &Bytes| (k.len() + v.len()).min(u32::MAX as usize) as u32)
            .time_to_idle(Duration::from_secs(6 * 3600))
            .support_invalidation_closures()
            .build();
        ResponseCache { inner }
    }

    pub fn key(project_id: i64, kind: &str, parts: &[&str]) -> String {
        format!("{project_id}:{kind}:{}", parts.join("\u{1f}"))
    }

    pub async fn get(&self, key: &str) -> Option<Bytes> {
        let v = self.inner.get(key).await;
        metrics::counter!("forgust_cache_requests_total", "result" => if v.is_some() { "hit" } else { "miss" }).increment(1);
        v
    }

    pub async fn put(&self, key: String, value: Bytes) {
        self.inner.insert(key, value).await;
    }

    pub fn drop_project(&self, project_id: i64) {
        let prefix = format!("{project_id}:");
        let _ = self.inner.invalidate_entries_if(move |k, _| k.starts_with(&prefix));
    }

    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    /// Apply pending inserts/evictions (counts are eventually consistent).
    pub async fn sync(&self) {
        self.inner.run_pending_tasks().await;
    }
}

/// Keep caches consistent with repo changes published on the event bus.
pub fn spawn_cache_invalidator(state: AppState) {
    let mut rx = state.events.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Event::ProjectDeleted { project_id }) | Ok(Event::RepoReset { project_id }) => state.cache.drop_project(project_id),
                // results are keyed by object IDs, so a push invalidates nothing
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
}
