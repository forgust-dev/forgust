//! Background jobs behind our own `JobQueue` trait (ADR-0004).
//!
//! Enqueueing happens inside the caller's transaction through graphile_worker's SQL
//! API (`graphile_worker.add_job`), so a job exists if and only if the write that
//! caused it committed. The worker side uses the graphile_worker crate.

use async_trait::async_trait;
use graphile_worker::{IntoTaskHandlerResult, TaskHandler, WorkerContext, WorkerOptions};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

use crate::events::Event;
use crate::state::AppState;

/// Tasks without arguments accept whatever payload they are queued with:
/// the queue stores "none" as `{}`, cron adds `{"_cron": ...}`, and a unit
/// struct's own `Deserialize` only takes `null` (its jobs then failed on
/// "invalid type: map, expected unit struct").
macro_rules! payload_free {
    ($name:ident) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                serde::de::IgnoredAny::deserialize(d)?;
                Ok($name)
            }
        }
    };
}

pub const SCHEMA: &str = "graphile_worker";

#[derive(Debug, Clone, Default)]
pub struct JobRequest {
    pub task: &'static str,
    pub payload: serde_json::Value,
    /// Jobs in the same named queue run one at a time (per-repo serial work).
    pub queue: Option<String>,
    /// Deduplicates pending jobs with the same key.
    pub job_key: Option<String>,
    pub max_attempts: Option<i32>,
}

#[async_trait]
pub trait JobQueue: Send + Sync + 'static {
    /// Enqueue inside the caller's transaction.
    async fn enqueue(&self, conn: &mut PgConnection, job: JobRequest) -> anyhow::Result<()>;
}

pub struct GraphileQueue;

#[async_trait]
impl JobQueue for GraphileQueue {
    async fn enqueue(&self, conn: &mut PgConnection, job: JobRequest) -> anyhow::Result<()> {
        // Runtime-checked: the graphile_worker schema is created by the worker at
        // startup, so it is not visible to compile-time query checks.
        sqlx::query(
            "SELECT graphile_worker.add_job($1, $2::json, queue_name => $3, job_key => $4,
                     max_attempts => coalesce($5, 25), job_key_mode => 'replace')",
        )
        .bind(job.task)
        .bind(&job.payload)
        .bind(job.queue)
        .bind(job.job_key)
        .bind(job.max_attempts)
        .execute(conn)
        .await?;
        Ok(())
    }
}

pub fn repo_queue(project_id: i64) -> String {
    format!("repo:{project_id}")
}

// ------------------------------------------------------------------ tasks

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefUpdate {
    pub old: String,
    pub new: String,
    pub name: String,
}

/// Follow-up work after a push (enqueued by the post-receive hook).
#[derive(Debug, Serialize, Deserialize)]
pub struct PostReceive {
    pub project_id: i64,
    pub updates: Vec<RefUpdate>,
    /// Who pushed (`kind:id`, see `Principal::encode`); pipelines run as them.
    #[serde(default)]
    pub principal: Option<String>,
}

/// Where a pushed ref's new commits start: its old commit, or for a new ref
/// the default branch's head (none when it is the default branch).
async fn push_base(state: &AppState, p: &forgust_db::projects::Project, u: &RefUpdate) -> Option<String> {
    if u.old != forgust_storage::ZERO_OID {
        return Some(u.old.clone());
    }
    match p.default_branch.as_deref().map(|b| format!("refs/heads/{b}")).filter(|r| *r != u.name) {
        Some(r) => state.storage.resolve(p.id, &r).await.ok().flatten(),
        None => None,
    }
}

/// The commits a push brought to a ref (GitLab's push hook: the newest 20, and
/// how many in all), from `from` ([`push_base`]).
async fn push_commits(
    state: &AppState,
    p: &forgust_db::projects::Project,
    u: &RefUpdate,
    from: Option<&str>,
) -> (Vec<serde_json::Value>, usize) {
    use forgust_storage::ZERO_OID;
    if u.new == ZERO_OID || u.name.starts_with("refs/tags/") {
        return (Vec::new(), 0);
    }
    let list = match from {
        Some(f) => state.storage.log_between(p.id, f, &u.new, 20).await,
        None => state.storage.log(p.id, &u.new, None, 0, 20).await,
    };
    let Ok(list) = list else { return (Vec::new(), 0) };
    let total = match from {
        Some(f) if list.len() == 20 => state.storage.rev_list(p.id, f, &u.new, false).await.map(|v| v.len()).unwrap_or(list.len()),
        None if list.len() == 20 => state.storage.commit_count(p.id, &u.new).await.map(|n| n as usize).unwrap_or(list.len()),
        _ => list.len(),
    };
    let base = state.cfg.web_url(&p.full_path);
    let commits = list
        .iter()
        .map(|c| {
            let message = if c.body.is_empty() { c.subject.clone() } else { format!("{}\n\n{}", c.subject, c.body) };
            serde_json::json!({
                "id": c.id, "message": message, "title": c.subject, "url": format!("{base}/-/commit/{}", c.id),
                "author": { "name": c.author.name, "email": c.author.email },
            })
        })
        .collect();
    (commits, total)
}

