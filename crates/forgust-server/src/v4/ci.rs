//! Pipelines and their jobs (`glab ci status`, `glab ci list`).

use axum::Json;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::response::Response;
use forgust_db::ci as cidb;
use serde::Deserialize;
use serde_json::Value;

use super::json as j;
use super::{PageQuery, paged};
use crate::api::project_for;
use crate::auth::Viewer;
use crate::auth::authz::ProjectAction;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    status: Option<String>,
    sha: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let list =
        cidb::list_pipelines(&state.db, p.id, q.git_ref.as_deref(), q.status.as_deref(), q.sha.as_deref(), pg.probe(), pg.offset()).await?;
    let items: Vec<Value> = list.iter().map(|pl| j::pipeline(&state.cfg, &p, pl)).collect();
    Ok(paged(&state, &uri, &pg, items, None))
}

#[derive(Deserialize)]
pub struct LatestQuery {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
}

pub async fn latest(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<LatestQuery>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let r = q.git_ref.or(p.default_branch.clone()).ok_or(ApiError::NotFound)?;
    let pl = cidb::list_pipelines(&state.db, p.id, Some(&r), None, None, 1, 0).await?.into_iter().next().ok_or(ApiError::NotFound)?;
    Ok(Json(j::pipeline(&state.cfg, &p, &pl)))
}

/// Pipelines are addressed by their global id here, as in GitLab.
async fn pipeline(
    state: &AppState,
    v: &crate::auth::Principal,
    id: &str,
    pipeline: i64,
) -> ApiResult<(forgust_db::projects::Project, cidb::Pipeline)> {
    let p = project_for(state, v, id, ProjectAction::ReadCode).await?;
    let pl = cidb::pipeline(&state.db, pipeline).await?.filter(|pl| pl.project_id == p.id).ok_or(ApiError::NotFound)?;
    Ok((p, pl))
}

pub async fn show(State(state): State<AppState>, Viewer(v): Viewer, Path((id, pid)): Path<(String, i64)>) -> ApiResult<Json<Value>> {
    let (p, pl) = pipeline(&state, &v, &id, pid).await?;
    Ok(Json(j::pipeline(&state.cfg, &p, &pl)))
}

pub async fn jobs(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path((id, pid)): Path<(String, i64)>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let (p, pl) = pipeline(&state, &v, &id, pid).await?;
    let builds: Vec<_> = cidb::builds_of_pipeline(&state.db, pl.id).await?.into_iter().filter(|b| !b.retried).collect();
    let total = builds.len() as i64;
    let items: Vec<Value> =
        builds.iter().skip(pg.offset() as usize).take(pg.per_page() as usize).map(|b| j::job(&state.cfg, &p, b, &pl)).collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

/// GET .../jobs/:job_id/artifacts/*artifact_path: one file of the archive.
pub async fn artifact_file(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, job_id, path)): Path<(String, i64, String)>,
) -> ApiResult<axum::response::Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let b = forgust_db::ci::build(&state.db, job_id).await?.filter(|b| b.project_id == p.id).ok_or(ApiError::NotFound)?;
    let a = forgust_db::ci::artifact(&state.db, b.id, "archive").await?.filter(|a| a.file_format == "zip").ok_or(ApiError::NotFound)?;
    crate::api::ci::serve_artifact_file(&state, &a, path.trim_start_matches('/')).await
}
