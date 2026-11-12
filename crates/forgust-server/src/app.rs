//! Wiring: build the state, the router and run the roles (ADR-0012).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use forgust_storage::{LocalStorage, LocalStorageConfig};
use sqlx::postgres::PgPoolOptions;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::auth::oidc::OidcClients;
use crate::blob::ObjectStoreBlobs;
use crate::config::Config;
use crate::crypto::SecretBox;
use crate::events::InProcessBus;
use crate::jobs::GraphileQueue;
use crate::state::{AppState, HookConfig, Inner, RateLimiter, RepoLocks, ResponseCache};

pub async fn connect(cfg: &Config) -> anyhow::Result<sqlx::PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(cfg.database.max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&cfg.database.url)
        .await?)
}

pub async fn migrate(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    // advisory lock: only one instance migrates at a time
    forgust_db::MIGRATOR.run(pool).await?;
    crate::jobs::migrate(pool).await?;
    Ok(())
}

/// The secret key from config, or `<data_dir>/secret.key` (created on first start).
fn secret_box(cfg: &Config) -> anyhow::Result<SecretBox> {
    if !cfg.security.secret_key.is_empty() {
        return SecretBox::from_base64(&cfg.security.secret_key);
    }
    let path = cfg.server.data_dir.join("secret.key");
    if !path.exists() {
        std::fs::create_dir_all(&cfg.server.data_dir)?;
        std::fs::write(&path, SecretBox::generate_key())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        tracing::warn!(path = %path.display(), "generated security.secret_key; back this file up with the database");
    }
    SecretBox::from_base64(&std::fs::read_to_string(&path)?)
}

pub fn storage_for(cfg: &Config) -> Arc<LocalStorage> {
    let mut sc = LocalStorageConfig::new(cfg.repos_dir());
    sc.git_bin = cfg.storage.git_bin.clone();
    sc.catfile_pool_size = cfg.storage.catfile_pool_size;
    sc.max_processes = cfg.storage.max_git_processes;
    Arc::new(LocalStorage::new(sc))
}

pub async fn build_state(
    cfg: Config,
    pool: sqlx::PgPool,
    metrics: Option<metrics_exporter_prometheus::PrometheusHandle>,
) -> anyhow::Result<AppState> {
    std::fs::create_dir_all(&cfg.server.data_dir)?;
    std::fs::create_dir_all(cfg.repos_dir())?;
    let run_dir = cfg.server.data_dir.join("run");
    std::fs::create_dir_all(&run_dir)?;
    let hooks_dir = cfg.server.data_dir.join("hooks");
    crate::hooks::install_hooks(&hooks_dir, cfg.server.hooks_exe.as_deref())?;
    check_git_version(&cfg.storage.git_bin)?;
    let blobs = ObjectStoreBlobs::from_config(&cfg)?;
    let secrets = secret_box(&cfg)?;
    let images = cfg.security.image_proxy.then(|| crate::render::ImageProxy::new(secrets.clone(), &cfg.server.external_url));
    let oidc = OidcClients { providers: cfg.oidc.clone() };
    let storage = storage_for(&cfg);
    let mailer: Arc<dyn crate::services::notify::Mailer> = if cfg.email.smtp_url.is_empty() {
        Arc::new(crate::services::notify::MemoryMailer::default())
    } else {
        Arc::new(crate::services::notify::SmtpMailer::new(&cfg.email.smtp_url, &cfg.email.from)?)
    };
    Ok(AppState(Arc::new(Inner {
        db: pool,
        storage,
        blobs: Arc::new(blobs),
        events: Arc::new(InProcessBus::new(1024)),
        jobs: Arc::new(GraphileQueue),
        cache: ResponseCache::new(cfg.limits.cache_bytes),
        secrets,
        repo_locks: RepoLocks::default(),
        hooks: HookConfig { hooks_dir, socket: socket_path(&run_dir), secret: crate::crypto::random_urlsafe(32) },
        metrics,
        highlighter: crate::render::Highlighter::new(),
        images,
        oidc,
        mailer,
        setup_token: Mutex::new(None),
        login_limiter: RateLimiter::new(10, Duration::from_secs(60)),
        login_failures: RateLimiter::new(20, Duration::from_secs(600)),
        totp_failures: RateLimiter::new(5, Duration::from_secs(600)),
        login_ip_failures: RateLimiter::new(100, Duration::from_secs(600)),
        oidc_key: tokio::sync::OnceCell::new(),
        anon_heavy_reads: RateLimiter::new(cfg.limits.anonymous_heavy_reads_per_minute.max(1), Duration::from_secs(60)),
        cfg,
    })))
}

/// git >= 2.45 is required: `--ref-format`, `rev-parse --show-ref-format`,
/// `merge-tree --write-tree`, `replay` (architecture: Git version).
pub fn check_git_version(git: &std::path::Path) -> anyhow::Result<()> {
    let out = std::process::Command::new(git).arg("--version").output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let ver = text.split_whitespace().nth(2).unwrap_or("");
    let mut it = ver.split('.').map(|x| x.parse::<u32>().unwrap_or(0));
    let (major, minor) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    anyhow::ensure!((major, minor) >= (2, 45), "git {ver} is too old: Forgust needs git 2.45 or newer");
    Ok(())
}

/// Unix socket paths are limited to ~108 bytes; fall back to a short path in the
/// system temp dir (unique per data dir) when the data dir is deep.
fn socket_path(run_dir: &std::path::Path) -> std::path::PathBuf {
    let p = run_dir.join("internal.sock");
    if p.as_os_str().len() < 100 {
        return p;
    }
    use sha2::Digest;
    let h = hex::encode(sha2::Sha256::digest(run_dir.as_os_str().as_encoded_bytes()));
    std::env::temp_dir().join(format!("forgust-{}.sock", &h[..16]))
}

