//! Instance administration.

use axum::Json;
use axum::extract::{Path, Query, State};
use forgust_db::audit::AuditEvent;
use forgust_db::settings::InstanceSettings;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::Paging;
use super::dto::UserDto;
use crate::auth::Admin;
use crate::error::{ApiError, ApiResult};
use crate::services::{self, users};
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct Overview {
    pub users: i64,
    pub version: String,
    pub blob_backend: String,
    pub cache_entries: u64,
    pub git_processes: usize,
    pub counts: forgust_db::admin::InstanceCounts,
    pub ci: forgust_db::admin::CiActivity,
    /// Projects with dependency updates on, and Renovate runs going on.
    pub renovate_projects: i64,
    pub renovate_runs: i64,
}

pub async fn overview(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<Overview>> {
    let (counts, ci, renovate_projects, renovate_runs) = tokio::try_join!(
        forgust_db::admin::counts(&state.db),
        forgust_db::admin::ci_activity(&state.db),
        forgust_db::renovate::enabled_count(&state.db),
        forgust_db::renovate::active_runs(&state.db, None),
    )?;
    Ok(Json(Overview {
        users: counts.users_active + counts.users_blocked,
        version: env!("CARGO_PKG_VERSION").into(),
        blob_backend: state.blobs.backend().into(),
        cache_entries: state.cache.entry_count(),
        git_processes: state.storage.catfile_processes(),
        counts,
        ci,
        renovate_projects,
        renovate_runs,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct StorageOverview {
    /// `local` or `s3`.
    pub blob_backend: String,
    /// `s3://bucket` or the blobs directory.
    pub blob_location: String,
    pub kinds: forgust_db::admin::StorageKinds,
    pub total: i64,
    /// Largest first: what each keeps, LFS objects and image layers counted for every project using them.
    pub projects: Vec<forgust_db::admin::ProjectStorage>,
    /// Top-level groups and personal namespaces, largest first.
    pub namespaces: Vec<forgust_db::admin::GroupStorage>,
    /// Repositories not measured yet (sizes refresh after pushes and hourly).
    pub repositories_unmeasured: i64,
    /// The last listing of the blob store, if any.
    pub scan: Option<forgust_db::admin::BlobScan>,
    /// The storage quota of every top-level group and personal namespace
    /// without its own.
    pub default_quota: Option<i64>,
}

/// What the instance keeps, by kind and by project.
pub async fn storage(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<StorageOverview>> {
    let (kinds, projects, namespaces, scan) = tokio::try_join!(
        forgust_db::admin::storage_kinds(&state.db),
        forgust_db::admin::largest_projects(&state.db, 25),
        forgust_db::admin::largest_namespaces(&state.db, 10),
        forgust_db::admin::blob_scan(&state.db),
    )?;
    let unmeasured = forgust_db::admin::stale_repository_sizes(&state.db, 10_000).await?.len() as i64;
    if unmeasured > 0 {
        // measured in the background (one job at a time); the page shows them later
        let mut conn = state.db.acquire().await?;
        state.jobs.enqueue(&mut conn, crate::jobs::MeasureRepositories.request()).await.map_err(ApiError::Internal)?;
    }
    let k = &kinds;
    let total = k.repositories + k.lfs + k.artifacts + k.job_logs_archived + k.job_logs_live + k.registry + k.uploads;
    let b = &state.cfg.blobs;
    let blob_location =
        if state.blobs.backend() == "s3" { format!("s3://{}", b.s3_bucket) } else { state.cfg.blobs_dir().display().to_string() };
    Ok(Json(StorageOverview {
        blob_backend: state.blobs.backend().into(),
        blob_location,
        kinds,
        total,
        projects,
        namespaces,
        repositories_unmeasured: unmeasured,
        scan,
        default_quota: forgust_db::quotas::default_quota(&state.db).await?,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct SetQuota {
    /// Bytes. For the instance default and projects: None or 0 for no quota.
    /// For a namespace: None for the instance default, 0 for no quota.
    pub bytes: Option<i64>,
}

fn valid_quota(b: Option<i64>) -> ApiResult<()> {
    if b.is_some_and(|b| b < 0) {
        return Err(ApiError::bad("a quota is a number of bytes (0 for none)"));
    }
    Ok(())
}

/// Then the banners and emails follow at once.
async fn quotas_changed(state: &AppState) {
    if let Err(e) = crate::services::quotas::measure(state).await {
        tracing::warn!("measuring quotas: {e:#}");
    }
}

/// The storage quota of each top-level group and personal namespace without its own.
pub async fn set_default_quota(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Json(req): Json<SetQuota>,
) -> ApiResult<Json<serde_json::Value>> {
    valid_quota(req.bytes)?;
    let bytes = req.bytes.filter(|b| *b > 0);
    forgust_db::quotas::set_default_quota(&state.db, bytes).await?;
    services::audit(&state, &p, "storage_quota_changed", None, json!({ "scope": "default", "bytes": bytes }), None).await;
    quotas_changed(&state).await;
    Ok(Json(json!({ "bytes": bytes })))
}

pub async fn set_namespace_quota(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Path(id): Path<i64>,
    Json(req): Json<SetQuota>,
) -> ApiResult<Json<serde_json::Value>> {
    valid_quota(req.bytes)?;
    let ns = forgust_db::namespaces::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    if ns.parent_id.is_some() {
        return Err(ApiError::bad("quotas are set on top-level groups (a subgroup counts for its organization) or on projects"));
    }
    forgust_db::quotas::set_namespace_quota(&state.db, ns.id, req.bytes).await?;
    services::audit(&state, &p, "storage_quota_changed", Some(("namespace", ns.id)), json!({ "bytes": req.bytes }), None).await;
    quotas_changed(&state).await;
    Ok(Json(json!({ "bytes": req.bytes })))
}

pub async fn set_project_quota(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Path(id): Path<i64>,
    Json(req): Json<SetQuota>,
) -> ApiResult<Json<serde_json::Value>> {
    valid_quota(req.bytes)?;
    let proj = forgust_db::projects::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    let bytes = req.bytes.filter(|b| *b > 0);
    forgust_db::quotas::set_project_quota(&state.db, proj.id, bytes).await?;
    services::audit(&state, &p, "storage_quota_changed", Some(("project", proj.id)), json!({ "bytes": bytes }), None).await;
    quotas_changed(&state).await;
    Ok(Json(json!({ "bytes": bytes })))
}

/// List the blob store by prefix in the background (one scan at a time).
pub async fn scan_storage(State(state): State<AppState>, Admin(_, p): Admin) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.db.begin().await?;
    if !forgust_db::admin::start_blob_scan(&mut *tx).await? {
        return Err(ApiError::conflict("a scan is already running"));
    }
    state.jobs.enqueue(&mut tx, crate::jobs::ScanBlobs.request()).await.map_err(ApiError::Internal)?;
    tx.commit().await?;
    services::audit(&state, &p, "blob_scan_started", None, json!({}), None).await;
    Ok(Json(json!({ "started": true })))
}

#[derive(Serialize, ToSchema)]
pub struct HealthCheck {
    pub name: String,
    /// ok, warn or fail
    pub status: String,
    pub detail: String,
    /// Milliseconds the check took, when it talks to something.
    pub millis: Option<u64>,
}

#[derive(Serialize, ToSchema)]
pub struct Health {
    pub checks: Vec<HealthCheck>,
}

fn check(name: &str, status: &str, detail: impl Into<String>, millis: Option<u64>) -> HealthCheck {
    HealthCheck { name: name.into(), status: status.into(), detail: detail.into(), millis }
}

/// Is every part of the instance working: database, blob store, email, git, workers, runners.
pub async fn health(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<Health>> {
    use sqlx::Row;
    let mut checks = Vec::new();

    let t = std::time::Instant::now();
    match sqlx::query(
        "SELECT version() AS v, pg_database_size(current_database()) AS size,
                (SELECT count(*) FROM _sqlx_migrations) AS n, (SELECT max(version) FROM _sqlx_migrations) AS last",
    )
    .fetch_one(&state.db)
    .await
    {
        Ok(r) => {
            let v: String = r.try_get("v").unwrap_or_default();
            let size: i64 = r.try_get("size").unwrap_or(0);
            let n: i64 = r.try_get("n").unwrap_or(0);
            let last: Option<i64> = r.try_get("last").unwrap_or(None);
            let short = v.split(" on ").next().unwrap_or(&v).to_string();
            checks.push(check(
                "Database",
                "ok",
                format!("{short} · {} MB · {n} migrations, last {}", size / 1_048_576, last.unwrap_or(0)),
                Some(t.elapsed().as_millis() as u64),
            ));
        }
        Err(e) => checks.push(check("Database", "fail", forgust_db::admin::redact_error(&e.to_string()), None)),
    }

    // write, read and delete a small object
    let t = std::time::Instant::now();
    let key = format!(".health/{}", crate::crypto::random_token("probe_"));
    let probe = async {
        state.blobs.put_bytes(&key, bytes::Bytes::from_static(b"ok")).await?;
        let got = state.blobs.size(&key).await?;
        state.blobs.delete(&key).await?;
        anyhow::ensure!(got == Some(2), "the object read back has the wrong size");
        anyhow::Ok(())
    };
    match probe.await {
        Ok(()) => checks.push(check(
            "Blob store",
            "ok",
            format!("{}: wrote, read and deleted an object", state.blobs.backend()),
            Some(t.elapsed().as_millis() as u64),
        )),
        Err(e) => checks.push(check("Blob store", "fail", forgust_db::admin::redact_error(&e.to_string()), None)),
    }

    let smtp = &state.cfg.email.smtp_url;
    checks.push(if smtp.is_empty() {
        check("Email", "warn", "not configured: no password resets, confirmations or notifications", None)
    } else {
        let host = url::Url::parse(smtp).ok().and_then(|u| u.host_str().map(|h| format!("{}://{h}:{}", u.scheme(), u.port().unwrap_or(0))));
        check("Email", "ok", format!("{} (send a test email to check delivery)", host.unwrap_or_else(|| "configured".into())), None)
    });

    match tokio::process::Command::new("git").arg("--version").output().await {
        Ok(o) if o.status.success() => checks.push(check("Git", "ok", String::from_utf8_lossy(&o.stdout).trim().to_string(), None)),
        _ => checks.push(check("Git", "fail", "git does not run", None)),
    }

    // background jobs: is anything waiting much longer than it should?
    match sqlx::query(
        "SELECT count(*) FILTER (WHERE locked_at IS NULL AND attempts < max_attempts AND run_at < now()) AS due,
                min(run_at) FILTER (WHERE locked_at IS NULL AND attempts < max_attempts AND run_at < now()) AS oldest,
                count(*) FILTER (WHERE attempts >= max_attempts) AS failed
           FROM graphile_worker.jobs",
    )
    .fetch_one(&state.db)
    .await
    {
        Ok(r) => {
            let due: i64 = r.try_get("due").unwrap_or(0);
            let failed: i64 = r.try_get("failed").unwrap_or(0);
            let oldest: Option<chrono::DateTime<chrono::Utc>> = r.try_get("oldest").unwrap_or(None);
            let late = oldest.map(|o| (chrono::Utc::now() - o).num_seconds()).unwrap_or(0);
            let status = if late > 600 {
                "fail"
            } else if late > 60 || failed > 0 {
                "warn"
            } else {
                "ok"
            };
            let mut d = format!("{due} due now");
            if late > 0 {
                d.push_str(&format!(", the oldest for {late} s"));
            }
            if failed > 0 {
                d.push_str(&format!(" · {failed} failed (see Jobs)"));
            }
            checks.push(check("Background jobs", status, d, None));
        }
        Err(e) => checks.push(check("Background jobs", "fail", forgust_db::admin::redact_error(&e.to_string()), None)),
    }

    match forgust_db::admin::ci_activity(&state.db).await {
        Ok(ci) => checks.push(check(
            "Runners",
            if ci.runners_online > 0 { "ok" } else { "warn" },
            format!("{} online of {} ({} paused) · {} jobs waiting", ci.runners_online, ci.runners, ci.runners_paused, ci.jobs_queued),
            None,
        )),
        Err(e) => checks.push(check("Runners", "fail", forgust_db::admin::redact_error(&e.to_string()), None)),
    }
    Ok(Json(Health { checks }))
}

#[derive(Serialize, ToSchema)]
pub struct JobsOverview {
    pub queues: Vec<forgust_db::admin::TaskQueue>,
    /// Jobs out of attempts, newest first (at most 100).
    pub failed: Vec<forgust_db::admin::FailedJob>,
}

/// Run a failed job again now.
pub async fn retry_job(State(state): State<AppState>, Admin(_, p): Admin, Path(id): Path<i64>) -> ApiResult<Json<serde_json::Value>> {
    if !forgust_db::admin::retry_job(&state.db, id).await? {
        return Err(ApiError::conflict("only a failed job a worker does not hold can be retried"));
    }
    services::audit(&state, &p, "job_retried", None, json!({ "job": id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

/// Drop a failed job.
pub async fn drop_job(State(state): State<AppState>, Admin(_, p): Admin, Path(id): Path<i64>) -> ApiResult<axum::http::StatusCode> {
    if !forgust_db::admin::drop_job(&state.db, id).await? {
        return Err(ApiError::conflict("only a failed job a worker does not hold can be dropped"));
    }
    services::audit(&state, &p, "job_dropped", None, json!({ "job": id }), None).await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// The scheduled tasks an administrator may run now.
const RUNNABLE: &[(&str, &str)] = &[
    ("ci_sweep", "CI housekeeping: stuck jobs, log archiving, expired artifacts, registry cleanup and garbage collection"),
    ("maintenance_sweep", "Repository maintenance (full repack of repositories pushed to) and repository sizes"),
    ("cleanup", "Expired sessions and tokens, idle git processes"),
    ("measure_repositories", "Repository sizes for Storage"),
];

#[derive(Serialize, ToSchema)]
pub struct Runnable {
    pub task: String,
    pub description: String,
}

pub async fn runnable(Admin(..): Admin) -> Json<Vec<Runnable>> {
    Json(RUNNABLE.iter().map(|(t, d)| Runnable { task: (*t).into(), description: (*d).into() }).collect())
}

/// Run a scheduled task now (queued; at most one waits at a time).
pub async fn run_task(State(state): State<AppState>, Admin(_, p): Admin, Path(task): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let Some((name, _)) = RUNNABLE.iter().find(|(t, _)| *t == task) else { return Err(ApiError::NotFound) };
    let req = crate::jobs::JobRequest {
        task: name,
        job_key: Some(format!("admin:{name}")),
        max_attempts: Some(1),
        payload: serde_json::Value::Null,
        ..Default::default()
    };
    let mut conn = state.db.acquire().await?;
    state.jobs.enqueue(&mut conn, req).await.map_err(ApiError::Internal)?;
    services::audit(&state, &p, "task_run", None, json!({ "task": name }), None).await;
    Ok(Json(json!({ "queued": true })))
}

#[derive(Deserialize, ToSchema)]
pub struct TestEmail {
    /// Default: the administrator's own address.
    #[serde(default)]
    pub to: Option<String>,
}

/// Send a test email now (not queued), so the answer says whether it left.
pub async fn test_email(
    State(state): State<AppState>,
    Admin(me, p): Admin,
    Json(b): Json<TestEmail>,
) -> ApiResult<Json<serde_json::Value>> {
    if state.cfg.email.smtp_url.is_empty() {
        return Err(ApiError::conflict("email is not configured (email.smtp_url)"));
    }
    let to = b.to.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| me.email.clone());
    crate::services::paths::validate_email(&to)?;
    let email = crate::services::notify::Email {
        to: vec![to.clone()],
        subject: "Forgust test email".into(),
        body: format!("This is a test email from {}, sent by {} from Admin > Health.\n", state.cfg.server.external_url, me.username),
        ..Default::default()
    };
    state.mailer.send(&email).await.map_err(|e| ApiError::bad(format!("not sent: {}", forgust_db::admin::redact_error(&e.to_string()))))?;
    services::audit(&state, &p, "test_email_sent", None, json!({ "to": to }), None).await;
    Ok(Json(json!({ "sent": to })))
}

/// The banner everyone sees (none when empty or ended). Public: signed-out pages show it too.
pub async fn banner(State(state): State<AppState>) -> ApiResult<Json<Option<forgust_db::admin::Banner>>> {
    let b = forgust_db::admin::banner(&state.db).await?;
    let live = !b.text.trim().is_empty() && b.until.is_none_or(|u| u > chrono::Utc::now());
    Ok(Json(live.then_some(b)))
}

/// The banner as set (also when ended), for the admin's form.
pub async fn banner_settings(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<forgust_db::admin::Banner>> {
    Ok(Json(forgust_db::admin::banner(&state.db).await?))
}

pub async fn set_banner(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Json(b): Json<forgust_db::admin::Banner>,
) -> ApiResult<Json<forgust_db::admin::Banner>> {
    let text = b.text.trim().to_string();
    if text.chars().count() > 500 || text.contains('\n') {
        return Err(ApiError::bad("the banner is one line of at most 500 characters"));
    }
    if b.tone != "info" && b.tone != "warn" {
        return Err(ApiError::bad("tone is info or warn"));
    }
    let b = forgust_db::admin::Banner { text, tone: b.tone, until: b.until };
    forgust_db::admin::set_banner(&state.db, &b).await?;
    services::audit(&state, &p, "banner_set", None, json!({ "text": b.text, "until": b.until }), None).await;
    Ok(Json(b))
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    search: String,
    /// projects: size, activity (default) or name
    #[serde(default)]
    sort: String,
    #[serde(default = "first_page")]
    page: i64,
    #[serde(default = "per_page")]
    per_page: i64,
}

/// Every project, with what it keeps and how busy it is.
pub async fn projects(
    State(state): State<AppState>,
    Admin(..): Admin,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<forgust_db::admin::AdminProject>>> {
    let paging = Paging { page: q.page, per_page: q.per_page, search: q.search };
    Ok(Json(forgust_db::admin::projects(&state.db, &paging.search, &q.sort, paging.limit(), paging.offset()).await?))
}

/// Every group.
pub async fn groups(
    State(state): State<AppState>,
    Admin(..): Admin,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<forgust_db::admin::AdminGroup>>> {
    let paging = Paging { page: q.page, per_page: q.per_page, search: q.search };
    Ok(Json(forgust_db::admin::groups(&state.db, &paging.search, paging.limit(), paging.offset()).await?))
}

/// Repack a project's repository now (full maintenance, queued with its pushes).
pub async fn project_maintenance(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let proj = forgust_db::projects::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    let mut conn = state.db.acquire().await?;
    state
        .jobs
        .enqueue(&mut conn, crate::jobs::Maintenance { project_id: proj.id, full: true }.request())
        .await
        .map_err(ApiError::Internal)?;
    crate::jobs::refresh_repository_size(&state, proj.id).await;
    services::audit(&state, &p, "repository_maintenance", Some(("project", proj.id)), json!({}), None).await;
    Ok(Json(json!({ "queued": true })))
}

#[derive(Serialize, ToSchema)]
pub struct BackupEntry {
    /// `forgust-<UTC time>`
    pub name: String,
    pub bytes: u64,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Its manifest is written (the last step): restorable with `forgust restore`.
    pub complete: bool,
    pub repositories: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct Backups {
    /// On the server (the volume behind `server.data_dir`).
    pub location: String,
    /// A backup is queued or running.
    pub running: bool,
    pub backups: Vec<BackupEntry>,
}

fn dir_size(path: &std::path::Path) -> u64 {
    std::fs::read_dir(path)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

fn valid_backup_name(name: &str) -> bool {
    name.starts_with("forgust-") && name.len() < 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Backups made from here (the database and the repositories; not the blob store).
pub async fn backups(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<Backups>> {
    use sqlx::Row;
    let dir = state.cfg.backups_dir();
    let list = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !valid_backup_name(&name) || !e.path().is_dir() {
                continue;
            }
            let manifest: Option<crate::backup::Manifest> =
                std::fs::read(e.path().join("manifest.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
            out.push(BackupEntry {
                bytes: dir_size(&e.path()),
                created_at: manifest.as_ref().map(|m| m.created_at),
                complete: manifest.is_some(),
                repositories: manifest.map(|m| m.projects.len()),
                name,
            });
        }
        out.sort_by(|a, b| b.name.cmp(&a.name));
        out
    })
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    let running =
        sqlx::query("SELECT count(*) AS n FROM graphile_worker.jobs WHERE task_identifier = 'backup' AND attempts < max_attempts")
            .fetch_one(&state.db)
            .await
            .map(|r| r.try_get::<i64, _>("n").unwrap_or(0) > 0)
            .unwrap_or(false);
    Ok(Json(Backups { location: state.cfg.backups_dir().display().to_string(), running, backups: list }))
}

/// Back up the database and every repository now, in the background.
pub async fn start_backup(State(state): State<AppState>, Admin(_, p): Admin) -> ApiResult<Json<serde_json::Value>> {
    let mut conn = state.db.acquire().await?;
    state.jobs.enqueue(&mut conn, crate::jobs::Backup.request()).await.map_err(ApiError::Internal)?;
    services::audit(&state, &p, "backup_started", None, json!({}), None).await;
    Ok(Json(json!({ "queued": true })))
}

/// Delete a backup made from here.
pub async fn delete_backup(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Path(name): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    if !valid_backup_name(&name) {
        return Err(ApiError::NotFound);
    }
    let path = state.cfg.backups_dir().join(&name);
    if !path.is_dir() {
        return Err(ApiError::NotFound);
    }
    tokio::fs::remove_dir_all(&path).await.map_err(|e| ApiError::Internal(e.into()))?;
    services::audit(&state, &p, "backup_deleted", None, json!({ "name": name }), None).await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// The background job queue.
pub async fn jobs(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<JobsOverview>> {
    let (queues, failed) = tokio::try_join!(forgust_db::admin::job_queues(&state.db), forgust_db::admin::failed_jobs(&state.db, 100))?;
    Ok(Json(JobsOverview { queues, failed }))
}

#[derive(Deserialize)]
pub struct UserQuery {
    /// all (people), admins, blocked, external, unconfirmed, two_factor_off, bots
    #[serde(default)]
    filter: String,
    #[serde(default)]
    search: String,
    #[serde(default = "first_page")]
    page: i64,
    #[serde(default = "per_page")]
    per_page: i64,
}

fn first_page() -> i64 {
    1
}
fn per_page() -> i64 {
    50
}

pub async fn users(
    State(state): State<AppState>,
    Admin(..): Admin,
    Query(q): Query<UserQuery>,
) -> ApiResult<Json<Vec<forgust_db::admin::AdminUser>>> {
    let paging = Paging { page: q.page, per_page: q.per_page, search: q.search };
    Ok(Json(forgust_db::admin::users(&state.db, &paging.search, &q.filter, paging.limit(), paging.offset()).await?))
}

/// A person an administrator acts on (not a bot: bots have their own owners).
async fn person(state: &AppState, id: i64) -> ApiResult<forgust_db::users::User> {
    let u = forgust_db::users::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    if u.is_bot() {
        return Err(ApiError::bad("bots are managed where they belong (project, group or dependency updates)"));
    }
    Ok(u)
}

#[derive(Deserialize)]
pub struct DeleteUserQuery {
    /// Delete their personal projects too (else they block the deletion).
    #[serde(default)]
    delete_projects: bool,
}

/// Delete a person. What they wrote (merge requests, issues, comments, pipelines)
/// stays, without its author; their memberships, keys, tokens, approvals and
/// to-dos go. Refused for yourself, the only Owner of a group, and while they
/// have personal projects (unless those go too).
pub async fn delete_user(
    State(state): State<AppState>,
    Admin(me, p): Admin,
    Path(id): Path<i64>,
    Query(q): Query<DeleteUserQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let u = person(&state, id).await?;
    if u.id == me.id {
        return Err(ApiError::bad("you cannot delete your own account here"));
    }
    if u.is_admin && u.state == "active" && forgust_db::admin::active_admins(&state.db).await? <= 1 {
        return Err(ApiError::conflict("this is the last administrator"));
    }
    let groups = forgust_db::admin::sole_owned_groups(&state.db, u.id).await?;
    if !groups.is_empty() {
        return Err(ApiError::conflict(format!(
            "{} is the only Owner of {}: make someone else Owner first",
            u.username,
            groups.join(", ")
        )));
    }
    let projects = forgust_db::admin::personal_projects(&state.db, u.id).await?;
    if !projects.is_empty() && !q.delete_projects {
        return Err(ApiError::conflict(format!(
            "{} has personal projects ({}): transfer them, or delete them with the account",
            u.username,
            projects.iter().map(|(_, path)| path.as_str()).collect::<Vec<_>>().join(", ")
        )));
    }
    for (pid, _) in &projects {
        if let Some(proj) = forgust_db::projects::find(&state.db, *pid).await? {
            crate::services::projects::delete(&state, &p, &proj).await?;
        }
    }
    let mut tx = state.db.begin().await?;
    forgust_db::admin::delete_user(&mut tx, u.id).await?;
    tx.commit().await?;
    if u.avatar_at.is_some()
        && let Err(e) = state.blobs.delete(&format!("avatars/user/{}.png", u.id)).await
    {
        tracing::warn!(user = u.id, error = %e, "avatar not deleted");
    }
    services::audit(
        &state,
        &p,
        "user_deleted",
        Some(("user", u.id)),
        json!({ "username": u.username, "email": u.email, "projects_deleted": projects.len() }),
        None,
    )
    .await;
    Ok(Json(json!({ "deleted": u.username, "projects_deleted": projects.len() })))
}

/// Turn two-factor authentication off (a lost authenticator); they set it up again.
pub async fn reset_two_factor(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let u = person(&state, id).await?;
    forgust_db::users::set_totp(&state.db, u.id, None, false).await?;
    services::audit(&state, &p, "two_factor_reset", Some(("user", u.id)), json!({}), None).await;
    Ok(Json(json!({ "ok": true })))
}

/// End every session of a user (they sign in again).
pub async fn sign_out(State(state): State<AppState>, Admin(me, p): Admin, Path(id): Path<i64>) -> ApiResult<Json<serde_json::Value>> {
    let u = person(&state, id).await?;
    if u.id == me.id {
        return Err(ApiError::bad("sign yourself out from your profile instead"));
    }
    forgust_db::sessions::delete_for_user(&state.db, u.id).await?;
    services::audit(&state, &p, "sessions_revoked", Some(("user", u.id)), json!({}), None).await;
    Ok(Json(json!({ "ok": true })))
}

/// Revoke every personal access token of a user.
pub async fn revoke_tokens(State(state): State<AppState>, Admin(_, p): Admin, Path(id): Path<i64>) -> ApiResult<Json<serde_json::Value>> {
    let u = person(&state, id).await?;
    let n = forgust_db::admin::revoke_personal_tokens(&state.db, u.id).await?;
    services::audit(&state, &p, "tokens_revoked", Some(("user", u.id)), json!({ "count": n }), None).await;
    Ok(Json(json!({ "revoked": n })))
}

/// Email the confirmation link again (to the new address when one waits).
pub async fn resend_confirmation(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let u = person(&state, id).await?;
    let to = match (&u.unconfirmed_email, u.email_verified_at) {
        (Some(new), _) => new.clone(),
        (None, None) => u.email.clone(),
        (None, Some(_)) => return Err(ApiError::conflict("this address is already confirmed")),
    };
    crate::services::accounts::send_confirmation(&state, &u, &to).await?;
    services::audit(&state, &p, "confirmation_resent", Some(("user", u.id)), json!({}), None).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateUser {
    pub username: String,
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub password: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
}

pub async fn create_user(State(state): State<AppState>, Admin(_, p): Admin, Json(req): Json<CreateUser>) -> ApiResult<Json<UserDto>> {
    let mut tx = state.db.begin().await?;
    let u = users::create(
        &mut tx,
        users::CreateUser {
            username: &req.username,
            email: &req.email,
            name: &req.name,
            password: req.password.as_deref(),
            is_admin: req.is_admin,
            email_verified: true,
        },
    )
    .await?;
    tx.commit().await?;
    services::audit(&state, &p, "user_created", Some(("user", u.id)), json!({ "username": u.username, "admin": u.is_admin }), None).await;
    Ok(Json(UserDto::from(&u, true)))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateUser {
    pub is_admin: Option<bool>,
    /// External people see only what they are a member of.
    pub is_external: Option<bool>,
    /// `active` or `blocked`.
    pub state: Option<String>,
    pub password: Option<String>,
}

pub async fn update_user(
    State(state): State<AppState>,
    Admin(me, p): Admin,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUser>,
) -> ApiResult<Json<UserDto>> {
    let u = forgust_db::users::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    if let Some(admin) = req.is_admin {
        if u.id == me.id && !admin {
            return Err(ApiError::bad("you cannot remove your own admin rights"));
        }
        if u.is_bot() && admin {
            return Err(ApiError::bad("bots cannot be administrators"));
        }
        forgust_db::users::set_admin(&state.db, id, admin).await?;
        services::audit(&state, &p, if admin { "admin_granted" } else { "admin_revoked" }, Some(("user", id)), json!({}), None).await;
    }
    if let Some(s) = req.state.as_deref() {
        if s != "active" && s != "blocked" {
            return Err(ApiError::bad("state must be active or blocked"));
        }
        if u.id == me.id && s == "blocked" {
            return Err(ApiError::bad("you cannot block yourself"));
        }
        forgust_db::users::set_state(&state.db, id, s).await?;
        if s == "blocked" {
            forgust_db::sessions::delete_for_user(&state.db, id).await?;
        }
        services::audit(&state, &p, if s == "blocked" { "user_blocked" } else { "user_unblocked" }, Some(("user", id)), json!({}), None)
            .await;
    }
    if let Some(ext) = req.is_external {
        if u.is_bot() {
            return Err(ApiError::bad("bots cannot be external"));
        }
        if ext && u.is_admin {
            return Err(ApiError::bad("administrators cannot be external"));
        }
        forgust_db::admin::set_external(&state.db, id, ext).await?;
        services::audit(&state, &p, if ext { "user_made_external" } else { "user_made_internal" }, Some(("user", id)), json!({}), None)
            .await;
    }
    if let Some(pw) = req.password.as_deref() {
        users::validate_password(pw)?;
        forgust_db::users::set_password(&state.db, id, &crate::crypto::hash_password_gated(pw).await?).await?;
        // whoever was signed in with the old password is not anymore
        forgust_db::sessions::delete_for_user(&state.db, id).await?;
        services::audit(&state, &p, "password_reset", Some(("user", id)), json!({}), None).await;
    }
    let u = forgust_db::users::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(UserDto::from(&u, true)))
}

pub async fn settings(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<InstanceSettings>> {
    Ok(Json(forgust_db::settings::get(&state.db).await?))
}

pub async fn update_settings(
    State(state): State<AppState>,
    Admin(_, p): Admin,
    Json(req): Json<InstanceSettings>,
) -> ApiResult<Json<InstanceSettings>> {
    if !forgust_storage::validate_rev(&req.default_branch_name) {
        return Err(ApiError::bad("invalid default branch name"));
    }
    forgust_db::settings::update(&state.db, &req).await?;
    services::audit(&state, &p, "instance_settings_updated", None, serde_json::to_value(&req).unwrap_or_default(), None).await;
    Ok(Json(forgust_db::settings::get(&state.db).await?))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    target_type: Option<String>,
    target_id: Option<i64>,
    before: Option<i64>,
}

pub async fn audit(State(state): State<AppState>, Admin(..): Admin, Query(q): Query<AuditQuery>) -> ApiResult<Json<Vec<AuditEvent>>> {
    Ok(Json(forgust_db::audit::list(&state.db, q.target_type.as_deref(), q.target_id, 200, q.before).await?))
}