impl PostReceive {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            // one at a time per repo, in push order (MR versions follow the
            // branch); not the maintenance queue, so pushes never wait on a repack
            queue: Some(format!("push:{}", self.project_id)),
            payload: serde_json::to_value(&self).unwrap_or_default(),
            job_key: None,
            max_attempts: Some(5),
        }
    }
}

impl PostReceive {
    /// Pipelines, push webhooks and maintenance accounting: never repeated.
    async fn once(&self, state: &AppState) -> anyhow::Result<()> {
        let pusher = match &self.principal {
            Some(p) => crate::hooks::decode_principal(state, p).await.ok(),
            None => None,
        };
        if let Err(e) = crate::ci::on_push(state, self.project_id, &self.updates, pusher.as_ref()).await {
            tracing::warn!(project_id = self.project_id, error = %e, "CI on push failed");
        }
        if let Some(p) = forgust_db::projects::find(&state.db, self.project_id).await? {
            let deleted_tags: Vec<String> = self
                .updates
                .iter()
                .filter(|u| u.new == forgust_storage::ZERO_OID)
                .filter_map(|u| u.name.strip_prefix("refs/tags/").map(str::to_string))
                .collect();
            if !deleted_tags.is_empty()
                && let Err(e) = crate::services::releases::on_tags_deleted(state, &p, &deleted_tags).await
            {
                tracing::warn!(project_id = self.project_id, error = %e, "releases of deleted tags not deleted");
            }
            let who = pusher.as_ref().and_then(|p| p.user());
            // the contributions calendar: a push event per branch or tag a person pushed
            let person = pusher.as_ref().and_then(|p| p.direct_user());
            for u in &self.updates {
                let from = if u.new == forgust_storage::ZERO_OID { None } else { push_base(state, &p, u).await };
                let (commits, total) = push_commits(state, &p, u, from.as_deref()).await;
                if let Some(user) = person
                    && u.new != forgust_storage::ZERO_OID
                    && (u.name.starts_with("refs/heads/") || u.name.starts_with("refs/tags/"))
                {
                    // a branch's count is the hook's; a tag's is walked here
                    let n = match (&from, u.name.starts_with("refs/heads/")) {
                        (Some(_), true) => total,
                        (Some(f), false) => state.storage.rev_list(p.id, f, &u.new, false).await.map(|v| v.len()).unwrap_or(1),
                        (None, _) => 1,
                    };
                    if let Err(e) = forgust_db::contributions::record_push(&state.db, user.id, p.id, &u.name, n.min(100_000) as i32).await {
                        tracing::warn!(project_id = p.id, error = %e, "push event not recorded");
                    }
                }
                let payload = serde_json::json!({
                    "object_kind": "push", "event_name": "push", "ref": u.name, "before": u.old, "after": u.new,
                    "checkout_sha": (u.new != forgust_storage::ZERO_OID).then_some(&u.new),
                    "user_id": who.map(|w| w.id), "user_username": who.map(|w| &w.username), "user_name": who.map(|w| &w.name),
                    "project": crate::services::mr::project_json(state, &p),
                    "commits": commits, "total_commits_count": total,
                });
                crate::services::webhooks::emit(state, &p, "push", payload).await;
            }
        }
        refresh_repository_size(state, self.project_id).await;
        let pushes = forgust_db::maintenance::record_push(&state.db, self.project_id).await?;
        if pushes >= PUSHES_PER_MAINTENANCE {
            let mut conn = state.db.acquire().await?;
            state.jobs.enqueue(&mut conn, Maintenance { project_id: self.project_id, full: false }.request()).await?;
        }
        Ok(())
    }
}

