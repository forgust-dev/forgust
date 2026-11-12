//! Security findings (ADR-0024): a finished job's security reports
//! become the project's findings; a complete default-branch scan resolves what
//! it no longer finds; merge requests compare their pipeline with the default
//! branch's. Reading runs as background jobs, one project at a time, in order.

use std::collections::HashMap;

use forgust_db::ci::Pipeline;
use forgust_db::projects::Project;
use forgust_db::security as sdb;
use serde::Serialize;

use crate::ci::security::{self as parse, Finding};
use crate::state::AppState;

/// Lines of a finding read for its fingerprint, at most.
const MAX_LINES: usize = 50;

/// A pipeline on the default branch itself (not a tag, not a merge request's).
pub fn on_default_branch(p: &Project, pl: &Pipeline) -> bool {
    !pl.tag && pl.merge_request_id.is_none() && p.default_branch.as_deref() == Some(pl.git_ref.as_str())
}

/// After a pipeline ends: queue resolving what its scans no longer find
/// (behind the reading of its reports: same queue).
pub async fn after_pipeline(state: &AppState, pipeline_id: i64, project_id: i64) {
    let r: anyhow::Result<()> = async {
        let mut tx = state.db.begin().await?;
        state.jobs.enqueue(&mut tx, crate::jobs::ResolveSecurityFindings { pipeline_id, project_id }.request()).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(e) = r {
        tracing::warn!(pipeline_id, error = %e, "security resolution not queued");
    }
}

/// Read a finished job's security reports into findings.
pub async fn ingest(state: &AppState, build_id: i64) -> anyhow::Result<()> {
    let Some(b) = forgust_db::ci::build(&state.db, build_id).await? else { return Ok(()) };
    let Some(pl) = forgust_db::ci::pipeline(&state.db, b.pipeline_id).await? else { return Ok(()) };
    let Some(p) = forgust_db::projects::find(&state.db, b.project_id).await? else { return Ok(()) };
    let default = on_default_branch(&p, &pl);
    let mut files: HashMap<String, Option<Vec<String>>> = HashMap::new();
    // what to tell people once read (slice 3)
    let mut fixable: Vec<Finding> = Vec::new();
    let mut secrets: Vec<Finding> = Vec::new();
    for a in forgust_db::ci::artifacts_of_build(&state.db, b.id).await? {
        if !parse::REPORT_TYPES.contains(&a.file_type.as_str()) {
            continue;
        }
        let parsed = match crate::ci::reports::read(state, &a.blob_key, &a.file_format, parse::MAX_BYTES).await {
            Ok(data) => parse::parse(&data, &a.file_type),
            Err(e) => Err(e.to_string()),
        };
        let (scanner, findings) = match parsed {
            Ok(x) => x,
            Err(e) => {
                sdb::record_scan(&state.db, b.id, pl.id, &a.file_type, "", 0, Some(&e)).await?;
                continue;
            }
        };
        let mut tx = state.db.begin().await?;
        // a SARIF file may hold several kinds: each is recorded as scanned
        let mut kinds: Vec<String> = vec![a.file_type.clone()];
        for f in &findings {
            let lines = match (&f.file, f.report_type.as_str()) {
                (Some(file), "sast" | "secret_detection") if f.stable.is_none() => lines_of(state, &p, &pl.sha, file, f, &mut files).await,
                _ => None,
            };
            let fp = f.fingerprint(lines.as_deref());
            let ids = serde_json::to_value(&f.identifiers)?;
            let (id, prev_state, prev_default, new) = sdb::upsert(
                &mut tx,
                &sdb::NewFinding {
                    project_id: p.id,
                    pipeline_id: pl.id,
                    fingerprint: &fp,
                    report_type: &f.report_type,
                    scanner: &f.scanner,
                    severity: &f.severity,
                    title: &f.title,
                    description: &f.description,
                    solution: &f.solution,
                    rule_id: &f.rule_id,
                    identifiers: &ids,
                    file: f.file.as_deref(),
                    start_line: f.start_line,
                    end_line: f.end_line,
                    package_name: f.package_name.as_deref(),
                    package_version: f.package_version.as_deref(),
                    fixed_version: f.fixed_version.as_deref(),
                    image: f.image.as_deref(),
                },
            )
            .await?;
            if default && !prev_default {
                sdb::seen_on_default(&mut tx, id).await?;
                let action = if new || prev_state == "detected" {
                    "detected"
                } else if prev_state == "resolved" {
                    "redetected"
                } else {
                    ""
                };
                if !action.is_empty() {
                    sdb::event(&mut *tx, id, action, None, Some(pl.id), None).await?;
                    if prev_state != "dismissed"
                        && f.fixed_version.is_some()
                        && matches!(f.report_type.as_str(), "dependency_scanning" | "container_scanning")
                    {
                        fixable.push(f.clone());
                    }
                }
            }
            if new && f.report_type == "secret_detection" {
                secrets.push(f.clone());
            }
            if !kinds.contains(&f.report_type) {
                kinds.push(f.report_type.clone());
            }
        }
        for k in &kinds {
            let n = findings.iter().filter(|f| &f.report_type == k).count() as i32;
            sdb::record_scan(&mut *tx, b.id, pl.id, k, &scanner, n, None).await?;
        }
        tx.commit().await?;
    }
    state.events.publish(crate::events::Event::Pipeline { project_id: p.id, pipeline_id: pl.id, status: pl.status.clone() });
    alert(state, &p, &pl, &fixable, &secrets).await;
    Ok(())
}

/// Vulnerable dependencies with a fix, newly on the default branch: one email to
/// the Maintainers. A new secret: to whoever ran the pipeline (they pushed it),
/// to rotate it.
async fn alert(state: &AppState, p: &Project, pl: &Pipeline, fixable: &[Finding], secrets: &[Finding]) {
    use super::notify::{Notice, Reason, deliver};
    let web = state.cfg.web_url(&p.full_path);
    if !fixable.is_empty() {
        let mut body = format!(
            "The default branch of {} has {} vulnerable dependenc{} with a fix:\n\n",
            p.full_path,
            fixable.len(),
            if fixable.len() == 1 { "y" } else { "ies" }
        );
        for f in fixable.iter().take(20) {
            let what = f.package_name.as_deref().unwrap_or(&f.rule_id);
            let cve =
                f.identifiers.iter().find(|i| i.kind == "cve" || i.kind == "ghsa").map(|i| format!(" ({})", i.value)).unwrap_or_default();
            body.push_str(&format!(
                "- [{}] {what} {} → {}{cve}: {}\n",
                f.severity,
                f.package_version.as_deref().unwrap_or("?"),
                f.fixed_version.as_deref().unwrap_or("?"),
                f.title
            ));
        }
        if fixable.len() > 20 {
            body.push_str(&format!("- and {} more\n", fixable.len() - 20));
        }
        body.push_str(&format!(
            "\nSee them: {web}/-/security\nWith dependency updates on, Renovate opens the merge requests that fix them.\n"
        ));
        let people = sdb::maintainer_ids(&state.db, p.id).await.unwrap_or_default().into_iter().map(|id| (id, Reason::Direct)).collect();
        let subject =
            format!("[{}] {} vulnerable dependenc{} to update", p.full_path, fixable.len(), if fixable.len() == 1 { "y" } else { "ies" });
        deliver(
            state,
            Notice {
                project: p,
                need: crate::auth::authz::ProjectAction::ReadCode,
                actor: None,
                subject,
                body,
                thread: None,
                starts_thread: false,
            },
            people,
        )
        .await;
    }
    if let (false, Some(user)) = (secrets.is_empty(), pl.user_id) {
        let mut body = format!(
            "A scan of {} ({}) found {} secret{} in the code:\n\n",
            p.full_path,
            pl.git_ref,
            secrets.len(),
            if secrets.len() == 1 { "" } else { "s" }
        );
        for f in secrets.iter().take(20) {
            body.push_str(&format!(
                "- {} in {}{}\n",
                f.title,
                f.file.as_deref().unwrap_or("?"),
                f.start_line.map(|l| format!(":{l}")).unwrap_or_default()
            ));
        }
        body.push_str(&format!(
            "\nRemoving it from the code is not enough: it stays in the history. Revoke it and make a new one, then dismiss the finding.\n{web}/-/security\n"
        ));
        deliver(
            state,
            Notice {
                project: p,
                need: crate::auth::authz::ProjectAction::ReadCode,
                actor: None,
                subject: format!("[{}] A secret was found in the code", p.full_path),
                body,
                thread: None,
                starts_thread: false,
            },
            vec![(user, Reason::Direct)],
        )
        .await;
    }
}

/// The text of a finding's lines at the pipeline's commit (None when the file
/// cannot be read: the fingerprint then uses the line number).
async fn lines_of(
    state: &AppState,
    p: &Project,
    sha: &str,
    file: &str,
    f: &Finding,
    cache: &mut HashMap<String, Option<Vec<String>>>,
) -> Option<String> {
    if !cache.contains_key(file) {
        let read = state.storage.blob(p.id, sha, file, 4 << 20).await.ok().filter(|b| !b.truncated);
        cache.insert(file.to_string(), read.map(|b| String::from_utf8_lossy(&b.data).lines().map(str::to_string).collect()));
    }
    let lines = cache.get(file)?.as_ref()?;
    let start = f.start_line? as usize;
    let end = f.end_line.map(|e| e as usize).unwrap_or(start).max(start).min(start + MAX_LINES);
    (start >= 1 && start <= lines.len()).then(|| lines[start - 1..end.min(lines.len())].join("\n"))
}

/// A default-branch pipeline ended: for each report type it scanned
/// completely (and no newer pipeline did), what it no longer finds is
/// resolved.
pub async fn resolve(state: &AppState, pipeline_id: i64) -> anyhow::Result<()> {
    let Some(pl) = forgust_db::ci::pipeline(&state.db, pipeline_id).await? else { return Ok(()) };
    let Some(p) = forgust_db::projects::find(&state.db, pl.project_id).await? else { return Ok(()) };
    if !on_default_branch(&p, &pl) || !crate::ci::pipeline::is_complete(&pl.status) {
        return Ok(());
    }
    let branch = p.default_branch.clone().unwrap_or_default();
    for t in sdb::complete_report_types(&state.db, pl.id).await? {
        if sdb::newer_scan_exists(&state.db, p.id, pl.id, &t, &branch).await? {
            continue;
        }
        let mut tx = state.db.begin().await?;
        for id in sdb::resolve_missing(&mut tx, p.id, pl.id, &t).await? {
            sdb::event(&mut *tx, id, "resolved", None, Some(pl.id), None).await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// What a merge request's pipeline finds, against the default branch.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MrSecurity {
    /// The merge request's newest pipeline that read security reports.
    pub head_pipeline_iid: Option<i64>,
    /// The default branch's newest one.
    pub base_pipeline_iid: Option<i64>,
    /// Report types both scanned (only those compare).
    pub report_types: Vec<String>,
    /// Found by the merge request's pipeline, not by the default branch's.
    pub new: Vec<sdb::Finding>,
    /// Found by the default branch's, no longer by the merge request's.
    pub fixed: Vec<sdb::Finding>,
}

impl MrSecurity {
    /// New critical or high findings nobody dismissed (the merge check).
    pub fn blocking(&self) -> usize {
        self.new.iter().filter(|f| (f.severity == "critical" || f.severity == "high") && f.state != "dismissed").count()
    }
}

pub async fn for_merge_request(state: &AppState, p: &Project, mr: &forgust_db::merge_requests::MergeRequest) -> anyhow::Result<MrSecurity> {
    let mut out = MrSecurity { head_pipeline_iid: None, base_pipeline_iid: None, report_types: vec![], new: vec![], fixed: vec![] };
    let pipelines = forgust_db::ci::pipelines_of_mr(&state.db, mr.id, &mr.source_branch, p.id, 50).await?;
    let ids: Vec<i64> = pipelines.iter().map(|x| x.id).collect();
    let Some(head) = sdb::latest_scanned(&state.db, &ids).await? else { return Ok(out) };
    out.head_pipeline_iid = pipelines.iter().find(|x| x.id == head).map(|x| x.iid);
    let head_types = sdb::scanned_types(&state.db, head).await?;
    let base = match &p.default_branch {
        Some(b) => sdb::latest_default_scan(&state.db, p.id, b).await?,
        None => None,
    };
    let (base_types, base_found) = match base {
        Some(b) => {
            out.base_pipeline_iid = forgust_db::ci::pipeline(&state.db, b).await?.map(|x| x.iid);
            let t = sdb::scanned_types(&state.db, b).await?;
            (t, Some(b))
        }
        None => (vec![], None),
    };
    // without a base scan everything the MR finds is new
    let types: Vec<String> =
        if base_found.is_some() { head_types.iter().filter(|t| base_types.contains(t)).cloned().collect() } else { head_types.clone() };
    let in_head = sdb::sighted(&state.db, head, &types).await?;
    let in_base = match base_found {
        Some(b) => sdb::sighted(&state.db, b, &types).await?,
        None => vec![],
    };
    let new: Vec<i64> = in_head.iter().filter(|i| !in_base.contains(i)).copied().collect();
    let fixed: Vec<i64> = in_base.iter().filter(|i| !in_head.contains(i)).copied().collect();
    out.new = sdb::by_ids(&state.db, &new).await?;
    out.fixed = sdb::by_ids(&state.db, &fixed).await?;
    out.report_types = types;
    Ok(out)
}
