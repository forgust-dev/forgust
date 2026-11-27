//! Searching the instance (M7): projects, issues and merge requests by title,
//! only what the viewer may see (`forgust_db::search`); and the text of the
//! code, at `/search/code` (`code_search`).

use axum::Json;
use axum::extract::{Query, State};
use forgust_db::search::{self as sdb, Counts, ItemHit, ProjectHit};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::{ClientIp, Viewer};
use crate::code_search::search as code;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    /// all (a few of each, default), projects, issues or merge_requests; or first:
    /// the first of issues, merge_requests and projects that finds something
    scope: Option<String>,
    /// For issues and merge requests: opened, closed, merged or all (default)
    state: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct SearchResults {
    /// The scope answered: the one asked for, or the one `first` picked.
    pub scope: String,
    pub counts: Counts,
    pub projects: Vec<ProjectHit>,
    pub issues: Vec<ItemHit>,
    pub merge_requests: Vec<ItemHit>,
}

/// Search projects, issues and merge requests by title, only what the caller may see (`scope`, `state`, paging).
#[utoipa::path(get, path = "/api/v1/search", responses((status = 200, body = SearchResults)), tag = "projects")]
pub async fn search(State(state): State<AppState>, Viewer(v): Viewer, Query(q): Query<SearchQuery>) -> ApiResult<Json<SearchResults>> {
    let term = q.q.trim();
    if term.is_empty() || term.chars().count() > 200 {
        return Err(ApiError::bad("search for 1-200 characters"));
    }
    let pattern = forgust_db::like_escape(term);
    let viewer = v.search_viewer();
    let asked = q.scope.as_deref().unwrap_or("all");
    let st = q.state.as_deref().unwrap_or("all");
    if !["opened", "closed", "merged", "all"].contains(&st) {
        return Err(ApiError::bad("state must be opened, closed, merged or all"));
    }
    match asked {
        "all" | "first" | "projects" | "issues" | "merge_requests" => {}
        _ => return Err(ApiError::bad("scope must be all, first, projects, issues or merge_requests")),
    }
    let (per, offset) = super::page_window(q.page.unwrap_or(1), q.per_page.unwrap_or(20));
    let issue_state = if st == "merged" { "none" } else { st };
    let counts = sdb::counts(&state.db, viewer, &pattern).await?;
    let scope = if asked == "first" {
        // the counts ignore the state: a merged filter leaves issues out, which have none
        let kinds: &[(&str, i64)] = &[("issues", counts.issues), ("merge_requests", counts.merge_requests), ("projects", counts.projects)];
        let open = |k: &&(&str, i64)| !(st == "merged" && k.0 == "issues");
        kinds.iter().filter(open).find(|k| k.1 > 0).or_else(|| kinds.iter().find(open)).map_or("issues", |k| k.0)
    } else {
        asked
    };
    let (limit, offset) = if scope == "all" { (5, 0) } else { (per, offset) };
    let mut out = SearchResults { scope: scope.to_owned(), counts, projects: vec![], issues: vec![], merge_requests: vec![] };
    if matches!(scope, "all" | "projects") {
        out.projects = sdb::projects(&state.db, viewer, &pattern, limit, offset).await?;
    }
    if matches!(scope, "all" | "issues") {
        out.issues = sdb::issues(&state.db, viewer, &pattern, issue_state, limit, offset).await?;
    }
    if matches!(scope, "all" | "merge_requests") {
        out.merge_requests = sdb::merge_requests(&state.db, viewer, &pattern, st, limit, offset).await?;
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CodeQuery {
    #[serde(default)]
    q: String,
    /// `q` is a regular expression.
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    case_sensitive: bool,
    /// Only files whose path contains this.
    path: Option<String>,
    /// Only files with exactly this name.
    filename: Option<String>,
    /// Only files in this language or of this type (`Rust`, `Markdown`...):
    /// one of `/search/code/languages`.
    language: Option<String>,
    /// Only this project (id or full path).
    project_id: Option<String>,
    /// Only this group and its subgroups (id or full path).
    group_id: Option<String>,
    /// The `next` of the previous answer, to go on from there.
    after: Option<String>,
    per_page: Option<i64>,
}

/// Files of default branches whose text matches. There is no count: files
/// are read until the page is full.
#[derive(Serialize, ToSchema)]
pub struct CodeResults {
    pub files: Vec<CodeHit>,
    /// Pass it as `after` for the following files; absent at the end.
    pub next: Option<String>,
    /// The search stopped early (too many candidate files, or out of time):
    /// a narrower search may find more.
    pub partial: bool,
    /// Projects of this search whose code is not indexed yet (on the first
    /// page): their files cannot be found for now.
    pub not_indexed: i64,
}

#[derive(Serialize, ToSchema)]
pub struct CodeHit {
    pub project_id: i64,
    pub project_path: String,
    pub path: String,
    /// The default branch.
    pub r#ref: String,
    /// The commit the lines are from.
    pub commit: String,
    pub language: Option<String>,
    /// Lines that match in the whole file; `chunks` shows the first ones.
    pub matches: usize,
    pub chunks: Vec<CodeChunk>,
}

#[derive(Serialize, ToSchema)]
pub struct CodeChunk {
    /// Number of the first line.
    pub start: usize,
    /// One HTML string per line, syntax highlighted, the matches in `<mark>`.
    pub lines: Vec<String>,
}

/// Text inside the files of default branches: exact text (3 characters or
/// more) or a regular expression, in the code the caller may read.
#[utoipa::path(get, path = "/api/v1/search/code", responses((status = 200, body = CodeResults)), tag = "projects")]
pub async fn code(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    ip: ClientIp,
    Query(q): Query<CodeQuery>,
) -> ApiResult<Json<CodeResults>> {
    // reading files is work: anonymous visitors have a budget of it
    crate::auth::heavy_read_allowed(&state, &v, ip)?;
    let params = code::Params {
        text: q.q,
        regex: q.regex,
        case_sensitive: q.case_sensitive,
        path: q.path,
        filename: q.filename,
        language: q.language,
        limit: super::page_window(1, q.per_page.unwrap_or(20)).0 as usize,
        ..code::Params::scoped(&state, &v, q.project_id.as_deref(), q.group_id.as_deref(), q.after.as_deref()).await?
    };
    let found = code::search(&state, &v, params).await?;
    let files = found
        .files
        .into_iter()
        .map(|f| CodeHit {
            chunks: f.chunks.iter().map(|c| CodeChunk { start: c.start, lines: code::chunk_html(&state, &f.path, c) }).collect(),
            project_id: f.project_id,
            project_path: f.project_path,
            path: f.path,
            r#ref: f.branch,
            commit: f.commit,
            language: f.language.map(str::to_string),
            matches: f.matches,
        })
        .collect();
    Ok(Json(CodeResults {
        files,
        next: found.next.as_ref().map(code::encode_cursor),
        partial: found.partial,
        not_indexed: found.not_indexed,
    }))
}

/// What a code search can be limited to with `language`: programming
/// languages and kinds of data, markup and prose, from a file's extension
/// or name.
#[utoipa::path(get, path = "/api/v1/search/code/languages", responses((status = 200, body = Vec<String>)), tag = "projects")]
pub async fn code_languages() -> Json<Vec<&'static str>> {
    Json(crate::languages::names())
}
