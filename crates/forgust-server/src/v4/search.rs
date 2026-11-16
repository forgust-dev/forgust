//! GitLab's search API, for code only: `scope=blobs` on the instance, a group
//! or a project. As GitLab does, one entry per matching chunk of a file, and
//! `filename:`, `path:` and `extension:` in the search text narrow the files.
//! Only the default branch is searched.

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
    /// The search text is a regular expression (GitLab's exact code search).
    #[serde(default)]
    regex: bool,
    /// Projects only: must be the default branch, the only one indexed.
    r#ref: Option<String>,
}

/// GitLab's filters inside the search text (`keyword filename:*.py`), taken
/// out of it: (text, path, extension). `filename` is GitLab's older name for
/// `path`; a `*` matches anything, which "the path contains" already does
/// for the longest piece between them.
fn split_filters(search: &str) -> (String, Option<String>, Option<String>) {
    let (mut text, mut path, mut extension) = (Vec::new(), None, None);
    for word in search.split(' ') {
        if let Some(glob) = word.strip_prefix("path:").or_else(|| word.strip_prefix("filename:")) {
            path = glob.split('*').max_by_key(|piece| piece.len()).filter(|p| !p.is_empty()).map(str::to_string);
        } else if let Some(ext) = word.strip_prefix("extension:") {
            extension = Some(ext.to_string());
        } else {
            text.push(word);
        }
    }
    (text.join(" ").trim().to_string(), path, extension)
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
    (params.text, params.path, params.extension) = split_filters(&q.search);
    params.regex = q.regex;
    // an entry is a chunk and every file has at least one: this many files
    // hold the page and one more entry, to know whether another page follows
    let (offset, per) = (page.offset() as usize, page.per_page() as usize);
    params.limit = offset + per + 1;
    let found = code::search(state, v, params).await?;
    if let Some(r) = q.r#ref.as_deref().filter(|r| found.files.iter().any(|f| f.branch != *r)) {
        return Err(ApiError::bad(format!("only the default branch can be searched, not `{r}`")));
    }
    let items: Vec<Value> = found
        .files
        .iter()
        .flat_map(|f| f.chunks.iter().map(move |c| (f, c)))
        .skip(offset)
        .take(per + 1)
        .map(|(f, c)| {
            let name = f.path.rsplit('/').next().unwrap_or(&f.path);
            json!({
                "basename": name.rsplit_once('.').map(|(b, _)| b).filter(|b| !b.is_empty()).unwrap_or(name),
                "data": c.lines.iter().map(|l| format!("{}\n", l.text)).collect::<String>(),
                "path": f.path,
                "filename": f.path,
                "id": Value::Null,
                "ref": f.branch,
                "startline": c.start,
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

#[cfg(test)]
mod tests {
    use super::split_filters;

    #[test]
    fn gitlab_filters_leave_the_search_text() {
        assert_eq!(split_filters("keyword filename:*.py"), ("keyword".into(), Some(".py".into()), None));
        assert_eq!(
            split_filters("fn main path:src/*/main.rs extension:rs"),
            ("fn main".into(), Some("/main.rs".into()), Some("rs".into()))
        );
        assert_eq!(split_filters("a: b path:*"), ("a: b".into(), None, None));
    }
}
