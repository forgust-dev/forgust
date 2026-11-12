//! Security findings (ADR-0024): one row per fingerprint per project,
//! sightings per pipeline, the reports read, and each finding's history.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;
use crate::ci::ACTIVE;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Finding {
    pub id: i64,
    pub project_id: i64,
    pub report_type: String,
    pub scanner: String,
    pub severity: String,
    pub title: String,
    pub description: String,
    pub solution: String,
    pub rule_id: String,
    #[schema(value_type = Vec<SecurityIdentifier>)]
    pub identifiers: serde_json::Value,
    pub file: Option<String>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub package_name: Option<String>,
    pub package_version: Option<String>,
    pub fixed_version: Option<String>,
    pub image: Option<String>,
    /// `detected`, `dismissed` or `resolved`.
    pub state: String,
    /// Seen by the default branch's newest scan.
    pub on_default: bool,
    pub dismissal_reason: Option<String>,
    pub dismissal_comment: Option<String>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// The newest pipeline that saw it (its iid).
    pub last_pipeline_iid: Option<i64>,
}

/// A finding's identifier (its rule, a CVE, a CWE...), as stored.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SecurityIdentifier {
    /// `cve`, `cwe`, `ghsa`, `owasp`, or the scanner's rule type.
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub value: String,
    pub url: Option<String>,
}

pub struct NewFinding<'a> {
    pub project_id: i64,
    pub pipeline_id: i64,
    pub fingerprint: &'a str,
    pub report_type: &'a str,
    pub scanner: &'a str,
    pub severity: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    pub solution: &'a str,
    pub rule_id: &'a str,
    pub identifiers: &'a serde_json::Value,
    pub file: Option<&'a str>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub package_name: Option<&'a str>,
    pub package_version: Option<&'a str>,
    pub fixed_version: Option<&'a str>,
    pub image: Option<&'a str>,
}

/// Record a finding a pipeline's report holds (its details are the newest
/// report's) and that the pipeline saw it. Returns `(id, state, on_default,
/// new)`, the state and flag as they were before.
pub async fn upsert(c: &mut PgConnection, f: &NewFinding<'_>) -> Result<(i64, String, bool, bool)> {
    let r = sqlx::query!(
        r#"WITH prev AS (SELECT state, on_default FROM security_findings WHERE project_id = $1 AND fingerprint = $2)
           INSERT INTO security_findings (project_id, fingerprint, report_type, scanner, severity, title, description, solution, rule_id,
                                          identifiers, file, start_line, end_line, package_name, package_version, fixed_version, image,
                                          first_pipeline_id, last_pipeline_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $18)
           ON CONFLICT (project_id, fingerprint) DO UPDATE SET
               scanner = EXCLUDED.scanner, severity = EXCLUDED.severity, title = EXCLUDED.title, description = EXCLUDED.description,
               solution = EXCLUDED.solution, identifiers = EXCLUDED.identifiers, file = EXCLUDED.file, start_line = EXCLUDED.start_line,
               end_line = EXCLUDED.end_line, package_version = EXCLUDED.package_version, fixed_version = EXCLUDED.fixed_version,
               image = EXCLUDED.image, last_pipeline_id = EXCLUDED.last_pipeline_id, last_seen_at = now()
           RETURNING id, (SELECT state FROM prev) AS prev_state, (SELECT on_default FROM prev) AS prev_default"#,
        f.project_id,
        f.fingerprint,
        f.report_type,
        f.scanner,
        f.severity,
        f.title,
        f.description,
        f.solution,
        f.rule_id,
        f.identifiers,
        f.file,
        f.start_line,
        f.end_line,
        f.package_name,
        f.package_version,
        f.fixed_version,
        f.image,
        f.pipeline_id
    )
    .fetch_one(&mut *c)
    .await?;
    sqlx::query!(
        "INSERT INTO security_finding_sightings (finding_id, pipeline_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        r.id,
        f.pipeline_id
    )
    .execute(&mut *c)
    .await?;
    let new = r.prev_state.is_none();
    Ok((r.id, r.prev_state.unwrap_or_else(|| "detected".into()), r.prev_default.unwrap_or(false), new))
}

