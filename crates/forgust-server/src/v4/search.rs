//! GitLab's search API, for code only: `scope=blobs` on the instance, a group
//! or a project. One entry per file (GitLab sends one per matching chunk),
//! with the first matching lines as `data`.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{PageQuery, paged};
use crate::api::{group_for, project_for};
use crate::auth::authz::{GroupAction, ProjectAction};
use crate::auth::{ClientIp, Viewer};
use crate::code_search::search::{self as code, Params};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct SearchQuery {
    scope: Option<String>,
    #[serde(default)]
    search: String,
}

async fn blobs(
    state: &AppState,
    v: &crate::auth::Principal,
    ip: ClientIp,
    uri: &axum::http::Uri,
    q: SearchQuery,
    page: PageQuery,
    mut params: Params,
) -> ApiResult<Response> {
    if q.scope.as_deref() != Some("blobs") {
        return Err(ApiError::bad("scope must be blobs: other scopes are not supported"));
    }
    crate::auth::heavy_read_allowed(state, v, ip)?;
    params.text = q.search.clone();
    // one more than the page, to know whether another follows
    params.limit = page.probe() as usize;
    params.skip = page.offset() as usize;
    let found = code::search(state, v, params).await?;
    let items: Vec<Value> = found
        .files
        .iter()
        .map(|f| {
            let name = f.path.rsplit('/').next().unwrap_or(&f.path);
            let first = f.chunks.first();
            json!({
                "basename": name.rsplit_once('.').map(|(b, _)| b).filter(|b| !b.is_empty()).unwrap_or(name),
                "data": first.map(|c| c.lines.iter().map(|l| format!("{}\n", l.text)).collect::<String>()).unwrap_or_default(),
                "path": f.path,
                "filename": f.path,
                "id": Value::Null,
                "ref": f.branch,
                "startline": first.map(|c| c.start).unwrap_or(1),
                "project_id": f.project_id,
            })
        })
        .collect();
    Ok(paged(state, uri, &page, items, None))
}

pub async fn instance(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    ip: ClientIp,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<SearchQuery>,
    Query(page): Query<PageQuery>,
) -> ApiResult<Response> {
    blobs(&state, &v, ip, &uri, q, page, Params::default()).await
}

pub async fn project(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    ip: ClientIp,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<SearchQuery>,
    Query(page): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    blobs(&state, &v, ip, &uri, q, page, Params { project_id: Some(p.id), ..Params::default() }).await
}

pub async fn group(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    ip: ClientIp,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<SearchQuery>,
    Query(page): Query<PageQuery>,
) -> ApiResult<Response> {
    let g = group_for(&state, &v, &id, GroupAction::Read).await?;
    blobs(&state, &v, ip, &uri, q, page, Params { group_path: Some(g.full_path), ..Params::default() }).await
}
