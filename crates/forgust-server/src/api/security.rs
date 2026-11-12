//! Security findings: the project's Security tab, a finding's
//! history, dismissing, and what a merge request finds. Developers and above
//! (as GitLab's vulnerability report).

use axum::Json;
use axum::extract::{Path, Query, State};
use forgust_db::projects::Project;
use forgust_db::security::{self as sdb, Finding, FindingEvent, SecurityCounts};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::{ProjectAction, project_role};
use crate::auth::{Principal, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services;
use crate::state::AppState;

/// The project, for a Developer (or above) of it.
async fn developer_of(state: &AppState, v: &Principal, id: &str) -> ApiResult<Project> {
    let p = project_for(state, v, id, ProjectAction::ReadCode).await?;
    let role = project_role(state, v, &p).await?;
    if !(role.bypass || role.level >= forgust_db::access::DEVELOPER) {
        return Err(ApiError::forbidden("security findings are for Developers and above"));
    }
    Ok(p)
}

#[derive(Deserialize, Default)]
pub struct FindingsQuery {
    #[serde(default)]
    state: Option<String>,
    severity: Option<String>,
    report_type: Option<String>,
    #[serde(default)]
    search: String,
    page: Option<i64>,
    per_page: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct SecurityOverview {
    pub counts: SecurityCounts,
    pub findings: Vec<Finding>,
    /// The default branch's newest pipeline that read security reports.
    pub last_scan_pipeline_iid: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/security/findings", responses((status = 200, body = SecurityOverview)), tag = "security")]
pub async fn findings(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<FindingsQuery>,
) -> ApiResult<Json<SecurityOverview>> {
    let p = developer_of(&state, &v, &id).await?;
    let st = q.state.as_deref().unwrap_or("detected");
    if !["detected", "dismissed", "resolved", "all"].contains(&st) {
        return Err(ApiError::bad("state: detected, dismissed, resolved or all"));
    }
    let (per, offset) = super::page_window(q.page.unwrap_or(1), q.per_page.unwrap_or(50));
    let list = sdb::list(
        &state.db,
        p.id,
        &sdb::ListQuery {
            state: st,
            severity: q.severity.as_deref().filter(|s| !s.is_empty()),
            report_type: q.report_type.as_deref().filter(|s| !s.is_empty()),
            search: q.search.trim(),
            limit: per,
            offset,
        },
    )
    .await?;
    let last = match &p.default_branch {
        Some(b) => match sdb::latest_default_scan(&state.db, p.id, b).await? {
            Some(pl) => forgust_db::ci::pipeline(&state.db, pl).await?.map(|x| x.iid),
            None => None,
        },
        None => None,
    };
    Ok(Json(SecurityOverview { counts: sdb::counts(&state.db, p.id).await?, findings: list, last_scan_pipeline_iid: last }))
}

#[derive(Serialize, ToSchema)]
pub struct FindingDetail {
    #[serde(flatten)]
    pub finding: Finding,
    pub events: Vec<FindingEvent>,
    /// Renovate's merge requests updating its package (dependencies).
    pub fix_merge_requests: Vec<sdb::FixMergeRequest>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/security/findings/{finding_id}", responses((status = 200, body = FindingDetail)), tag = "security")]
pub async fn finding(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, fid)): Path<(String, i64)>,
) -> ApiResult<Json<FindingDetail>> {
    let p = developer_of(&state, &v, &id).await?;
    let f = sdb::find(&state.db, p.id, fid).await?.ok_or(ApiError::NotFound)?;
    let fixes = match (&f.package_name, f.report_type.as_str()) {
        (Some(pkg), "dependency_scanning") => sdb::fix_merge_requests(&state.db, p.id, pkg).await?,
        _ => vec![],
    };
    Ok(Json(FindingDetail { events: sdb::events(&state.db, f.id).await?, fix_merge_requests: fixes, finding: f }))
}

#[derive(Deserialize, ToSchema)]
pub struct DismissBody {
    /// `false_positive`, `acceptable_risk`, `used_in_tests`, `mitigating_control` or `not_applicable`.
    pub reason: String,
    #[serde(default)]
    pub comment: String,
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/security/findings/{finding_id}/dismiss", request_body = DismissBody, responses((status = 200, body = FindingDetail)), tag = "security")]
pub async fn dismiss(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, fid)): Path<(String, i64)>,
    Json(req): Json<DismissBody>,
) -> ApiResult<Json<FindingDetail>> {
    let p = developer_of(&state, &v, &id).await?;
    crate::auth::authz::writable(&p)?;
    let user = v.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    if !["false_positive", "acceptable_risk", "used_in_tests", "mitigating_control", "not_applicable"].contains(&req.reason.as_str()) {
        return Err(ApiError::bad("reason: false_positive, acceptable_risk, used_in_tests, mitigating_control or not_applicable"));
    }
    let comment = req.comment.trim();
    if comment.len() > 10_000 {
        return Err(ApiError::bad("the comment is too long"));
    }
    let f = sdb::find(&state.db, p.id, fid).await?.ok_or(ApiError::NotFound)?;
    if f.state == "dismissed" {
        return Err(ApiError::conflict("already dismissed"));
    }
    sdb::set_dismissed(&state.db, f.id, Some((&req.reason, comment, user.id))).await?;
    let note = if comment.is_empty() { req.reason.clone() } else { format!("{}: {comment}", req.reason) };
    sdb::event(&state.db, f.id, "dismissed", Some(user.id), None, Some(&note)).await?;
    services::audit(
        &state,
        &v,
        "security_finding_dismissed",
        Some(("project", p.id)),
        json!({ "finding": f.id, "reason": req.reason }),
        None,
    )
    .await;
    finding(State(state), Viewer(v), Path((id, fid))).await
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/security/findings/{finding_id}/reopen", responses((status = 200, body = FindingDetail)), tag = "security")]
pub async fn reopen(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, fid)): Path<(String, i64)>,
) -> ApiResult<Json<FindingDetail>> {
    let p = developer_of(&state, &v, &id).await?;
    crate::auth::authz::writable(&p)?;
    let user = v.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    let f = sdb::find(&state.db, p.id, fid).await?.ok_or(ApiError::NotFound)?;
    if f.state != "dismissed" {
        return Err(ApiError::conflict("only a dismissed finding can be reopened"));
    }
    sdb::set_dismissed(&state.db, f.id, None).await?;
    sdb::event(&state.db, f.id, "reopened", Some(user.id), None, None).await?;
    services::audit(&state, &v, "security_finding_reopened", Some(("project", p.id)), json!({ "finding": f.id }), None).await;
    finding(State(state), Viewer(v), Path((id, fid))).await
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/security", responses((status = 200, body = services::security::MrSecurity)), tag = "security")]
pub async fn merge_request(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<services::security::MrSecurity>> {
    let p = developer_of(&state, &v, &id).await?;
    let m = forgust_db::merge_requests::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(services::security::for_merge_request(&state, &p, &m).await.map_err(ApiError::Internal)?))
}

// ------------------------------------------------------------ built-in scanners

#[derive(Serialize, ToSchema)]
pub struct ScanningStatus {
    /// The instance runs scanners at all.
    pub instance_enabled: bool,
    pub enabled: bool,
    pub scanners: Vec<String>,
    /// What a project may turn on.
    pub available: Vec<String>,
    pub interval_hours: i32,
    pub next_run_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The runs are this person's (they turned scanning on).
    pub run_as: Option<String>,
    pub last_pipeline_iid: Option<i64>,
    pub last_status: Option<String>,
}

async fn scanning_status(state: &AppState, p: &Project) -> ApiResult<ScanningStatus> {
    let cfg = services::scanning::config(state).await.map_err(ApiError::Internal)?;
    let s = sdb::scanning(&state.db, p.id).await?;
    let last = match s.as_ref().and_then(|s| s.last_pipeline_id) {
        Some(id) => forgust_db::ci::pipeline(&state.db, id).await?,
        None => None,
    };
    let run_as = match s.as_ref().and_then(|s| s.user_id) {
        Some(u) => forgust_db::users::find(&state.db, u).await?.map(|u| u.username),
        None => None,
    };
    Ok(ScanningStatus {
        instance_enabled: cfg.enabled,
        enabled: s.as_ref().is_some_and(|s| s.enabled),
        scanners: s
            .as_ref()
            .map(|s| s.scanners.clone())
            .unwrap_or_else(|| services::scanning::DEFAULT_SCANNERS.iter().map(|x| x.to_string()).collect()),
        available: services::scanning::SCANNERS.iter().map(|x| x.to_string()).collect(),
        interval_hours: s.as_ref().map_or(24, |s| s.interval_hours),
        next_run_at: s.as_ref().and_then(|s| s.next_run_at),
        run_as,
        last_pipeline_iid: last.as_ref().map(|l| l.iid),
        last_status: last.map(|l| l.status),
    })
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/security/scanning", responses((status = 200, body = ScanningStatus)), tag = "security")]
pub async fn scanning(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<ScanningStatus>> {
    let p = developer_of(&state, &v, &id).await?;
    Ok(Json(scanning_status(&state, &p).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct ScanningBody {
    pub enabled: bool,
    pub scanners: Vec<String>,
    #[serde(default = "day")]
    pub interval_hours: i32,
}

fn day() -> i32 {
    24
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/security/scanning", request_body = ScanningBody, responses((status = 200, body = ScanningStatus)), tag = "security")]
pub async fn set_scanning(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Json(b): Json<ScanningBody>,
) -> ApiResult<Json<ScanningStatus>> {
    let p = project_for(&state, &v, &id, ProjectAction::Admin).await?;
    services::scanning::configure(
        &state,
        &v,
        &p,
        services::scanning::Change { enabled: b.enabled, scanners: b.scanners, interval_hours: b.interval_hours },
    )
    .await?;
    Ok(Json(scanning_status(&state, &p).await?))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/security/scanning/run", responses((status = 200, body = ScanningStatus)), tag = "security")]
pub async fn run_scan(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<ScanningStatus>> {
    let p = project_for(&state, &v, &id, ProjectAction::Admin).await?;
    services::scanning::run(&state, &p).await?;
    Ok(Json(scanning_status(&state, &p).await?))
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct GroupScanning {
    /// Projects below that never chose get scanning, and new ones.
    pub enabled: bool,
}

#[utoipa::path(get, path = "/api/v1/groups/{id}/security/scanning", responses((status = 200, body = GroupScanning)), tag = "security")]
pub async fn group_scanning(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<GroupScanning>> {
    let ns = super::group_for(&state, &v, &id, crate::auth::authz::GroupAction::Admin).await?;
    Ok(Json(GroupScanning { enabled: sdb::scanning_group_default(&state.db, ns.id).await? }))
}

#[utoipa::path(put, path = "/api/v1/groups/{id}/security/scanning", request_body = GroupScanning, responses((status = 200, body = Object)), tag = "security")]
pub async fn set_group_scanning(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Json(b): Json<GroupScanning>,
) -> ApiResult<Json<serde_json::Value>> {
    let ns = super::group_for(&state, &v, &id, crate::auth::authz::GroupAction::Admin).await?;
    let n = services::scanning::set_group_default(&state, &v, &ns, b.enabled).await?;
    Ok(Json(json!({ "enabled": b.enabled, "projects_turned_on": n })))
}

#[derive(Serialize, ToSchema)]
pub struct AdminScanning {
    pub settings: crate::config::SecurityScanningConfig,
    /// Projects with scanning on.
    pub projects: i64,
    /// Scans not finished.
    pub running: usize,
    /// Projects with open critical or high findings, most first.
    pub exposed: Vec<sdb::ExposedProject>,
}

#[utoipa::path(get, path = "/api/v1/admin/security/scanning", responses((status = 200, body = AdminScanning)), tag = "security")]
pub async fn admin_scanning(State(state): State<AppState>, crate::auth::Admin(..): crate::auth::Admin) -> ApiResult<Json<AdminScanning>> {
    Ok(Json(AdminScanning {
        settings: services::scanning::config(&state).await.map_err(ApiError::Internal)?,
        projects: sdb::scanning_count(&state.db).await?,
        running: sdb::active_scans(&state.db, None).await?.len(),
        exposed: sdb::exposed_projects(&state.db, 50).await?,
    }))
}

#[utoipa::path(put, path = "/api/v1/admin/security/scanning", request_body = crate::config::SecurityScanningConfig, responses((status = 200, body = AdminScanning)), tag = "security")]
pub async fn set_admin_scanning(
    State(state): State<AppState>,
    crate::auth::Admin(me, p): crate::auth::Admin,
    Json(b): Json<crate::config::SecurityScanningConfig>,
) -> ApiResult<Json<AdminScanning>> {
    for img in [&b.sast_image, &b.secrets_image, &b.trivy_image] {
        if img.trim().is_empty() || img.contains(char::is_whitespace) {
            return Err(ApiError::bad("an image is a name like registry/name:tag"));
        }
    }
    if !(1..=50).contains(&b.max_concurrent) || !(60..=86_400).contains(&b.timeout_secs) {
        return Err(ApiError::bad("1 to 50 scans at once, a timeout of 1 minute to a day"));
    }
    sdb::set_scanning_overrides(&state.db, &serde_json::to_value(&b).map_err(|e| ApiError::Internal(e.into()))?).await?;
    services::audit(&state, &p, "security_scanning_settings", None, json!({ "by": me.username }), None).await;
    admin_scanning(State(state), crate::auth::Admin(me, p)).await
}