/// The default branch sees it now: back to `detected` if it was resolved.
pub async fn seen_on_default(c: &mut PgConnection, id: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE security_findings SET on_default = true,
                state = CASE WHEN state = 'resolved' THEN 'detected' ELSE state END,
                resolved_at = CASE WHEN state = 'resolved' THEN NULL ELSE resolved_at END
          WHERE id = $1",
        id
    )
    .execute(c)
    .await?;
    Ok(())
}

pub async fn event(
    e: impl PgExecutor<'_>,
    finding_id: i64,
    action: &str,
    user_id: Option<i64>,
    pipeline_id: Option<i64>,
    note: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO security_finding_events (finding_id, action, user_id, pipeline_id, note) VALUES ($1, $2, $3, $4, $5)",
        finding_id,
        action,
        user_id,
        pipeline_id,
        note
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn record_scan(
    e: impl PgExecutor<'_>,
    build_id: i64,
    pipeline_id: i64,
    report_type: &str,
    scanner: &str,
    findings: i32,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO security_scans (build_id, pipeline_id, report_type, scanner, findings, error) VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (build_id, report_type) DO UPDATE SET scanner = $4, findings = $5, error = $6, created_at = now()",
        build_id,
        pipeline_id,
        report_type,
        scanner,
        findings,
        error
    )
    .execute(e)
    .await?;
    Ok(())
}

/// The report types a pipeline scanned completely: every job that uploaded
/// such a report passed and was read without error.
pub async fn complete_report_types(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Vec<String>> {
    sqlx::query_scalar!(
        r#"SELECT s.report_type AS "t!" FROM security_scans s
             JOIN ci_builds b ON b.id = s.build_id
            WHERE s.pipeline_id = $1 AND NOT b.retried
            GROUP BY s.report_type
           HAVING bool_and(s.error IS NULL AND b.status = 'success')"#,
        pipeline_id
    )
    .fetch_all(e)
    .await
}

/// Whether a newer default-branch pipeline already scanned this report type
/// (an older pipeline ending late must not resolve what it no longer sees).
pub async fn newer_scan_exists(
    e: impl PgExecutor<'_>,
    project_id: i64,
    pipeline_id: i64,
    report_type: &str,
    default_branch: &str,
) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM security_scans s JOIN ci_pipelines p ON p.id = s.pipeline_id
                           WHERE p.project_id = $1 AND p.id > $2 AND s.report_type = $3 AND p.ref = $4 AND NOT p.tag
                             AND p.merge_request_id IS NULL) AS "x!""#,
        project_id,
        pipeline_id,
        report_type,
        default_branch
    )
    .fetch_one(e)
    .await
}

/// A complete default-branch scan no longer finds them: detected ones are
/// resolved, dismissed ones stay dismissed; neither is on the default branch
/// any more. Returns the ids that were resolved.
pub async fn resolve_missing(c: &mut PgConnection, project_id: i64, pipeline_id: i64, report_type: &str) -> Result<Vec<i64>> {
    let gone = sqlx::query!(
        r#"UPDATE security_findings f SET on_default = false,
                  state = CASE WHEN f.state = 'detected' THEN 'resolved' ELSE f.state END,
                  resolved_at = CASE WHEN f.state = 'detected' THEN now() ELSE f.resolved_at END
            WHERE f.project_id = $1 AND f.report_type = $3 AND f.on_default
              AND NOT EXISTS (SELECT 1 FROM security_finding_sightings s WHERE s.finding_id = f.id AND s.pipeline_id = $2)
           RETURNING f.id, (f.state = 'resolved') AS "resolved!""#,
        project_id,
        pipeline_id,
        report_type
    )
    .fetch_all(&mut *c)
    .await?;
    Ok(gone.into_iter().filter(|r| r.resolved).map(|r| r.id).collect())
}

pub struct ListQuery<'a> {
    /// `detected`, `dismissed`, `resolved`, or `all`.
    pub state: &'a str,
    pub severity: Option<&'a str>,
    pub report_type: Option<&'a str>,
    pub search: &'a str,
    pub limit: i64,
    pub offset: i64,
}

