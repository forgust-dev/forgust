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
    pub smart_view_maps: SmartViewMaps,
    pub secrets: SecretBox,
    pub repo_locks: RepoLocks,
    pub hooks: HookConfig,
    pub metrics: Option<PrometheusHandle>,
    pub highlighter: crate::render::Highlighter,
    /// The code search index (None: `code_search.enabled` is off).
    pub code_index: Option<Arc<crate::code_search::CodeIndex>>,
    /// Proxy for Markdown images from other sites (None: `security.image_proxy` off).
    pub images: Option<crate::render::ImageProxy>,
    pub oidc: crate::auth::oidc::OidcClients,
    /// Security keys and passkeys (None: `server.external_url` is not a domain
    /// over https, or localhost).
    pub webauthn: Option<Arc<crate::auth::webauthn::RelyingParty>>,
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
    /// Audit events that could come in floods (refusals, repository reads):
    /// one row per key and minute.
    pub audit_throttle: AuditThrottle,
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

    /// Give back a hit `allow` counted (the attempt it guarded succeeded).
    pub fn undo(&self, key: &str) {
        if let Some((n, _)) = self.hits.lock().unwrap().get_mut(key) {
            *n = n.saturating_sub(1);
        }
    }

    pub fn forget(&self, key: &str) {
        self.hits.lock().unwrap().remove(key);
    }
}

/// An audit row held by [`AuditThrottle`]: the first of its window, kept to
/// write the count of those held back once the window ends.
#[derive(Debug, Clone)]
pub struct HeldEvent {
    pub principal_kind: &'static str,
    pub principal_id: Option<i64>,
    pub action: String,
    pub target: Option<(String, i64)>,
    pub details: serde_json::Value,
    pub ip: Option<String>,
}

/// At most one audit row per key and window: the first event is written, the
/// ones after it in the window are counted, and the count is written when the
/// window ends ([`AuditThrottle::overdue`]) or with the next row for the key.
pub struct AuditThrottle {
    window: Duration,
    seen: Mutex<HashMap<String, Window>>,
}

struct Window {
    started: std::time::Instant,
    held_back: u32,
    first: HeldEvent,
}

impl AuditThrottle {
    pub fn new(window: Duration) -> Self {
        AuditThrottle { window, seen: Mutex::new(HashMap::new()) }
    }

    /// `Some(repeats held back and not yet written)` when the event is to be
    /// written now, `None` when it is only counted. `event` is called only
    /// when a window starts.
    pub fn admit(&self, key: String, event: impl FnOnce() -> HeldEvent) -> Option<u32> {
        let mut seen = self.seen.lock().unwrap();
        let now = std::time::Instant::now();
        // a flood of distinct keys must not grow the map without bound: past
        // this many, old windows go with whatever they held back
        if seen.len() > 100_000 {
            seen.retain(|_, w| now.duration_since(w.started) < self.window);
        }
        match seen.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let w = o.get_mut();
                if now.duration_since(w.started) < self.window {
                    w.held_back += 1;
                    return None;
                }
                Some(std::mem::replace(w, Window { started: now, held_back: 0, first: event() }).held_back)
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(Window { started: now, held_back: 0, first: event() });
                Some(0)
            }
        }
    }

    /// Windows that ended with events held back, with their count, for a row
    /// saying so: they start again from now (a burst still going stays at a
    /// row a minute). Windows that ended with nothing held back go.
    pub fn overdue(&self) -> Vec<(HeldEvent, u32)> {
        let now = std::time::Instant::now();
        let mut seen = self.seen.lock().unwrap();
        seen.retain(|_, w| w.held_back > 0 || now.duration_since(w.started) < self.window);
        seen.values_mut()
            .filter(|w| now.duration_since(w.started) >= self.window)
            .map(|w| {
                w.started = now;
                (w.first.clone(), std::mem::take(&mut w.held_back))
            })
            .collect()
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

/// The Smart View's maps as shown, by the id of the map kept: what the server puts in when
/// one is read (whose account a committer is, who reviewed each block's changes) moves
/// without a push, so each is kept a quarter of an hour. Readers at once wait for one build.
pub type SmartViewMaps = Cache<i64, Arc<crate::api::smart_view::ShownMap>>;

pub fn smart_view_maps() -> SmartViewMaps {
    Cache::builder().max_capacity(256).time_to_live(Duration::from_secs(900)).build()
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn a_hit_given_back_is_not_counted() {
        let l = super::RateLimiter::new(2, Duration::from_secs(60));
        assert!(l.allow("k"));
        l.undo("k");
        assert!(l.allow("k") && l.allow("k"));
        assert!(!l.allow("k"), "three counted hits is one too many");
        l.undo("nobody");
    }

    use super::AuditThrottle;

    fn event() -> super::HeldEvent {
        super::HeldEvent {
            principal_kind: "user",
            principal_id: Some(1),
            action: "x".into(),
            target: None,
            details: serde_json::json!({}),
            ip: None,
        }
    }

    #[test]
    fn the_throttle_counts_what_it_holds_back() {
        let t = AuditThrottle::new(Duration::from_millis(100));
        assert_eq!(t.admit("a".into(), event), Some(0));
        assert_eq!(t.admit("a".into(), event), None);
        assert_eq!(t.admit("a".into(), event), None);
        assert_eq!(t.admit("b".into(), event), Some(0), "keys are apart");
        assert!(t.overdue().is_empty(), "windows still open");
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(t.admit("a".into(), event), Some(2), "the next row says how many were held back");
        assert_eq!(t.admit("a".into(), event), None);
    }

    #[test]
    fn a_burst_that_stops_is_still_counted() {
        let t = AuditThrottle::new(Duration::from_millis(50));
        t.admit("a".into(), event);
        t.admit("a".into(), event);
        t.admit("b".into(), event);
        std::thread::sleep(Duration::from_millis(70));
        let overdue = t.overdue();
        assert_eq!(overdue.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [1], "b held nothing back");
        assert!(t.overdue().is_empty(), "written once");
        assert_eq!(t.admit("a".into(), event), None, "a burst still going: counted in the window that started again");
        std::thread::sleep(Duration::from_millis(70));
        assert_eq!(t.overdue().iter().map(|(_, n)| *n).collect::<Vec<_>>(), [1]);
        std::thread::sleep(Duration::from_millis(70));
        assert!(t.overdue().is_empty());
        assert_eq!(t.admit("a".into(), event), Some(0), "quiet for a window: a fresh one");
    }
}