/// Measure a repository on disk for the instance's storage view. Best effort.
pub async fn refresh_repository_size(state: &AppState, project_id: i64) {
    match state.storage.disk_size(project_id).await {
        Ok(bytes) => {
            if let Err(e) = forgust_db::admin::set_repository_bytes(&state.db, project_id, bytes as i64).await {
                tracing::warn!(project_id, error = %e, "repository size not recorded");
            }
        }
        Err(e) => tracing::debug!(project_id, error = %e, "repository not measured"),
    }
}

/// Pushes before a repo gets an incremental repack.
const PUSHES_PER_MAINTENANCE: i32 = 20;

impl TaskHandler for PostReceive {
    const IDENTIFIER: &'static str = "post_receive";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        // Only steps that are safe to repeat may fail the job (and retry it);
        // what follows them happens once, so it logs instead.
        crate::services::repos::after_push(state, self.project_id, &self.updates).await.map_err(|e| e.to_string())?;
        crate::services::mr::on_push(state, self.project_id, &self.updates).await.map_err(|e| e.to_string())?;
        // merge trains follow their targets and sources
        if let Err(e) = crate::services::merge_train::on_push(state, self.project_id).await {
            tracing::warn!(project_id = self.project_id, error = %e, "merge trains not refreshed");
        }
        if let Err(e) = self.once(state).await {
            tracing::warn!(project_id = self.project_id, error = %e, "post-receive follow-up failed");
        }
        Ok::<(), String>(())
    }
}

/// `git maintenance` work, serialized per repo (ADR-0004).
#[derive(Debug, Serialize, Deserialize)]
pub struct Maintenance {
    pub project_id: i64,
    pub full: bool,
}

impl Maintenance {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            job_key: Some(format!("maintenance:{}", self.project_id)),
            queue: Some(repo_queue(self.project_id)),
            payload: serde_json::to_value(&self).unwrap_or_default(),
            max_attempts: Some(3),
        }
    }
}

impl TaskHandler for Maintenance {
    const IDENTIFIER: &'static str = "repo_maintenance";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        // exclusive: waits for in-flight pushes, blocks new ones meanwhile
        let lock = state.repo_locks.get(self.project_id);
        let _guard = lock.write().await;
        let started = std::time::Instant::now();
        state.storage.maintenance(self.project_id, self.full).await.map_err(|e| e.to_string())?;
        forgust_db::maintenance::record_maintenance(&state.db, self.project_id).await.map_err(|e| e.to_string())?;
        state.events.publish(Event::Push { project_id: self.project_id, refs: vec![] });
        metrics::histogram!("forgust_repo_maintenance_seconds").record(started.elapsed().as_secs_f64());
        Ok::<(), String>(())
    }
}

/// Hourly: full maintenance for repos pushed to since their last run.
#[derive(Debug, Serialize, Default)]
pub struct MaintenanceSweep;
payload_free!(MaintenanceSweep);

impl TaskHandler for MaintenanceSweep {
    const IDENTIFIER: &'static str = "maintenance_sweep";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        let due = forgust_db::maintenance::due(&state.db, 500).await.map_err(|e| e.to_string())?;
        let mut tx = state.db.begin().await.map_err(|e| e.to_string())?;
        // sizes for the admin storage view: unknown or a day old (a repack changes them)
        state.jobs.enqueue(&mut tx, MeasureRepositories.request()).await.map_err(|e| e.to_string())?;
        for project_id in due {
            state.jobs.enqueue(&mut tx, Maintenance { project_id, full: true }.request()).await.map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
}

/// A backup asked for in Admin > Backups: the database and every repository,
/// into `<data_dir>/backups` (the blob store is backed up with its own tools).
#[derive(Debug, Serialize, Default)]
pub struct Backup;
payload_free!(Backup);

impl Backup {
    pub fn request(self) -> JobRequest {
        JobRequest { task: Self::IDENTIFIER, job_key: Some("backup".into()), max_attempts: Some(1), ..Default::default() }
    }
}

impl TaskHandler for Backup {
    const IDENTIFIER: &'static str = "backup";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        let dir = crate::backup::backup(&state.cfg, state.storage.as_ref(), &state.db, &state.cfg.backups_dir())
            .await
            .map_err(|e| forgust_db::admin::redact_error(&e.to_string()))?;
        tracing::info!(dir = %dir.display(), "backup written");
        Ok::<(), String>(())
    }
}