/// The project's findings: on the default branch (detected or dismissed), or
/// resolved; most severe first.
pub async fn list(e: impl PgExecutor<'_>, project_id: i64, q: &ListQuery<'_>) -> Result<Vec<Finding>> {
    sqlx::query_as!(
        Finding,
        r#"SELECT f.id, f.project_id, f.report_type, f.scanner, f.severity, f.title, f.description, f.solution, f.rule_id,
                  f.identifiers, f.file, f.start_line, f.end_line, f.package_name, f.package_version, f.fixed_version, f.image,
                  f.state, f.on_default, f.dismissal_reason, f.dismissal_comment, u.username AS "dismissed_by?", f.dismissed_at,
                  f.first_seen_at, f.last_seen_at, f.resolved_at, lp.iid AS "last_pipeline_iid?"
             FROM security_findings f
             LEFT JOIN users u ON u.id = f.dismissed_by
             LEFT JOIN ci_pipelines lp ON lp.id = f.last_pipeline_id
            WHERE f.project_id = $1
              AND (f.on_default OR f.state = 'resolved')
              AND ($2 = 'all' OR f.state = $2)
              AND ($3::text IS NULL OR f.severity = $3)
              AND ($4::text IS NULL OR f.report_type = $4)
              AND ($5 = '' OR f.title ILIKE '%' || $5 || '%' OR f.file ILIKE '%' || $5 || '%' OR f.package_name ILIKE '%' || $5 || '%'
                   OR f.rule_id ILIKE '%' || $5 || '%')
            ORDER BY array_position(ARRAY['critical', 'high', 'medium', 'low', 'info', 'unknown'], f.severity), f.last_seen_at DESC, f.id
            LIMIT $6 OFFSET $7"#,
        project_id,
        q.state,
        q.severity,
        q.report_type,
        q.search,
        q.limit,
        q.offset
    )
    .fetch_all(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<Option<Finding>> {
    sqlx::query_as!(
        Finding,
        r#"SELECT f.id, f.project_id, f.report_type, f.scanner, f.severity, f.title, f.description, f.solution, f.rule_id,
                  f.identifiers, f.file, f.start_line, f.end_line, f.package_name, f.package_version, f.fixed_version, f.image,
                  f.state, f.on_default, f.dismissal_reason, f.dismissal_comment, u.username AS "dismissed_by?", f.dismissed_at,
                  f.first_seen_at, f.last_seen_at, f.resolved_at, lp.iid AS "last_pipeline_iid?"
             FROM security_findings f
             LEFT JOIN users u ON u.id = f.dismissed_by
             LEFT JOIN ci_pipelines lp ON lp.id = f.last_pipeline_id
            WHERE f.project_id = $1 AND f.id = $2"#,
        project_id,
        id
    )
    .fetch_optional(e)
    .await
}

/// These findings, most severe first.
pub async fn by_ids(e: impl PgExecutor<'_>, ids: &[i64]) -> Result<Vec<Finding>> {
    sqlx::query_as!(
        Finding,
        r#"SELECT f.id, f.project_id, f.report_type, f.scanner, f.severity, f.title, f.description, f.solution, f.rule_id,
                  f.identifiers, f.file, f.start_line, f.end_line, f.package_name, f.package_version, f.fixed_version, f.image,
                  f.state, f.on_default, f.dismissal_reason, f.dismissal_comment, u.username AS "dismissed_by?", f.dismissed_at,
                  f.first_seen_at, f.last_seen_at, f.resolved_at, lp.iid AS "last_pipeline_iid?"
             FROM security_findings f
             LEFT JOIN users u ON u.id = f.dismissed_by
             LEFT JOIN ci_pipelines lp ON lp.id = f.last_pipeline_id
            WHERE f.id = ANY($1)
            ORDER BY array_position(ARRAY['critical', 'high', 'medium', 'low', 'info', 'unknown'], f.severity), f.id"#,
        ids
    )
    .fetch_all(e)
    .await
}

/// SecurityCounts of the findings on the default branch by severity (undismissed).
#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct SecurityCounts {
    pub critical: i64,
    pub high: i64,
    pub medium: i64,
    pub low: i64,
    pub info: i64,
    pub unknown: i64,
    pub dismissed: i64,
    pub resolved: i64,
}

pub async fn counts(e: impl PgExecutor<'_>, project_id: i64) -> Result<SecurityCounts> {
    let r = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE on_default AND state = 'detected' AND severity = 'critical') AS "critical!",
                  count(*) FILTER (WHERE on_default AND state = 'detected' AND severity = 'high') AS "high!",
                  count(*) FILTER (WHERE on_default AND state = 'detected' AND severity = 'medium') AS "medium!",
                  count(*) FILTER (WHERE on_default AND state = 'detected' AND severity = 'low') AS "low!",
                  count(*) FILTER (WHERE on_default AND state = 'detected' AND severity = 'info') AS "info!",
                  count(*) FILTER (WHERE on_default AND state = 'detected' AND severity = 'unknown') AS "unknown!",
                  count(*) FILTER (WHERE on_default AND state = 'dismissed') AS "dismissed!",
                  count(*) FILTER (WHERE state = 'resolved') AS "resolved!"
             FROM security_findings WHERE project_id = $1"#,
        project_id
    )
    .fetch_one(e)
    .await?;
    Ok(SecurityCounts {
        critical: r.critical,
        high: r.high,
        medium: r.medium,
        low: r.low,
        info: r.info,
        unknown: r.unknown,
        dismissed: r.dismissed,
        resolved: r.resolved,
    })
}

