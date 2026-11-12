//! Repository browsing (M2). Results keyed by immutable object IDs are cached as
//! rendered JSON (architecture: Caching); ref → commit lookups are never cached.

use std::future::Future;

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use forgust_db::projects::Project;
use forgust_storage::{ArchiveFormat, DiffOptions, EntryKind, RefInfo, TreeEntry};
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;
use utoipa::ToSchema;

use super::project_for;
use crate::auth::Viewer;
use crate::auth::authz::ProjectAction;
use crate::error::{ApiError, ApiResult};
use crate::render;
use crate::state::{AppState, ResponseCache};

fn json_bytes(b: Bytes) -> Response {
    let mut r = Response::new(Body::from(b));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

/// Serve from cache or compute, serialize and store.
pub(crate) async fn cached<T, F, Fut>(state: &AppState, key: String, f: F) -> ApiResult<Response>
where
    T: Serialize,
    F: FnOnce() -> Fut,
    Fut: Future<Output = ApiResult<T>>,
{
    if let Some(b) = state.cache.get(&key).await {
        return Ok(json_bytes(b));
    }
    let v = f().await?;
    let b = Bytes::from(serde_json::to_vec(&v).map_err(|e| ApiError::Internal(e.into()))?);
    state.cache.put(key, b.clone()).await;
    Ok(json_bytes(b))
}

async fn readable(state: &AppState, viewer: &crate::auth::Principal, id: &str) -> ApiResult<Project> {
    project_for(state, viewer, id, ProjectAction::ReadCode).await
}

/// Resolve a branch, tag or commit to a commit ID.
async fn commit_of(state: &AppState, p: &Project, rev: Option<&str>) -> ApiResult<(String, String)> {
    let rev = match rev.filter(|r| !r.is_empty()) {
        Some(r) => r.to_string(),
        None => p.default_branch.clone().ok_or(ApiError::NotFound)?,
    };
    let full = crate::services::edits::qualify(state, p.id, &rev).await?;
    let id = state.storage.resolve(p.id, &full).await?.ok_or(ApiError::NotFound)?;
    Ok((rev, id))
}

#[derive(Deserialize)]
pub struct RefPath {
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub path: String,
}

#[derive(Serialize, ToSchema)]
pub struct RefResolved {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub commit: String,
    pub path: String,
}

#[derive(Deserialize)]
pub struct ResolveQuery {
    /// `<ref>/<path>` as found in `/-/tree/` and `/-/blob/` URLs; refs may contain
    /// slashes, so the longest prefix that names a ref wins.
    pub path: String,
}

/// Split `feature/x/src/main.rs` into ref `feature/x` and path `src/main.rs`.
pub async fn resolve(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<ResolveQuery>,
) -> ApiResult<axum::Json<RefResolved>> {
    let p = readable(&state, &v, &id).await?;
    Ok(axum::Json(split_ref(&state, &p, &q.path).await?))
}

pub async fn split_ref(state: &AppState, p: &Project, ref_and_path: &str) -> ApiResult<RefResolved> {
    let segs: Vec<&str> = ref_and_path.trim_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        let (git_ref, commit) = commit_of(state, p, None).await?;
        return Ok(RefResolved { git_ref, commit, path: String::new() });
    }
    for i in (1..=segs.len().min(16)).rev() {
        let candidate = segs[..i].join("/");
        if !forgust_storage::validate_rev(&candidate) {
            continue;
        }
        for full in [format!("refs/heads/{candidate}"), format!("refs/tags/{candidate}")] {
            if let Some(commit) = state.storage.resolve(p.id, &full).await? {
                return Ok(RefResolved { git_ref: candidate, commit, path: segs[i..].join("/") });
            }
        }
    }
    let first = segs[0];
    if first.len() >= 7
        && first.len() <= 40
        && first.bytes().all(|b| b.is_ascii_hexdigit())
        && let Some(commit) = state.storage.resolve(p.id, first).await?
    {
        return Ok(RefResolved { git_ref: first.to_string(), commit, path: segs[1..].join("/") });
    }
    Err(ApiError::NotFound)
}