async fn health(State(state): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.db).await {
        Ok(_) => (StatusCode::OK, axum::Json(serde_json::json!({ "status": "ok" }))).into_response(),
        Err(e) => {
            // the reason goes to the log, not to whoever asks (it can name hosts and users)
            tracing::error!(error = %e, "health check: database unreachable");
            (StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({ "status": "error", "database": "unreachable" })))
                .into_response()
        }
    }
}

async fn metrics_endpoint(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    let want = &state.cfg.telemetry.metrics_token;
    if !want.is_empty() {
        let given = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
        if !given.is_some_and(|g| crate::crypto::eq_ct(g, want)) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    match &state.metrics {
        Some(h) => ([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], h.render()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Headers every response gets unless it set its own: no MIME sniffing, no
/// framing, no referrer to other sites, a locked-down CSP for anything that is
/// not the web app (API JSON, downloads), and HSTS behind https.
async fn security_headers(State(state): State<AppState>, req: Request, next: Next) -> Response {
    use axum::http::HeaderValue;
    use axum::http::header::*;
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.entry(X_CONTENT_TYPE_OPTIONS).or_insert(HeaderValue::from_static("nosniff"));
    h.entry(X_FRAME_OPTIONS).or_insert(HeaderValue::from_static("DENY"));
    h.entry(REFERRER_POLICY).or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.entry(CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; sandbox"));
    if state.cfg.server.external_url.starts_with("https://") {
        h.entry(STRICT_TRANSPORT_SECURITY).or_insert(HeaderValue::from_static("max-age=31536000"));
    }
    resp
}

/// Request duration histogram by route (the speed targets are measured on these).
async fn track(req: Request, next: Next) -> Response {
    let route = req.extensions().get::<MatchedPath>().map(|m| m.as_str().to_string()).unwrap_or_else(|| "fallback".into());
    let method = req.method().to_string();
    let start = Instant::now();
    let resp = next.run(req).await;
    metrics::histogram!("forgust_http_request_duration_seconds", "route" => route, "method" => method, "status" => resp.status().as_u16().to_string())
        .record(start.elapsed().as_secs_f64());
    resp
}

pub fn router(state: AppState) -> Router {
    let api = crate::api::router().layer(CompressionLayer::new());
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_endpoint))
        .nest("/api/v1", api)
        // GitLab Runner API (stock runners, ADR-0003)
        .nest("/api/v4", crate::ci::runner_api::router().merge(crate::v4::router()))
        // container registry: /v2/ and its token endpoint (ADR-0022)
        .merge(crate::registry::router())
        // OIDC discovery for CI id_tokens
        .merge(crate::ci::oidc::router())
        // the MCP server and its OAuth sign-in (ADR-0025)
        .merge(crate::oauth::router())
        .fallback(crate::web::fallback)
        .layer(middleware::from_fn_with_state(state.clone(), security_headers))
        // Pages sites (their own hosts, their own headers): before anything else
        .layer(middleware::from_fn_with_state(state.clone(), crate::pages::by_host))
        .layer(middleware::from_fn(track))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(tower_http::trace::DefaultMakeSpan::new().level(tracing::Level::INFO))
                .on_response(tower_http::trace::DefaultOnResponse::new().level(tracing::Level::DEBUG)),
        )
        .with_state(state)
}

/// When there is no user yet, print a one-time link to create the first admin.
pub async fn prepare_setup(state: &AppState) -> anyhow::Result<()> {
    if forgust_db::users::count_humans(&state.db).await? == 0 {
        let token = crate::crypto::random_urlsafe(24);
        tracing::warn!("no users yet: create the first administrator at {}/setup?token={token}", state.cfg.server.external_url);
        *state.setup_token.lock().unwrap() = Some(token);
    }
    Ok(())
}

pub struct Running {
    pub http: tokio::task::JoinHandle<()>,
    pub addr: std::net::SocketAddr,
    pub ssh_addr: Option<std::net::SocketAddr>,
}

/// Start the roles this process runs. Returns once listeners are bound.
pub async fn start(state: AppState) -> anyhow::Result<Running> {
    crate::state::spawn_cache_invalidator(state.clone());
    if state.cfg.has_role("worker") {
        crate::jobs::start_worker(state.clone(), 8).await?;
    }
    // internal API for git hooks (Unix socket)
    let sock = state.hooks.socket.clone();
    let _ = std::fs::remove_file(&sock);
    let unix = tokio::net::UnixListener::bind(&sock)?;
    let internal = crate::hooks::internal_router(state.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(unix, internal).await {
            tracing::error!(error = %e, "internal API stopped");
        }
    });

    let mut ssh_addr = None;
    if state.cfg.has_role("api") && !state.cfg.server.ssh_listen.is_empty() {
        let listener = tokio::net::TcpListener::bind(&state.cfg.server.ssh_listen).await?;
        ssh_addr = Some(listener.local_addr()?);
        let s = state.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::ssh::serve(s, listener).await {
                tracing::error!(error = %e, "SSH server stopped");
            }
        });
    }
    prepare_setup(&state).await?;
    let listener = tokio::net::TcpListener::bind(&state.cfg.server.listen).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, url = %state.cfg.server.external_url, "HTTP listening");
    let app = router(state);
    let http = tokio::spawn(async move {
        let shutdown = async {
            let ctrl_c = tokio::signal::ctrl_c();
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
            tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
            tracing::info!("shutting down");
        };
        if let Err(e) =
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).with_graceful_shutdown(shutdown).await
        {
            tracing::error!(error = %e, "HTTP server stopped");
        }
    });
    Ok(Running { http, addr, ssh_addr })
}