/// Measure the repositories whose size is unknown or a day old (Admin > Storage).
#[derive(Debug, Serialize, Default)]
pub struct MeasureRepositories;
payload_free!(MeasureRepositories);

impl MeasureRepositories {
    pub fn request(self) -> JobRequest {
        JobRequest { task: Self::IDENTIFIER, job_key: Some("measure_repositories".into()), max_attempts: Some(3), ..Default::default() }
    }
}

impl TaskHandler for MeasureRepositories {
    const IDENTIFIER: &'static str = "measure_repositories";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        for project_id in forgust_db::admin::stale_repository_sizes(&state.db, 1000).await.map_err(|e| e.to_string())? {
            refresh_repository_size(state, project_id).await;
        }
        // then how full each quota is: banners, and an email when one fills up
        crate::services::quotas::measure(state).await.map_err(|e| format!("{e:#}"))?;
        Ok::<(), String>(())
    }
}

/// An administrator asked what the blob store holds (Admin > Storage): list it
/// once, by top-level prefix, with what nothing references, within limits.
#[derive(Debug, Serialize, Default)]
pub struct ScanBlobs;
payload_free!(ScanBlobs);

impl ScanBlobs {
    pub fn request(self) -> JobRequest {
        JobRequest { task: Self::IDENTIFIER, job_key: Some("blob_scan".into()), max_attempts: Some(1), ..Default::default() }
    }
}

/// At most this many objects per scan, for at most this long.
const BLOB_SCAN_MAX_OBJECTS: u64 = 5_000_000;
const BLOB_SCAN_BUDGET: std::time::Duration = std::time::Duration::from_secs(15 * 60);

impl TaskHandler for ScanBlobs {
    const IDENTIFIER: &'static str = "blob_scan";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        let limits = crate::services::blob_scan::Limits {
            max_objects: BLOB_SCAN_MAX_OBJECTS,
            budget: BLOB_SCAN_BUDGET,
            min_age: chrono::Duration::days(1),
        };
        let (r, error) = match crate::services::blob_scan::scan(state, &limits).await {
            Ok(r) => (r, None),
            Err(e) => (
                crate::services::blob_scan::Report { prefixes: Vec::new(), orphans: Vec::new(), complete: false },
                Some(forgust_db::admin::redact_error(&format!("{e:#}"))),
            ),
        };
        forgust_db::admin::finish_blob_scan(&state.db, &r.prefixes, &r.orphans, r.complete, error.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
}

/// Every few minutes: expired sessions, tokens and memberships, idle git processes.
#[derive(Debug, Serialize, Default)]
pub struct Cleanup;
payload_free!(Cleanup);

impl TaskHandler for Cleanup {
    const IDENTIFIER: &'static str = "cleanup";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        forgust_db::sessions::delete_expired(&state.db).await.map_err(|e| e.to_string())?;
        // memberships and shares that expired: their access ends now
        let mut conn = state.db.acquire().await.map_err(|e| e.to_string())?;
        let ended = forgust_db::members::expire(&mut conn).await.map_err(|e| e.to_string())?;
        if !ended.is_empty() {
            tracing::info!(users = ended.len(), "expired memberships removed");
        }
        state.storage.reap_idle();
        metrics::gauge!("forgust_catfile_processes").set(state.storage.catfile_processes() as f64);
        Ok::<(), String>(())
    }
}

/// Start the downstream pipeline of a `trigger` job, out of the
/// upstream pipeline's processing.
#[derive(Debug, Serialize, Deserialize)]
pub struct StartBridge {
    pub build_id: i64,
}

impl StartBridge {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            payload: serde_json::to_value(&self).unwrap_or_default(),
            queue: None,
            job_key: Some(format!("bridge-{}", self.build_id)),
            max_attempts: Some(3),
        }
    }
}

impl TaskHandler for StartBridge {
    const IDENTIFIER: &'static str = "start_bridge";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        crate::ci::bridge::start(state, self.build_id).await.map_err(|e| format!("{e:#}"))?;
        Ok::<(), String>(())
    }
}

/// Deliver one webhook; retries with graphile_worker's backoff.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeliverWebhook {
    pub delivery_id: i64,
}

impl DeliverWebhook {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            payload: serde_json::to_value(&self).unwrap_or_default(),
            queue: None,
            job_key: None,
            max_attempts: Some(crate::services::webhooks::MAX_ATTEMPTS),
        }
    }
}

