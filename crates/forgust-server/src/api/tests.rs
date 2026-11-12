//! Test results from JUnit reports (M7): per pipeline, and per merge request
//! compared with its target branch (GitLab's test summary).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use forgust_db::ci::{self as cidb, TestReport};
use serde::Serialize;
use utoipa::ToSchema;

use super::project_for;
use crate::auth::Viewer;
use crate::auth::authz::ProjectAction;
use crate::ci::reports::{CaseRow, TestCase};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Serialize, ToSchema, Default)]
pub struct Totals {
    pub total: i32,
    pub success: i32,
    pub failed: i32,
    pub errored: i32,
    pub skipped: i32,
    pub duration: f64,
}

fn totals(reports: &[TestReport]) -> Totals {
    let mut t = Totals::default();
    for r in reports {
        t.total += r.total;
        t.failed += r.failed;
        t.errored += r.errored;
        t.skipped += r.skipped;
        t.duration += r.duration;
    }
    t.success = t.total - t.failed - t.errored - t.skipped;
    t
}

#[derive(Serialize, ToSchema)]
pub struct PipelineTests {
    #[serde(flatten)]
    pub totals: Totals,
    /// One per job that uploaded a report.
    pub jobs: Vec<TestReport>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/pipelines/{iid}/test_report", responses((status = 200, body = PipelineTests)), tag = "ci")]
pub async fn pipeline_tests(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<PipelineTests>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let pl = cidb::pipeline_by_iid(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let jobs = cidb::test_reports(&state.db, pl.id).await?;
    Ok(Json(PipelineTests { totals: totals(&jobs), jobs }))
}

#[derive(Serialize, ToSchema)]
pub struct ComparedFailure {
    pub job: String,
    #[schema(value_type = Object)]
    pub case: serde_json::Value,
}

#[derive(Serialize, ToSchema)]
pub struct MrTests {
    /// The merge request's head pipeline with test reports.
    pub head_pipeline_iid: Option<i64>,
    pub head: Option<Totals>,
    /// The target branch's latest finished pipeline with test reports.
    pub base_pipeline_iid: Option<i64>,
    pub base: Option<Totals>,
    /// Failing here, not on the target branch (or no comparison possible).
    pub new_failures: Vec<ComparedFailure>,
    /// Failing here and on the target branch.
    pub existing_failures: Vec<ComparedFailure>,
    /// Failing on the target branch, passing here: `classname::name`.
    pub resolved: Vec<String>,
}

fn cases(reports: &[TestReport]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for r in reports {
        for c in CaseRow::read(&r.cases) {
            out.entry(c.0).or_insert(c.1);
        }
    }
    out
}

#[derive(Serialize, ToSchema)]
pub struct TestCaseBrief {
    pub suite: String,
    pub classname: String,
    pub name: String,
    /// success, failed, error or skipped
    pub status: String,
    /// Seconds.
    pub time: f64,
}

#[derive(Serialize, ToSchema)]
pub struct JobTestCases {
    pub build_id: i64,
    pub job_name: String,
    /// Every case of the job's report (at most 20,000), in report order.
    pub cases: Vec<TestCaseBrief>,
}

/// Every test case of a pipeline's reports, per job (failure details are in `test_report`).
#[utoipa::path(get, path = "/api/v1/projects/{id}/pipelines/{iid}/test_cases", responses((status = 200, body = Vec<JobTestCases>)), tag = "ci")]
pub async fn pipeline_test_cases(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Vec<JobTestCases>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let pl = cidb::pipeline_by_iid(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let jobs = cidb::test_reports(&state.db, pl.id).await?;
    Ok(Json(
        jobs.into_iter()
            .map(|r| JobTestCases {
                build_id: r.build_id,
                job_name: r.job_name,
                cases: CaseRow::read(&r.cases)
                    .into_iter()
                    .map(|CaseRow(key, status, time, suite, name)| {
                        // older reports kept only the key (`classname::name`)
                        let (classname, name) = match key.strip_suffix(&format!("::{name}")) {
                            Some(c) if !name.is_empty() => (c.to_string(), name),
                            _ => key.rsplit_once("::").map(|(c, n)| (c.to_string(), n.to_string())).unwrap_or_default(),
                        };
                        TestCaseBrief { suite, classname, name, status, time }
                    })
                    .collect(),
            })
            .collect(),
    ))
}

fn failing(s: &str) -> bool {
    s == "failed" || s == "error"
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/test_summary", responses((status = 200, body = MrTests)), tag = "merge_requests")]
pub async fn mr_tests(State(state): State<AppState>, Viewer(v): Viewer, Path((id, iid)): Path<(String, i64)>) -> ApiResult<Json<MrTests>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let m = forgust_db::merge_requests::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let head_sha = forgust_db::merge_requests::latest_diff(&state.db, m.id).await?.map(|d| d.head_sha);
    let mut out = MrTests {
        head_pipeline_iid: None,
        head: None,
        base_pipeline_iid: None,
        base: None,
        new_failures: vec![],
        existing_failures: vec![],
        resolved: vec![],
    };
    let Some(head_id) = (match &head_sha {
        Some(sha) => cidb::pipeline_with_tests(&state.db, p.id, Some(sha), None).await?,
        None => None,
    }) else {
        return Ok(Json(out));
    };
    let head = cidb::test_reports(&state.db, head_id).await?;
    out.head_pipeline_iid = cidb::pipeline(&state.db, head_id).await?.map(|pl| pl.iid);
    out.head = Some(totals(&head));
    let base = match cidb::pipeline_with_tests(&state.db, p.id, None, Some(&m.target_branch)).await? {
        Some(id) if id != head_id => {
            out.base_pipeline_iid = cidb::pipeline(&state.db, id).await?.map(|pl| pl.iid);
            let b = cidb::test_reports(&state.db, id).await?;
            out.base = Some(totals(&b));
            Some(cases(&b))
        }
        _ => None,
    };
    let head_cases = cases(&head);
    for r in &head {
        let Ok(list) = serde_json::from_value::<Vec<TestCase>>(r.failures.clone()) else { continue };
        for c in list {
            let before = base.as_ref().and_then(|b| b.get(&c.key()));
            let f = ComparedFailure { job: r.job_name.clone(), case: serde_json::to_value(&c).unwrap_or_default() };
            if before.is_some_and(|s| failing(s)) { out.existing_failures.push(f) } else { out.new_failures.push(f) }
        }
    }
    if let Some(b) = &base {
        let mut fixed: Vec<String> =
            b.iter().filter(|(k, s)| failing(s) && head_cases.get(*k).is_some_and(|h| h == "success")).map(|(k, _)| k.clone()).collect();
        fixed.sort();
        out.resolved = fixed;
    }
    Ok(Json(out))
}
