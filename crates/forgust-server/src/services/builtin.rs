//! What built-in Renovate (ADR-0023) and built-in security scanning
//! (ADR-0024) share: features Forgust runs as its own pipelines, per project,
//! on a schedule, that a group can turn on for its projects. Each says how
//! (its settings, its runs); the scheduling and the group defaults are here.

use std::future::Future;

use chrono::{Duration, Utc};
use forgust_db::namespaces::Namespace;
use forgust_db::projects::Project;
use serde_json::json;

use crate::auth::Principal;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(crate) trait Builtin {
    /// What people call it ("dependency updates").
    const FEATURE: &'static str;
    /// One run, in logs ("renovate run").
    const RUN: &'static str;
    /// The audit action of a group's default.
    const GROUP_AUDIT: &'static str;

    /// `(max runs at once, a run's timeout in seconds)`, or `None` when the
    /// instance has it off.
    fn limits(state: &AppState) -> impl Future<Output = anyhow::Result<Option<(i64, f64)>>> + Send;
    /// Runs still unfinished after `secs`.
    fn stale(state: &AppState, secs: f64) -> impl Future<Output = anyhow::Result<Vec<i64>>> + Send;
    /// How many runs go on now, instance-wide.
    fn active(state: &AppState) -> impl Future<Output = anyhow::Result<i64>> + Send;
    /// Up to `n` projects whose run is due.
    fn due(state: &AppState, n: i64) -> impl Future<Output = anyhow::Result<Vec<i64>>> + Send;
    fn run(state: &AppState, project: &Project) -> impl Future<Output = ApiResult<Option<i64>>> + Send;
    /// Not started (a run going on, a failure): due again later.
    fn later(state: &AppState, project_id: i64, at: chrono::DateTime<Utc>) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn set_group(state: &AppState, namespace_id: i64, enabled: bool) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// Projects under a group that never chose.
    fn undecided_under(state: &AppState, namespace_id: i64) -> impl Future<Output = anyhow::Result<Vec<i64>>> + Send;
    /// Whether a group above (`traversal_ids`) turned it on for its projects.
    fn inherited(state: &AppState, traversal_ids: &[i64]) -> impl Future<Output = anyhow::Result<bool>> + Send;
    /// Turn it on for a project, with the defaults.
    fn turn_on(state: &AppState, principal: &Principal, project: &Project) -> impl Future<Output = ApiResult<()>> + Send;
}

/// Every minute: cancel what no runner took within a day, then start due runs
/// while there is room.
pub(crate) async fn run_due<B: Builtin>(state: &AppState) -> anyhow::Result<()> {
    let Some((max_concurrent, timeout)) = B::limits(state).await? else { return Ok(()) };
    for id in B::stale(state, timeout + 86_400.0).await? {
        tracing::warn!(pipeline = id, "{} never finished; canceled (no runner with the configured tags?)", B::RUN);
        crate::ci::pipeline::cancel(state, id).await?;
    }
    let free = max_concurrent - B::active(state).await?;
    if free <= 0 {
        return Ok(());
    }
    for project_id in B::due(state, free).await? {
        let Some(project) = forgust_db::projects::find(&state.db, project_id).await? else { continue };
        if let Err(e) = B::run(state, &project).await {
            // a run still going, or a failure: try again in an hour, not every minute
            tracing::info!(project_id, error = %e, "{} not started", B::RUN);
            B::later(state, project_id, Utc::now() + Duration::hours(1)).await?;
        }
    }
    Ok(())
}

/// A group's default: on, every project under it that never chose gets it.
pub(crate) async fn set_group_default<B: Builtin>(
    state: &AppState,
    principal: &Principal,
    ns: &Namespace,
    enabled: bool,
) -> ApiResult<usize> {
    if enabled && B::limits(state).await.map_err(ApiError::Internal)?.is_none() {
        return Err(ApiError::conflict(format!("{} is turned off on this instance", B::FEATURE)));
    }
    B::set_group(state, ns.id, enabled).await.map_err(ApiError::Internal)?;
    let mut n = 0;
    if enabled {
        for id in B::undecided_under(state, ns.id).await.map_err(ApiError::Internal)? {
            let Some(p) = forgust_db::projects::find(&state.db, id).await? else { continue };
            B::turn_on(state, principal, &p).await?;
            n += 1;
        }
    }
    super::audit(state, principal, B::GROUP_AUDIT, Some(("namespace", ns.id)), json!({ "enabled": enabled, "projects": n }), None).await;
    Ok(n)
}

/// A new project under a group that turned it on gets it.
pub(crate) async fn on_project_created<B: Builtin>(state: &AppState, principal: &Principal, project: &Project) {
    if B::inherited(state, &project.traversal_ids).await.unwrap_or(false)
        && let Err(e) = B::turn_on(state, principal, project).await
    {
        tracing::warn!(project_id = project.id, error = ?e, "{} not turned on for a new project", B::FEATURE);
    }
}