impl TaskHandler for DeliverWebhook {
    const IDENTIFIER: &'static str = "deliver_webhook";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        crate::services::webhooks::deliver(state, self.delivery_id).await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
}

/// Read a finished job's JUnit report (M7).
#[derive(Debug, Serialize, Deserialize)]
pub struct ParseTestReport {
    pub build_id: i64,
}

impl ParseTestReport {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            job_key: Some(format!("test-report:{}", self.build_id)),
            payload: serde_json::to_value(&self).unwrap_or_default(),
            queue: None,
            max_attempts: Some(3),
        }
    }
}

impl TaskHandler for ParseTestReport {
    const IDENTIFIER: &'static str = "parse_test_report";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        crate::ci::reports::store_test_report(state, self.build_id).await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
}

/// Read a finished job's security reports: one project at a time,
/// in order, before its pipeline's resolution.
#[derive(Debug, Serialize, Deserialize)]
pub struct ReadSecurityReports {
    pub build_id: i64,
    pub project_id: i64,
}

impl ReadSecurityReports {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            job_key: Some(format!("security-read:{}", self.build_id)),
            payload: serde_json::to_value(&self).unwrap_or_default(),
            queue: Some(format!("security:{}", self.project_id)),
            max_attempts: Some(3),
        }
    }
}

impl TaskHandler for ReadSecurityReports {
    const IDENTIFIER: &'static str = "read_security_reports";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        crate::services::security::ingest(state, self.build_id).await.map_err(|e| format!("{e:#}"))?;
        Ok::<(), String>(())
    }
}

/// A pipeline ended: resolve what its complete default-branch scans no longer find.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResolveSecurityFindings {
    pub pipeline_id: i64,
    pub project_id: i64,
}

impl ResolveSecurityFindings {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            job_key: Some(format!("security-resolve:{}", self.pipeline_id)),
            payload: serde_json::to_value(&self).unwrap_or_default(),
            queue: Some(format!("security:{}", self.project_id)),
            max_attempts: Some(3),
        }
    }
}

impl TaskHandler for ResolveSecurityFindings {
    const IDENTIFIER: &'static str = "resolve_security_findings";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        crate::services::security::resolve(state, self.pipeline_id).await.map_err(|e| format!("{e:#}"))?;
        Ok::<(), String>(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendEmail {
    pub email: crate::services::notify::Email,
}

impl SendEmail {
    pub fn request(self) -> JobRequest {
        JobRequest {
            task: Self::IDENTIFIER,
            payload: serde_json::to_value(&self).unwrap_or_default(),
            queue: None,
            job_key: None,
            max_attempts: Some(5),
        }
    }
}

impl TaskHandler for SendEmail {
    const IDENTIFIER: &'static str = "send_email";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        state.mailer.send(&self.email).await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
}

/// Spike for M0: a no-op job used by tests to prove transactional enqueue.
#[derive(Debug, Serialize, Deserialize)]
pub struct Ping {
    pub marker: String,
}

impl TaskHandler for Ping {
    const IDENTIFIER: &'static str = "ping";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        if let Some(state) = ctx.get_ext::<AppState>() {
            state.events.publish(Event::RepoReset { project_id: -1 });
        }
        tracing::info!(marker = %self.marker, "ping job ran");
        Ok::<(), String>(())
    }
}

/// CI housekeeping: silent jobs, log archiving, expired artifacts (every minute).
#[derive(Debug, Serialize, Default)]
pub struct CiSweep;
payload_free!(CiSweep);

impl TaskHandler for CiSweep {
    const IDENTIFIER: &'static str = "ci_sweep";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        crate::ci::sweep(state).await.map_err(|e| e.to_string())?;
        crate::services::auto_merge::sweep(state).await.map_err(|e| e.to_string())
    }
}

/// Ask GitHub for the latest Forgust release (every 6 hours, at start, when
/// turned on, or from Admin > Jobs; unless turned off).
#[derive(Debug, Serialize)]
pub struct UpdateCheck {
    /// Fail when the check does. Scheduled runs say `false`: an offline instance
    /// would fill the failed jobs, and the next run tries again. Admin > Jobs
    /// queues no payload, so an administrator's run shows why it failed.
    pub report: bool,
}

impl<'de> Deserialize<'de> for UpdateCheck {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let payload = serde_json::Value::deserialize(d)?;
        Ok(UpdateCheck { report: payload.get("report").and_then(|r| r.as_bool()).unwrap_or(true) })
    }
}