#[derive(Serialize, ToSchema)]
pub struct TreeResponse {
    pub commit: String,
    pub path: String,
    pub entries: Vec<TreeEntry>,
    /// Last commit that touched this directory.
    pub last_commit: Option<forgust_storage::CommitInfo>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/tree", responses((status = 200, body = TreeResponse)), tag = "repository")]
pub async fn tree(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let (_, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let path = q.path.trim_matches('/').to_string();
    let key = ResponseCache::key(p.id, "tree", &[&commit, &path]);
    cached(&state, key, || async {
        let entries = state.storage.tree(p.id, &commit, &path).await?;
        let mut last_commit = state.storage.log(p.id, &commit, Some(&path), 0, 1).await?.pop();
        crate::services::people::link_commits(&state, last_commit.as_mut()).await;
        Ok(TreeResponse { commit: commit.clone(), path: path.clone(), entries, last_commit })
    })
    .await
}

#[derive(Serialize, ToSchema)]
pub struct PathCommit {
    pub path: String,
    /// Last commit that touched this entry (none when it could not be found in time).
    pub commit: Option<forgust_storage::CommitInfo>,
}

#[derive(Serialize, ToSchema)]
pub struct LastCommits {
    pub commit: String,
    pub entries: Vec<PathCommit>,
    /// False when the time budget ran out before every entry was found: ask again later.
    pub complete: bool,
}

/// Entries looked up per directory, and the time spent on them per request.
const LAST_COMMITS_MAX: usize = 1000;
const LAST_COMMITS_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// The last commit of each entry of a directory (a `git log -1 -- <path>` per
/// entry, fast with the commit-graph's changed-path filters). Cached per
/// commit once complete.
#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/last_commits", responses((status = 200, body = LastCommits)), tag = "repository")]
pub async fn last_commits(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Response> {
    use futures::StreamExt;
    let p = readable(&state, &v, &id).await?;
    let (_, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let path = q.path.trim_matches('/').to_string();
    let key = ResponseCache::key(p.id, "last_commits", &[&commit, &path]);
    if let Some(b) = state.cache.get(&key).await {
        return Ok(json_bytes(b));
    }
    let entries = state.storage.tree(p.id, &commit, &path).await?;
    let deadline = tokio::time::Instant::now() + LAST_COMMITS_BUDGET;
    // each entry is cached on its own, so a retry after the budget ran out
    // only looks up what is still missing
    let found: Vec<(PathCommit, bool)> = futures::stream::iter(entries.into_iter().take(LAST_COMMITS_MAX))
        .map(|e| {
            let (state, commit) = (&state, &commit);
            async move {
                let key = ResponseCache::key(p.id, "last_commit", &[commit, &e.path]);
                if let Some(c) =
                    state.cache.get(&key).await.and_then(|b| serde_json::from_slice::<Option<forgust_storage::CommitInfo>>(&b).ok())
                {
                    return (PathCommit { path: e.path, commit: c }, true);
                }
                match tokio::time::timeout_at(deadline, state.storage.log(p.id, commit, Some(&e.path), 0, 1)).await {
                    Ok(r) => {
                        let c = r.ok().and_then(|mut v| v.pop());
                        if let Ok(b) = serde_json::to_vec(&c) {
                            state.cache.put(key, Bytes::from(b)).await;
                        }
                        (PathCommit { path: e.path, commit: c }, true)
                    }
                    Err(_) => (PathCommit { path: e.path, commit: None }, false),
                }
            }
        })
        .buffered(8)
        .collect()
        .await;
    let complete = found.iter().all(|(_, done)| *done);
    let mut entries: Vec<PathCommit> = found.into_iter().map(|(e, _)| e).collect();
    crate::services::people::link_commits(&state, entries.iter_mut().filter_map(|e| e.commit.as_mut())).await;
    let body = LastCommits { commit, complete, entries };
    let b = Bytes::from(serde_json::to_vec(&body).map_err(|e| ApiError::Internal(e.into()))?);
    if complete {
        state.cache.put(key, b.clone()).await;
    }
    Ok(json_bytes(b))
}

#[derive(Serialize, ToSchema)]
pub struct RepoStats {
    pub commit: String,
    /// Git objects on disk (loose and packed).
    pub repository_bytes: u64,
    pub lfs_bytes: i64,
    pub lfs_objects: i64,
    /// By file extension, largest first (documentation and vendored paths left out).
    pub languages: Vec<crate::languages::LanguageShare>,
    /// The tree was too large to count every file.
    pub languages_truncated: bool,
    /// Commits reachable from `commit`.
    pub commit_count: u64,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/stats", responses((status = 200, body = RepoStats)), tag = "repository")]
pub async fn stats(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Json<RepoStats>> {
    let p = readable(&state, &v, &id).await?;
    let (_, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    // the language count only depends on the commit
    let key = ResponseCache::key(p.id, "languages", &[&commit]);
    let (languages, languages_truncated): (Vec<crate::languages::LanguageShare>, bool) =
        match state.cache.get(&key).await.and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(v) => v,
            None => {
                let (files, truncated) = state.storage.tree_sizes(p.id, &commit, 1_000_000).await?;
                let v = (crate::languages::breakdown(files.iter().map(|(p, s)| (p.as_str(), *s))), truncated);
                if let Ok(b) = serde_json::to_vec(&v) {
                    state.cache.put(key, Bytes::from(b)).await;
                }
                v
            }
        };
    let count_key = ResponseCache::key(p.id, "commit_count", &[&commit]);
    let (repository_bytes, (lfs_objects, lfs_bytes), commit_count) = tokio::try_join!(
        async { state.storage.disk_size(p.id).await.map_err(ApiError::from) },
        async { Ok::<_, ApiError>(forgust_db::lfs::project_usage(&state.db, p.id).await?) },
        async {
            // one walk of the history per commit
            if let Some(n) = state.cache.get(&count_key).await.and_then(|b| std::str::from_utf8(&b).ok()?.parse().ok()) {
                return Ok::<u64, ApiError>(n);
            }
            let n = state.storage.commit_count(p.id, &commit).await?;
            state.cache.put(count_key.clone(), Bytes::from(n.to_string())).await;
            Ok(n)
        },
    )?;
    Ok(Json(RepoStats { commit, repository_bytes, lfs_bytes, lfs_objects, languages, languages_truncated, commit_count }))
}

#[derive(Serialize, ToSchema)]
pub struct LfsInfo {
    pub oid: String,
    pub size: u64,
    /// The object was uploaded and can be downloaded.
    pub stored: bool,
}

#[derive(Serialize, ToSchema)]
pub struct BlobResponse {
    pub commit: String,
    pub path: String,
    pub name: String,
    pub id: String,
    pub size: u64,
    pub kind: EntryKind,
    pub binary: bool,
    /// Larger than the render limit: only the raw download is offered.
    pub too_large: bool,
    pub image: bool,
    pub language: String,
    /// Highlighted HTML, one entry per line.
    pub lines: Vec<String>,
    /// Rendered Markdown, for `.md` files.
    pub markdown_html: Option<String>,
    pub lfs: Option<LfsInfo>,
}

fn is_binary(data: &[u8]) -> bool {
    data.iter().take(8000).any(|&b| b == 0)
}

async fn render_blob(state: &AppState, p: &Project, git_ref: &str, commit: &str, path: &str) -> ApiResult<BlobResponse> {
    let limit = state.cfg.limits.max_render_bytes;
    let blob = state.storage.blob(p.id, commit, path, limit).await?;
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let image = render::is_image(path);
    let lfs = match crate::lfs::parse_pointer(&blob.data) {
        Some((oid, size)) => Some(LfsInfo { stored: forgust_db::lfs::linked_size(&state.db, p.id, &oid).await?.is_some(), oid, size }),
        None => None,
    };
    let binary = lfs.is_some() || is_binary(&blob.data) || blob.kind == EntryKind::Submodule;
    let too_large = blob.truncated;
    let (mut lines, mut markdown_html, mut language) = (Vec::new(), None, String::new());
    if !binary && !too_large {
        let text = String::from_utf8_lossy(&blob.data).into_owned();
        let hl = state.highlighter.clone();
        let (pth, txt) = (path.to_string(), text.clone());
        let (l, lang) =
            tokio::task::spawn_blocking(move || (hl.highlight_lines(&pth, &txt), hl.language(&pth, txt.lines().next().unwrap_or(""))))
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
        lines = l;
        language = lang;
        if render::is_markdown(path) {
            let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            markdown_html = Some(render::markdown_with(&text, &format!("/{}", p.full_path), git_ref, dir, state.images.as_ref()));
        }
    }
    Ok(BlobResponse {
        commit: commit.to_string(),
        path: path.to_string(),
        name,
        id: blob.id,
        size: blob.size,
        kind: blob.kind,
        binary,
        too_large,
        image,
        language,
        lines,
        markdown_html,
        lfs,
    })
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/blob", responses((status = 200, body = BlobResponse)), tag = "repository")]
pub async fn blob(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let (git_ref, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let path = q.path.trim_matches('/').to_string();
    if path.is_empty() {
        return Err(ApiError::bad("path is required"));
    }
    // markdown links depend on the ref name, so it is part of the key
    let key = ResponseCache::key(p.id, "blob", &[&commit, &path, &git_ref]);
    cached(&state, key, || render_blob(&state, &p, &git_ref, &commit, &path)).await
}

/// Raw file. Never served as HTML; LFS pointers resolve to the stored object.
pub async fn raw(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let (_, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    raw_response(&state, &p, &commit, q.path.trim_matches('/')).await
}

pub async fn raw_response(state: &AppState, p: &Project, commit: &str, path: &str) -> ApiResult<Response> {
    let max = state.cfg.limits.max_raw_bytes.min(256 << 20) as usize;
    let blob = state.storage.blob(p.id, commit, path, max).await?;
    if blob.truncated {
        return Err(ApiError::TooLarge("file is too large to serve raw; clone the repository".into()));
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let ctype = safe_content_type(name, &blob.data);
    let mut resp = if let Some((oid, size)) = crate::lfs::parse_pointer(&blob.data) {
        if forgust_db::lfs::linked_size(&state.db, p.id, &oid).await?.is_none() {
            return Err(ApiError::NotFound);
        }
        let (_, stream) = state.blobs.get(&crate::blob::lfs_key(&oid)).await?.ok_or(ApiError::NotFound)?;
        let mut r = Response::new(Body::from_stream(stream));
        r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(size));
        r
    } else {
        Response::new(Body::from(blob.data))
    };
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(&ctype).unwrap_or(HeaderValue::from_static("application/octet-stream")));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=60"));
    Ok(resp)
}

/// Images are served as images; everything else that looks like text is
/// text/plain, never HTML.
fn safe_content_type(name: &str, data: &[u8]) -> String {
    let guess = mime_guess::from_path(name).first_raw().unwrap_or("");
    match guess {
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/x-icon" | "image/avif" => guess.to_string(),
        "image/svg+xml" => "image/svg+xml".into(),
        "application/pdf" => guess.to_string(),
        _ if is_binary(data) => "application/octet-stream".into(),
        _ => "text/plain; charset=utf-8".into(),
    }
}

#[derive(Serialize, ToSchema)]
pub struct ReadmeResponse {
    pub path: String,
    pub html: String,
}

pub async fn readme(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let (git_ref, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let dir = q.path.trim_matches('/').to_string();
    let key = ResponseCache::key(p.id, "readme", &[&commit, &dir, &git_ref]);
    cached(&state, key, || async {
        let entries = state.storage.tree(p.id, &commit, &dir).await?;
        let mut candidates: Vec<&TreeEntry> =
            entries.iter().filter(|e| e.kind != EntryKind::Tree && e.name.to_ascii_lowercase().starts_with("readme")).collect();
        candidates.sort_by_key(|e| (!render::is_markdown(&e.name), e.name.len()));
        let Some(e) = candidates.first() else { return Ok(None) };
        let blob = state.storage.blob(p.id, &commit, &e.path, state.cfg.limits.max_render_bytes).await?;
        let text = String::from_utf8_lossy(&blob.data);
        let html = if render::is_markdown(&e.name) {
            render::markdown_with(&text, &format!("/{}", p.full_path), &git_ref, &dir, state.images.as_ref())
        } else {
            format!("<pre>{}</pre>", render::escape_html(&text))
        };
        Ok(Some(ReadmeResponse { path: e.path.clone(), html }))
    })
    .await
}

#[derive(Deserialize)]
pub struct ArchiveQuery {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    #[serde(default = "tgz")]
    format: String,
}

fn tgz() -> String {
    "tar.gz".into()
}

pub async fn archive(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    ip: crate::auth::ClientIp,
    Path(id): Path<String>,
    Query(q): Query<ArchiveQuery>,
) -> ApiResult<Response> {
    crate::auth::heavy_read_allowed(&state, &v, ip)?;
    let p = readable(&state, &v, &id).await?;
    let (git_ref, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let format = ArchiveFormat::from_ext(&q.format).ok_or_else(|| ApiError::bad("format must be tar.gz, tar or zip"))?;
    let safe_ref: String =
        git_ref.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '-' }).collect();
    let prefix = format!("{}-{}", p.path, safe_ref);
    let mut proc = state.storage.archive(p.id, &commit, format, &prefix).await?;
    let stdout = std::mem::replace(&mut proc.stdout, Box::pin(tokio::io::empty()));
    tokio::spawn(async move {
        let _ = proc.wait().await;
    });
    let mut r = Response::new(Body::from_stream(ReaderStream::new(stdout)));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(format.content_type()));
    r.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&crate::blob::disposition(&format!("{prefix}.{}", format.ext()), false))
            .unwrap_or(HeaderValue::from_static("attachment")),
    );
    Ok(r)
}

#[derive(Deserialize)]
pub struct CommitsQuery {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    path: String,
    #[serde(default = "one")]
    page: usize,
    #[serde(default = "per")]
    per_page: usize,
}

fn one() -> usize {
    1
}
fn per() -> usize {
    40
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/commits", responses((status = 200, body = [forgust_storage::CommitInfo])), tag = "repository")]
pub async fn commits(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    ip: crate::auth::ClientIp,
    Path(id): Path<String>,
    Query(q): Query<CommitsQuery>,
) -> ApiResult<Response> {
    crate::auth::heavy_read_allowed(&state, &v, ip)?;
    let p = readable(&state, &v, &id).await?;
    let (_, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let per_page = q.per_page.clamp(1, 100);
    let skip = (q.page.max(1) - 1) * per_page;
    let path = q.path.trim_matches('/').to_string();
    let key = ResponseCache::key(p.id, "log", &[&commit, &path, &skip.to_string(), &per_page.to_string()]);
    cached(&state, key, || async {
        let mut log = state.storage.log(p.id, &commit, if path.is_empty() { None } else { Some(&path) }, skip, per_page).await?;
        crate::services::people::link_commits(&state, &mut log).await;
        Ok(log)
    })
    .await
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/commits/{sha}", responses((status = 200, body = forgust_storage::CommitInfo)), tag = "repository")]
pub async fn commit(State(state): State<AppState>, Viewer(v): Viewer, Path((id, sha)): Path<(String, String)>) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let full = state.storage.resolve(p.id, &sha).await?.ok_or(ApiError::NotFound)?;
    let key = ResponseCache::key(p.id, "commit", &[&full]);
    cached(&state, key, || async {
        let mut c = state.storage.commit(p.id, &full).await?.ok_or(ApiError::NotFound)?;
        crate::services::people::link_commits(&state, [&mut c]).await;
        Ok(c)
    })
    .await
}

#[derive(Deserialize)]
pub struct DiffQuery {
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

impl DiffQuery {
    fn options(&self, state: &AppState) -> DiffOptions {
        let max = state.cfg.limits.max_diff_files_per_page;
        DiffOptions {
            offset: self.offset,
            limit: self.limit.unwrap_or(max).clamp(1, max),
            max_lines_per_file: state.cfg.limits.max_diff_lines_per_file,
            paths: None,
        }
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/commits/{sha}/diff", responses((status = 200, body = forgust_storage::Diff)), tag = "repository")]
pub async fn commit_diff(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, sha)): Path<(String, String)>,
    Query(q): Query<DiffQuery>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let full = state.storage.resolve(p.id, &sha).await?.ok_or(ApiError::NotFound)?;
    let opts = q.options(&state);
    let key = ResponseCache::key(p.id, "cdiff", &[&full, &opts.offset.to_string(), &opts.limit.to_string()]);
    cached(&state, key, || async {
        let info = state.storage.commit(p.id, &full).await?.ok_or(ApiError::NotFound)?;
        let parent = info.parents.first().cloned();
        let mut d = state.storage.diff(p.id, parent.as_deref(), &full, opts).await?;
        highlight_diff(&state, p.id, &mut d).await;
        Ok(d)
    })
    .await
}

#[derive(Deserialize)]
pub struct CompareQuery {
    from: String,
    to: String,
    /// `true`: diff `from..to` directly; default compares from the merge base.
    #[serde(default)]
    straight: bool,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct CompareResponse {
    pub from: String,
    pub to: String,
    pub merge_base: Option<String>,
    pub commits: Vec<forgust_storage::CommitInfo>,
    pub diff: forgust_storage::Diff,
}

pub async fn compare(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<CompareQuery>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let from =
        state.storage.resolve(p.id, &crate::services::edits::qualify(&state, p.id, &q.from).await?).await?.ok_or(ApiError::NotFound)?;
    let to = state.storage.resolve(p.id, &crate::services::edits::qualify(&state, p.id, &q.to).await?).await?.ok_or(ApiError::NotFound)?;
    let opts = DiffQuery { offset: q.offset, limit: q.limit }.options(&state);
    let key =
        ResponseCache::key(p.id, "compare", &[&from, &to, &q.straight.to_string(), &opts.offset.to_string(), &opts.limit.to_string()]);
    cached(&state, key, || async {
        let base = state.storage.merge_base(p.id, &from, &to).await?;
        let diff_from = if q.straight { Some(from.clone()) } else { base.clone().or(Some(from.clone())) };
        let mut commits = state.storage.log_between(p.id, &from, &to, 250).await?;
        crate::services::people::link_commits(&state, &mut commits).await;
        let mut diff = state.storage.diff(p.id, diff_from.as_deref(), &to, opts).await?;
        highlight_diff(&state, p.id, &mut diff).await;
        Ok(CompareResponse { from: from.clone(), to: to.clone(), merge_base: base, commits, diff })
    })
    .await
}

#[derive(Serialize, ToSchema)]
pub struct BranchDto {
    pub name: String,
    pub commit: Option<forgust_storage::CommitSummary>,
    pub default: bool,
    pub protected: bool,
}

#[derive(Deserialize)]
pub struct RefsQuery {
    #[serde(default)]
    search: String,
    #[serde(default = "one")]
    page: usize,
    #[serde(default = "per")]
    per_page: usize,
}

fn page_refs(refs: Vec<RefInfo>, prefix: &str, q: &RefsQuery) -> (Vec<RefInfo>, usize) {
    let search = q.search.to_lowercase();
    let filtered: Vec<RefInfo> = refs
        .into_iter()
        .filter(|r| search.is_empty() || r.name.strip_prefix(prefix).unwrap_or(&r.name).to_lowercase().contains(&search))
        .collect();
    let total = filtered.len();
    let per = q.per_page.clamp(1, 100);
    (filtered.into_iter().skip((q.page.max(1) - 1) * per).take(per).collect(), total)
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/branches", responses((status = 200, body = [BranchDto])), tag = "repository")]
pub async fn branches(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefsQuery>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let refs = state.storage.refs(p.id, "refs/heads/").await?;
    let (page, total) = page_refs(refs, "refs/heads/", &q);
    let rules = forgust_db::protected_branches::list(&state.db, p.id).await?;
    let out: Vec<BranchDto> = page
        .into_iter()
        .map(|r| {
            let name = r.name.trim_start_matches("refs/heads/").to_string();
            let protected = rules.iter().any(|rule| rule.matches(&name));
            BranchDto { default: Some(&name) == p.default_branch.as_ref(), protected, name, commit: r.commit }
        })
        .collect();
    let mut resp = axum::Json(out).into_response();
    resp.headers_mut().insert("x-total", HeaderValue::from(total));
    Ok(resp)
}

#[derive(Serialize, ToSchema)]
pub struct TagDto {
    pub name: String,
    pub target: String,
    pub annotated: bool,
    pub commit: Option<forgust_storage::CommitSummary>,
    /// The name of the tag's release, if it has one.
    pub release: Option<String>,
    /// A protected tag rule matches it.
    pub protected: bool,
}

pub async fn tags(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefsQuery>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let refs = state.storage.refs(p.id, "refs/tags/").await?;
    let (page, total) = page_refs(refs, "refs/tags/", &q);
    let names: Vec<String> = page.iter().map(|r| r.name.trim_start_matches("refs/tags/").to_string()).collect();
    let releases = forgust_db::releases::for_tags(&state.db, p.id, &names).await?;
    let rules = forgust_db::protected_branches::tags(&state.db, p.id).await?;
    let out: Vec<TagDto> = page
        .into_iter()
        .map(|r| {
            let name = r.name.trim_start_matches("refs/tags/").to_string();
            TagDto {
                release: releases.iter().find(|x| x.tag_name == name).map(|x| x.name.clone()),
                protected: rules.iter().any(|r| r.matches(&name)),
                name,
                annotated: r.peeled.is_some(),
                target: r.target,
                commit: r.commit,
            }
        })
        .collect();
    let mut resp = axum::Json(out).into_response();
    resp.headers_mut().insert("x-total", HeaderValue::from(total));
    Ok(resp)
}

#[derive(Serialize, ToSchema)]
pub struct BlameResponse {
    pub commit: String,
    pub path: String,
    pub hunks: Vec<forgust_storage::BlameHunk>,
    /// Highlighted HTML per line of the file.
    pub lines: Vec<String>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/blame", responses((status = 200, body = BlameResponse)), tag = "repository")]
pub async fn blame(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    ip: crate::auth::ClientIp,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Response> {
    crate::auth::heavy_read_allowed(&state, &v, ip)?;
    let p = readable(&state, &v, &id).await?;
    let (_, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let path = q.path.trim_matches('/').to_string();
    let key = ResponseCache::key(p.id, "blame", &[&commit, &path]);
    cached(&state, key, || async {
        let blob = state.storage.blob(p.id, &commit, &path, state.cfg.limits.max_render_bytes).await?;
        if blob.truncated || is_binary(&blob.data) {
            return Err(ApiError::TooLarge("blame is not available for binary or very large files".into()));
        }
        let hunks = state.storage.blame(p.id, &commit, &path).await?;
        let text = String::from_utf8_lossy(&blob.data).into_owned();
        let hl = state.highlighter.clone();
        let pth = path.clone();
        let lines = tokio::task::spawn_blocking(move || hl.highlight_lines(&pth, &text)).await.map_err(|e| ApiError::Internal(e.into()))?;
        Ok(BlameResponse { commit: commit.clone(), path: path.clone(), hunks, lines })
    })
    .await
}

/// Highlighted lines of one blob, cached by blob id (and file name, which picks
/// the grammar). None for binary or oversized blobs.
async fn highlighted_blob(
    state: &AppState,
    project_id: i64,
    commit: &str,
    path: &str,
    oid: Option<&str>,
) -> Option<std::sync::Arc<Vec<String>>> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let key = oid.map(|o| ResponseCache::key(project_id, "hlblob", &[o, name]));
    if let Some(k) = &key
        && let Some(b) = state.cache.get(k).await
    {
        return serde_json::from_slice::<Vec<String>>(&b).ok().map(std::sync::Arc::new);
    }
    let blob = state.storage.blob(project_id, commit, path, state.cfg.limits.max_render_bytes).await.ok()?;
    if blob.truncated || is_binary(&blob.data) || crate::lfs::parse_pointer(&blob.data).is_some() {
        return None;
    }
    let text = String::from_utf8_lossy(&blob.data).into_owned();
    let hl = state.highlighter.clone();
    let p = path.to_string();
    let lines = tokio::task::spawn_blocking(move || hl.highlight_lines(&p, &text)).await.ok()?;
    if let Some(k) = key {
        state.cache.put(k, Bytes::from(serde_json::to_vec(&lines).unwrap_or_default())).await;
    }
    Some(std::sync::Arc::new(lines))
}

/// Fill `DiffLine::html` from the highlighted full old and new files, so tokens
/// that span hunk boundaries (strings, comments) are coloured correctly.
pub async fn highlight_diff(state: &AppState, project_id: i64, diff: &mut forgust_storage::Diff) {
    use futures::StreamExt;
    let (from, to) = (diff.from.clone(), diff.to.clone());
    // every file's old and new side highlighted first, a few files at a time
    // (both sides of one at once), then laid onto the lines
    let wanted: Vec<_> = diff
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| !(f.binary || f.collapsed || f.hunks.is_empty()))
        .map(|(i, f)| {
            let old = f.old_oid.clone().filter(|_| f.status != "added").map(|oid| (f.old_path.clone(), oid));
            let new = f.new_oid.clone().filter(|_| f.status != "deleted").map(|oid| (f.new_path.clone(), oid));
            let (from, to) = (&from, &to);
            async move {
                let (old, new) = tokio::join!(
                    async {
                        match &old {
                            Some((path, oid)) => highlighted_blob(state, project_id, from, path, Some(oid)).await,
                            None => None,
                        }
                    },
                    async {
                        match &new {
                            Some((path, oid)) => highlighted_blob(state, project_id, to, path, Some(oid)).await,
                            None => None,
                        }
                    },
                );
                (i, old, new)
            }
        })
        .collect();
    let sides: Vec<_> = futures::stream::iter(wanted).buffered(8).collect().await;
    for (i, old, new) in sides {
        let f = &mut diff.files[i];
        for h in f.hunks.iter_mut() {
            for l in h.lines.iter_mut() {
                let pick = |lines: &Option<std::sync::Arc<Vec<String>>>, n: Option<u32>| {
                    lines.as_ref().zip(n).and_then(|(ls, n)| ls.get(n as usize - 1).cloned())
                };
                l.html = match l.kind {
                    forgust_storage::DiffLineKind::Del => pick(&old, l.old),
                    forgust_storage::DiffLineKind::Add | forgust_storage::DiffLineKind::Context => {
                        pick(&new, l.new).or_else(|| pick(&old, l.old))
                    }
                    forgust_storage::DiffLineKind::NoNewline => None,
                };
            }
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct FileList {
    pub commit: String,
    pub paths: Vec<String>,
    /// More files exist than were listed.
    pub truncated: bool,
}

/// Every file path at a ref, for the "go to file" finder.
pub async fn files(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let (_, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let key = ResponseCache::key(p.id, "files", &[&commit]);
    cached(&state, key, || async {
        let (paths, truncated) = state.storage.list_paths(p.id, &commit, 100_000).await?;
        Ok(FileList { commit: commit.clone(), paths, truncated })
    })
    .await
}

#[derive(Deserialize, ToSchema)]
pub struct MarkdownBody {
    pub text: String,
}

#[derive(Serialize, ToSchema)]
pub struct MarkdownHtml {
    pub html: String,
}

/// Comment previews: the same sanitized renderer the server uses everywhere.
pub async fn markdown(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    axum::Json(b): axum::Json<MarkdownBody>,
) -> ApiResult<axum::Json<MarkdownHtml>> {
    let p = readable(&state, &v, &id).await?;
    if b.text.len() > 200_000 {
        return Err(ApiError::TooLarge("text is too long".into()));
    }
    let r = p.default_branch.clone().unwrap_or_else(|| "main".into());
    // previews are of comments and descriptions: references link as they will
    Ok(axum::Json(MarkdownHtml { html: crate::services::refs::render(&state, &p, &b.text, &r, true).await }))
}

#[derive(Serialize, ToSchema)]
pub struct DescriptionTemplate {
    /// The file name without `.md`; `Default` fills new descriptions by itself.
    pub name: String,
    pub content: String,
}

/// Description templates on the default branch: `merge_request` from
/// `.gitlab/merge_request_templates/*.md` (or GitHub's pull request template),
/// `issue` from `.gitlab/issue_templates/*.md` (or `.github/ISSUE_TEMPLATE/*.md`).
#[utoipa::path(get, path = "/api/v1/projects/{id}/templates/{kind}", responses((status = 200, body = [DescriptionTemplate])), tag = "repository")]
pub async fn templates(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, kind)): Path<(String, String)>,
) -> ApiResult<axum::Json<Vec<DescriptionTemplate>>> {
    const MAX: usize = 50;
    const MAX_BYTES: usize = 64 << 10;
    let p = readable(&state, &v, &id).await?;
    let (dirs, files): (&[&str], &[&str]) = match kind.as_str() {
        "merge_request" => (
            &[".gitlab/merge_request_templates", ".github/PULL_REQUEST_TEMPLATE"],
            &[".github/pull_request_template.md", ".github/PULL_REQUEST_TEMPLATE.md", "docs/pull_request_template.md"],
        ),
        "issue" => (&[".gitlab/issue_templates", ".github/ISSUE_TEMPLATE"], &[".github/issue_template.md", ".github/ISSUE_TEMPLATE.md"]),
        _ => return Err(ApiError::bad("kind is merge_request or issue")),
    };
    let Ok((_, commit)) = commit_of(&state, &p, None).await else { return Ok(axum::Json(Vec::new())) };
    let mut paths: Vec<(String, String)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = state.storage.tree(p.id, &commit, dir).await else { continue };
        for e in entries
            .into_iter()
            .filter(|e| matches!(e.kind, EntryKind::Blob | EntryKind::Executable) && e.name.to_ascii_lowercase().ends_with(".md"))
        {
            let name = e.name[..e.name.len() - 3].to_string();
            if !paths.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                paths.push((name, e.path));
            }
        }
    }
    for f in files {
        if paths.is_empty() && state.storage.blob(p.id, &commit, f, 1).await.is_ok() {
            paths.push(("Default".into(), f.to_string()));
        }
    }
    paths.truncate(MAX);
    let mut out = Vec::new();
    for (name, path) in paths {
        if let Ok(b) = state.storage.blob(p.id, &commit, &path, MAX_BYTES).await
            && !b.truncated
            && let Ok(content) = String::from_utf8(b.data)
        {
            out.push(DescriptionTemplate { name, content });
        }
    }
    // Default first, then by name
    out.sort_by_key(|t| (!t.name.eq_ignore_ascii_case("default"), t.name.to_lowercase()));
    Ok(axum::Json(out))
}

#[derive(Deserialize, ToSchema)]
pub struct NewBranch {
    pub branch: String,
    /// A branch, tag or commit to start from.
    #[serde(rename = "ref")]
    pub from: String,
}

/// Create a branch (Developer+; protected patterns apply as for a push).
#[utoipa::path(post, path = "/api/v1/projects/{id}/repository/branches", request_body = NewBranch, responses((status = 200)), tag = "repository")]
pub async fn create_branch(
    State(state): State<AppState>,
    crate::auth::SignedIn(_, v): crate::auth::SignedIn,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<NewBranch>,
) -> ApiResult<axum::Json<serde_json::Value>> {
    let p = readable(&state, &v, &id).await?;
    let sha = crate::services::edits::create_branch(&state, &v, &p, &req.branch, &req.from).await?;
    Ok(axum::Json(serde_json::json!({ "name": req.branch.trim(), "commit": sha })))
}

pub async fn delete_branch(
    State(state): State<AppState>,
    crate::auth::SignedIn(_, v): crate::auth::SignedIn,
    Path((id, branch)): Path<(String, String)>,
) -> ApiResult<axum::http::StatusCode> {
    let p = readable(&state, &v, &id).await?;
    crate::services::edits::delete_branch(&state, &v, &p, &branch).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct NewTag {
    pub tag_name: String,
    /// A branch, tag or commit to tag.
    #[serde(rename = "ref")]
    pub from: String,
    /// Makes an annotated tag.
    pub message: Option<String>,
}

/// Create a tag (Developer+); a message makes it annotated.
#[utoipa::path(post, path = "/api/v1/projects/{id}/repository/tags", request_body = NewTag, responses((status = 200, body = crate::services::edits::CreatedTag)), tag = "repository")]
pub async fn create_tag(
    State(state): State<AppState>,
    crate::auth::SignedIn(_, v): crate::auth::SignedIn,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<NewTag>,
) -> ApiResult<axum::Json<crate::services::edits::CreatedTag>> {
    let p = readable(&state, &v, &id).await?;
    Ok(axum::Json(crate::services::edits::create_tag(&state, &v, &p, &req.tag_name, &req.from, req.message.as_deref()).await?))
}

/// Delete a tag; a release on it is deleted too.
#[utoipa::path(delete, path = "/api/v1/projects/{id}/repository/tags/{tag}", responses((status = 204)), tag = "repository")]
pub async fn delete_tag(
    State(state): State<AppState>,
    crate::auth::SignedIn(_, v): crate::auth::SignedIn,
    Path((id, tag)): Path<(String, String)>,
) -> ApiResult<axum::http::StatusCode> {
    let p = readable(&state, &v, &id).await?;
    crate::services::edits::delete_tag(&state, &v, &p, &tag).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct CommitBody {
    pub branch: String,
    /// Where a new `branch` starts (default: the default branch).
    pub start_branch: Option<String>,
    pub commit_message: String,
    pub actions: Vec<crate::services::edits::FileAction>,
    /// The branch head the editor loaded; the commit is refused if it moved.
    pub expected_head: Option<String>,
    /// With `start_branch` on an existing branch: replace that branch.
    #[serde(default)]
    pub force: bool,
}

/// Commit file changes made in the browser (GitLab's shape).
#[utoipa::path(post, path = "/api/v1/projects/{id}/repository/commits", request_body = CommitBody, responses((status = 200, body = crate::services::edits::Committed)), tag = "repository")]
pub async fn create_commit(
    State(state): State<AppState>,
    crate::auth::SignedIn(_, v): crate::auth::SignedIn,
    Path(id): Path<String>,
    axum::Json(req): axum::Json<CommitBody>,
) -> ApiResult<axum::Json<crate::services::edits::Committed>> {
    let p = readable(&state, &v, &id).await?;
    let c = crate::services::edits::commit(
        &state,
        &v,
        &p,
        crate::services::edits::CommitRequest {
            branch: &req.branch,
            start_branch: req.start_branch.as_deref(),
            message: &req.commit_message,
            actions: &req.actions,
            expected_head: req.expected_head.as_deref(),
            force: req.force,
        },
    )
    .await?;
    Ok(axum::Json(c))
}