/// Dismiss (with a reason) or reopen a finding. Reopening a finding the
/// default branch no longer has makes it resolved.
pub async fn set_dismissed(e: impl PgExecutor<'_>, id: i64, dismissal: Option<(&str, &str, i64)>) -> Result<()> {
    match dismissal {
        Some((reason, comment, user)) => {
            sqlx::query!(
                "UPDATE security_findings SET state = 'dismissed', dismissal_reason = $2, dismissal_comment = $3, dismissed_by = $4, dismissed_at = now()
                  WHERE id = $1",
                id,
                reason,
                comment,
                user
            )
            .execute(e)
            .await?;
        }
        None => {
            sqlx::query!(
                "UPDATE security_findings SET state = CASE WHEN on_default THEN 'detected' ELSE 'resolved' END,
                        resolved_at = CASE WHEN on_default THEN NULL ELSE coalesce(resolved_at, now()) END,
                        dismissal_reason = NULL, dismissal_comment = NULL, dismissed_by = NULL, dismissed_at = NULL
                  WHERE id = $1",
                id
            )
            .execute(e)
            .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FindingEvent {
    pub action: String,
    pub username: Option<String>,
    pub pipeline_iid: Option<i64>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub async fn events(e: impl PgExecutor<'_>, finding_id: i64) -> Result<Vec<FindingEvent>> {
    sqlx::query_as!(
        FindingEvent,
        r#"SELECT ev.action, u.username AS "username?", p.iid AS "pipeline_iid?", ev.note, ev.created_at
             FROM security_finding_events ev
             LEFT JOIN users u ON u.id = ev.user_id
             LEFT JOIN ci_pipelines p ON p.id = ev.pipeline_id
            WHERE ev.finding_id = $1 ORDER BY ev.id"#,
        finding_id
    )
    .fetch_all(e)
    .await
}

/// The newest pipeline among these that read security reports, with the
/// report types it scanned completely.
pub async fn latest_scanned(e: impl PgExecutor<'_>, pipeline_ids: &[i64]) -> Result<Option<i64>> {
    sqlx::query_scalar!("SELECT max(pipeline_id) FROM security_scans WHERE pipeline_id = ANY($1)", pipeline_ids).fetch_one(e).await
}

/// The newest default-branch pipeline that read security reports.
pub async fn latest_default_scan(e: impl PgExecutor<'_>, project_id: i64, default_branch: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        "SELECT max(s.pipeline_id) FROM security_scans s JOIN ci_pipelines p ON p.id = s.pipeline_id
          WHERE p.project_id = $1 AND p.ref = $2 AND NOT p.tag AND p.merge_request_id IS NULL",
        project_id,
        default_branch
    )
    .fetch_one(e)
    .await
}

/// Findings a pipeline saw, of these report types.
pub async fn sighted(e: impl PgExecutor<'_>, pipeline_id: i64, report_types: &[String]) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT f.id FROM security_finding_sightings s JOIN security_findings f ON f.id = s.finding_id
          WHERE s.pipeline_id = $1 AND f.report_type = ANY($2)",
        pipeline_id,
        report_types
    )
    .fetch_all(e)
    .await
}

/// Report types a pipeline read (with or without errors).
pub async fn scanned_types(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Vec<String>> {
    sqlx::query_scalar!(r#"SELECT DISTINCT report_type AS "t!" FROM security_scans WHERE pipeline_id = $1"#, pipeline_id).fetch_all(e).await
}

// ------------------------------------------------------------ built-in scanners

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ScanningSettings {
    pub project_id: i64,
    pub enabled: bool,
    /// `sast`, `secret_detection`, `dependency_scanning`, `iac`, `container_scanning`.
    pub scanners: Vec<String>,
    pub interval_hours: i32,
    pub user_id: Option<i64>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_pipeline_id: Option<i64>,
    pub history_scanned: bool,
}

pub async fn scanning(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<ScanningSettings>> {
    sqlx::query_as!(
        ScanningSettings,
        "SELECT project_id, enabled, scanners, interval_hours, user_id, next_run_at, last_pipeline_id, history_scanned
           FROM security_scanning_settings WHERE project_id = $1",
        project_id
    )
    .fetch_optional(e)
    .await
}

pub async fn save_scanning(
    e: impl PgExecutor<'_>,
    project_id: i64,
    enabled: bool,
    scanners: &[String],
    interval_hours: i32,
    user_id: Option<i64>,
    next_run_at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO security_scanning_settings (project_id, enabled, scanners, interval_hours, user_id, next_run_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (project_id) DO UPDATE SET enabled = $2, scanners = $3, interval_hours = $4, user_id = $5, next_run_at = $6,
                                                updated_at = now()",
        project_id,
        enabled,
        scanners,
        interval_hours,
        user_id,
        next_run_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// A run started (or was skipped): when the next one is due.
pub async fn scan_started(e: impl PgExecutor<'_>, project_id: i64, pipeline_id: Option<i64>, next_run_at: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        "UPDATE security_scanning_settings SET last_pipeline_id = COALESCE($2, last_pipeline_id), next_run_at = $3,
                history_scanned = history_scanned OR $2 IS NOT NULL, updated_at = now()
          WHERE project_id = $1",
        project_id,
        pipeline_id,
        next_run_at
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Bring the next run forward to `at` (a push), not sooner than `min_gap_secs`
/// after the last one started.
pub async fn bring_scan_forward(e: impl PgExecutor<'_>, project_id: i64, at: DateTime<Utc>, min_gap_secs: f64) -> Result<bool> {
    Ok(sqlx::query!(
        "UPDATE security_scanning_settings s
            SET next_run_at = LEAST(COALESCE(s.next_run_at, $2),
                                    GREATEST($2, COALESCE((SELECT p.created_at FROM ci_pipelines p WHERE p.id = s.last_pipeline_id), $2)
                                                 + make_interval(secs => $3)))
          WHERE s.project_id = $1 AND s.enabled",
        project_id,
        at,
        min_gap_secs
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}

/// Projects whose scan is due, longest waiting first.
pub async fn due_scans(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT s.project_id FROM security_scanning_settings s JOIN projects p ON p.id = s.project_id
          WHERE s.enabled AND s.next_run_at <= now() AND p.archived_at IS NULL ORDER BY s.next_run_at LIMIT $1",
        limit
    )
    .fetch_all(e)
    .await
}

/// Security pipelines not finished: of a project, or of the instance.
pub async fn active_scans(e: impl PgExecutor<'_>, project_id: Option<i64>) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM ci_pipelines WHERE source = 'security' AND ($1::bigint IS NULL OR project_id = $1) AND status = ANY($2)",
        project_id,
        ACTIVE as &[&str]
    )
    .fetch_all(e)
    .await
}

/// Security pipelines still unfinished after `secs` (no runner took them).
pub async fn stale_scans(e: impl PgExecutor<'_>, secs: f64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT id FROM ci_pipelines
          WHERE source = 'security' AND created_at < now() - make_interval(secs => $1) AND status = ANY($2)",
        secs,
        ACTIVE as &[&str]
    )
    .fetch_all(e)
    .await
}

/// Whether a merge request's commit was scanned (or is being scanned) already.
pub async fn mr_scanned(e: impl PgExecutor<'_>, mr_id: i64, sha: &str) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM ci_pipelines WHERE source = 'security' AND merge_request_id = $1 AND sha = $2) AS "x!""#,
        mr_id,
        sha
    )
    .fetch_one(e)
    .await
}

/// Projects with scanning on (the admin page).
pub async fn scanning_count(e: impl PgExecutor<'_>) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM security_scanning_settings WHERE enabled"#).fetch_one(e).await
}

pub async fn scanning_overrides(e: impl PgExecutor<'_>) -> Result<serde_json::Value> {
    sqlx::query_scalar!("SELECT security_scanning FROM instance_settings").fetch_one(e).await
}

pub async fn set_scanning_overrides(e: impl PgExecutor<'_>, v: &serde_json::Value) -> Result<()> {
    sqlx::query!("UPDATE instance_settings SET security_scanning = $1, updated_at = now()", v).execute(e).await?;
    Ok(())
}

pub async fn scanning_group_default(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<bool> {
    Ok(sqlx::query_scalar!("SELECT enabled FROM security_scanning_group_defaults WHERE namespace_id = $1", namespace_id)
        .fetch_optional(e)
        .await?
        .unwrap_or(false))
}

pub async fn set_scanning_group_default(e: impl PgExecutor<'_>, namespace_id: i64, enabled: bool) -> Result<()> {
    sqlx::query!(
        "INSERT INTO security_scanning_group_defaults (namespace_id, enabled) VALUES ($1, $2)
         ON CONFLICT (namespace_id) DO UPDATE SET enabled = $2",
        namespace_id,
        enabled
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Whether a group above the project turned scanning on for its projects.
pub async fn scanning_inherited(e: impl PgExecutor<'_>, traversal_ids: &[i64]) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM security_scanning_group_defaults WHERE enabled AND namespace_id = ANY ($1)) AS "x!""#,
        traversal_ids
    )
    .fetch_one(e)
    .await
}

/// Projects under a group that never chose (no scanning settings yet).
pub async fn scanning_undecided_under(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT p.id FROM projects p JOIN namespaces n ON n.id = p.namespace_id
          WHERE $1 = ANY (n.traversal_ids) AND NOT EXISTS (SELECT 1 FROM security_scanning_settings s WHERE s.project_id = p.id)
          ORDER BY p.id",
        namespace_id
    )
    .fetch_all(e)
    .await
}

// ------------------------------------------------------------ alerts (slice 3)

/// Maintainers and Owners of a project (its own and inherited members), to
/// alert about vulnerable dependencies.
pub async fn maintainer_ids(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        r#"SELECT DISTINCT m.user_id AS "id!" FROM members m
            WHERE m.access_level >= 40 AND (m.expires_at IS NULL OR m.expires_at > now())
              AND ((m.source_type = 'project' AND m.source_id = $1)
                OR (m.source_type = 'namespace' AND m.source_id = ANY((SELECT n.traversal_ids FROM projects p JOIN namespaces n ON n.id = p.namespace_id WHERE p.id = $1)::bigint[])))"#,
        project_id
    )
    .fetch_all(e)
    .await
}

/// A merge request that may fix a finding: Renovate's, updating its package.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FixMergeRequest {
    pub iid: i64,
    pub title: String,
    pub state: String,
}

pub async fn fix_merge_requests(e: impl PgExecutor<'_>, project_id: i64, package: &str) -> Result<Vec<FixMergeRequest>> {
    sqlx::query_as!(
        FixMergeRequest,
        r#"SELECT iid, title, state FROM merge_requests
            WHERE project_id = $1 AND source_branch LIKE 'renovate/%' AND state IN ('opened', 'merged')
              AND (title ILIKE '%' || $2 || '%' OR source_branch ILIKE '%' || $2 || '%')
            ORDER BY (state = 'opened') DESC, iid DESC LIMIT 5"#,
        project_id,
        package
    )
    .fetch_all(e)
    .await
}

/// Projects with open critical or high findings on their default branch.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ExposedProject {
    pub project_id: i64,
    pub full_path: String,
    pub critical: i64,
    pub high: i64,
}

pub async fn exposed_projects(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<ExposedProject>> {
    sqlx::query_as!(
        ExposedProject,
        r#"SELECT f.project_id AS "project_id!", r.path AS "full_path!",
                  count(*) FILTER (WHERE f.severity = 'critical') AS "critical!",
                  count(*) FILTER (WHERE f.severity = 'high') AS "high!"
             FROM security_findings f
             JOIN routes r ON r.source_type = 'project' AND r.source_id = f.project_id
            WHERE f.on_default AND f.state = 'detected' AND f.severity IN ('critical', 'high')
            GROUP BY f.project_id, r.path
            ORDER BY 3 DESC, 4 DESC, 2 LIMIT $1"#,
        limit
    )
    .fetch_all(e)
    .await
}