impl TaskHandler for UpdateCheck {
    const IDENTIFIER: &'static str = "update_check";

    async fn run(self, ctx: WorkerContext) -> impl IntoTaskHandlerResult {
        let state = ctx.get_ext::<AppState>().ok_or("no app state")?;
        match crate::services::updates::check(state).await {
            Err(e) if self.report => Err(format!("release check failed: {e:#}")),
            Err(e) => {
                tracing::warn!(error = %e, "release check failed");
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }
}

/// Start the worker (role `worker`). Also installs the graphile schema.
#[allow(deprecated)]
pub async fn start_worker(state: AppState, concurrency: usize) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let check_state = state.clone();
    let worker = WorkerOptions::default()
        .concurrency(concurrency)
        .schema(SCHEMA)
        .pg_pool(state.db.clone())
        .define_job::<PostReceive>()
        .define_job::<Maintenance>()
        .define_job::<MaintenanceSweep>()
        .define_job::<Cleanup>()
        .define_job::<ScanBlobs>()
        .define_job::<MeasureRepositories>()
        .define_job::<ReadSecurityReports>()
        .define_job::<ResolveSecurityFindings>()
        .define_job::<Backup>()
        .define_job::<Ping>()
        .define_job::<ParseTestReport>()
        .define_job::<DeliverWebhook>()
        .define_job::<StartBridge>()
        .define_job::<SendEmail>()
        .define_job::<CiSweep>()
        .define_job::<UpdateCheck>()
        .define_job::<crate::services::auto_merge::AutoMergeCheck>()
        .define_job::<crate::import::ImportJob>()
        .with_crontab("*/5 * * * * cleanup\n17 * * * * maintenance_sweep\n* * * * * ci_sweep\n43 */6 * * * update_check {report:false}")?
        .listen_os_shutdown_signals(false)
        // a worker that stops answering (a restart, a crash) gives its jobs back
        // within minutes: else they stay locked to it for hours, and so does
        // their queue (an import's projects run in one)
        .worker_recovery(graphile_worker::WorkerRecoveryConfig::default().enabled(true))
        .add_extension(state)
        .init()
        .await?;
    // a fresh install, or one that was down, hears of a release now rather than in up to 6 hours
    if let Err(e) = crate::services::updates::queue_check(&check_state).await {
        tracing::warn!(error = %e, "could not queue the release check");
    }
    Ok(tokio::spawn(async move {
        if let Err(e) = worker.run().await {
            tracing::error!(error = %e, "job worker stopped");
        }
    }))
}

/// Install the graphile_worker schema without running jobs (API-only processes).
pub async fn migrate(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let _ = WorkerOptions::default().schema(SCHEMA).pg_pool(pool.clone()).define_job::<Ping>().init().await?;
    Ok(())
}

#[cfg(test)]
mod payload_tests {
    use super::*;

    #[test]
    fn tasks_without_arguments_take_any_payload() {
        for payload in [serde_json::Value::Null, serde_json::json!({}), serde_json::json!({ "_cron": { "ts": "x", "backfilled": false } })]
        {
            serde_json::from_value::<CiSweep>(payload.clone()).unwrap();
            serde_json::from_value::<Cleanup>(payload.clone()).unwrap();
            serde_json::from_value::<MaintenanceSweep>(payload.clone()).unwrap();
            serde_json::from_value::<MeasureRepositories>(payload.clone()).unwrap();
            serde_json::from_value::<ScanBlobs>(payload.clone()).unwrap();
            serde_json::from_value::<Backup>(payload).unwrap();
        }
    }

    #[test]
    fn only_runs_asked_for_report_a_failed_release_check() {
        // Admin > Jobs queues null, which the queue stores as `{}`
        assert!(serde_json::from_value::<UpdateCheck>(serde_json::Value::Null).unwrap().report);
        assert!(serde_json::from_value::<UpdateCheck>(serde_json::json!({})).unwrap().report);
        // the crontab line and `queue_check` say so
        let cron = serde_json::json!({ "report": false, "_cron": { "ts": "x", "backfilled": false } });
        assert!(!serde_json::from_value::<UpdateCheck>(cron).unwrap().report);
        assert!(!serde_json::from_value::<UpdateCheck>(serde_json::json!({ "report": false })).unwrap().report);
    }
}
