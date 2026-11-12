//! Version, users, projects, files, branches, commits and commit statuses.

use axum::Json;
use axum::body::Body;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use forgust_db::projects::Project;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{PageQuery, json as j, paged};
use crate::api::{group_for, project_for};
use crate::auth::authz::{GroupAction, ProjectAction};
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub async fn version() -> Json<Value> {
    Json(
        json!({ "version": format!("{}-forgust", super::VERSION), "revision": env!("CARGO_PKG_VERSION"), "kas": { "enabled": false }, "enterprise": false }),
    )
}

pub async fn me(State(state): State<AppState>, SignedIn(_, p): SignedIn) -> ApiResult<Json<Value>> {
    let u = p.direct_user().ok_or(ApiError::Unauthorized)?;
    Ok(Json(j::me(&state.cfg, u)))
}

#[derive(Deserialize)]
pub struct UsersQuery {
    username: Option<String>,
    search: Option<String>,
}

pub async fn users(State(state): State<AppState>, SignedIn(_, _p): SignedIn, Query(q): Query<UsersQuery>) -> ApiResult<Json<Vec<Value>>> {
    let list = match (&q.username, &q.search) {
        (Some(name), _) => forgust_db::users::find_by_username(&state.db, name).await?.into_iter().collect(),
        (None, Some(s)) => forgust_db::users::list(&state.db, s, false, false, 20, 0).await?,
        _ => Vec::new(),
    };
    Ok(Json(list.iter().filter(|u| u.state == "active").map(|u| j::user(&state.cfg, u)).collect()))
}

/// Renovate asks whether reviewers are busy; nobody is.
pub async fn user_status() -> Json<Value> {
    Json(json!({ "availability": "not_set", "emoji": null, "message": null, "message_html": "", "clear_status_at": null }))
}

#[derive(Deserialize, Default)]
pub struct ProjectsQuery {
    membership: Option<bool>,
    owned: Option<bool>,
    min_access_level: Option<i16>,
    #[serde(default)]
    search: String,
    include_subgroups: Option<bool>,
}

async fn empty(state: &AppState, p: &Project) -> bool {
    state.storage.is_empty(p.id).await.unwrap_or(true)
}

async fn render(state: &AppState, list: &[Project]) -> Vec<Value> {
    // one git check per project: a few at a time
    let checks: Vec<_> = list.iter().map(|p| empty(state, p)).collect();
    let empties: Vec<bool> = futures::stream::iter(checks).buffered(8).collect().await;
    list.iter().zip(empties).map(|(p, e)| j::project(&state.cfg, p, e)).collect()
}

pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<ProjectsQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let pg = &pg;
    let user = v.direct_user();
    let projects: Vec<Project> = match user {
        Some(u) if q.membership.unwrap_or(false) || q.owned.unwrap_or(false) || q.min_access_level.is_some() => {
            let min = q.min_access_level.unwrap_or(0).max(if q.owned.unwrap_or(false) { forgust_db::access::OWNER } else { 0 });
            forgust_db::projects::for_user(&state.db, u.id, &q.search, pg.probe(), pg.offset())
                .await?
                .into_iter()
                .filter(|(_, level)| *level >= min)
                .map(|(p, _)| p)
                .collect()
        }
        _ => forgust_db::projects::explore(&state.db, v.sees_internal(), &q.search, pg.probe(), pg.offset()).await?,
    };
    let items = render(&state, &projects).await;
    Ok(paged(&state, &uri, pg, items, None))
}

/// Projects of a group; with `include_subgroups`, of its subgroups too (those the user is a member of).
pub async fn group_projects(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<ProjectsQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let ns = group_for(&state, &v, &id, GroupAction::Read).await?;
    let mut list =
        forgust_db::projects::in_namespace_visible(&state.db, ns.id, v.direct_user().map(|u| u.id), v.is_admin(), v.sees_internal())
            .await?;
    if q.include_subgroups.unwrap_or(false)
        && let Some(u) = v.direct_user()
    {
        let prefix = format!("{}/", ns.full_path.to_lowercase());
        for (p, _) in forgust_db::projects::for_user(&state.db, u.id, "", 10_000, 0).await? {
            if p.full_path.to_lowercase().starts_with(&prefix) && !list.iter().any(|x| x.id == p.id) {
                list.push(p);
            }
        }
    }
    let search = q.search.to_lowercase();
    list.retain(|p| search.is_empty() || p.full_path.to_lowercase().contains(&search));
    list.sort_by(|a, b| a.full_path.cmp(&b.full_path));
    let total = list.len() as i64;
    let page: Vec<Project> = list.into_iter().skip(pg.offset() as usize).take(pg.per_page() as usize).collect();
    let items = render(&state, &page).await;
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

pub async fn show(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let mut out = j::project(&state.cfg, &p, empty(&state, &p).await);
    // what the caller may do, as GitLab reports it
    let level = crate::auth::authz::project_role(&state, &v, &p).await?.level;
    out["permissions"] =
        json!({ "project_access": if level > 0 { json!({ "access_level": level }) } else { Value::Null }, "group_access": null });
    Ok(Json(out))
}

pub async fn members(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let all = forgust_db::members::list(&state.db, "project", p.id).await?;
    member_page(&state, &uri, &pg, all)
}

/// Direct and inherited members (from the project's groups); the highest role wins.
pub async fn members_all(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let mut all = forgust_db::members::list(&state.db, "project", p.id).await?;
    for ns in &p.traversal_ids {
        for m in forgust_db::members::list(&state.db, "namespace", *ns).await? {
            match all.iter_mut().find(|x| x.user_id == m.user_id) {
                Some(x) if x.access_level < m.access_level => x.access_level = m.access_level,
                Some(_) => {}
                None => all.push(m),
            }
        }
    }
    all.sort_by(|a, b| a.username.cmp(&b.username));
    member_page(&state, &uri, &pg, all)
}

fn member_page(state: &AppState, uri: &axum::http::Uri, pg: &PageQuery, all: Vec<forgust_db::members::Member>) -> ApiResult<Response> {
    let total = all.len() as i64;
    let items: Vec<Value> = all
        .iter()
        .skip(pg.offset() as usize)
        .take(pg.per_page() as usize)
        .map(|m| {
            json!({ "id": m.user_id, "username": m.username, "name": if m.name.is_empty() { &m.username } else { &m.name },
                    "state": "active", "access_level": m.access_level, "expires_at": m.expires_at,
                    "web_url": format!("{}/{}", j::base(&state.cfg), m.username), "avatar_url": null })
        })
        .collect();
    Ok(paged(state, uri, pg, items, Some(total)))
}

// ------------------------------------------------------------------ files

#[derive(Deserialize)]
pub struct RefQuery {
    #[serde(rename = "ref")]
    git_ref: Option<String>,
}

/// The commit a `ref` names (branch, tag, sha or HEAD).
async fn commit_of(state: &AppState, p: &Project, r: Option<&str>) -> ApiResult<String> {
    crate::services::edits::commit_of(state, p, r).await?.1.ok_or(ApiError::NotFound)
}

const MAX_FILE: usize = 10 << 20;

async fn read_file(
    state: &AppState,
    v: &crate::auth::Principal,
    id: &str,
    file: &str,
    r: Option<&str>,
) -> ApiResult<(Project, String, forgust_storage::Blob)> {
    let p = project_for(state, v, id, ProjectAction::ReadCode).await?;
    let commit = commit_of(state, &p, r).await?;
    let blob = state.storage.blob(p.id, &commit, file, MAX_FILE).await.map_err(|_| ApiError::NotFound)?;
    if blob.truncated {
        return Err(ApiError::bad("the file is too large for this API"));
    }
    Ok((p, commit, blob))
}

pub async fn file(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, file)): Path<(String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Json<Value>> {
    let (_p, commit, blob) = read_file(&state, &v, &id, &file, q.git_ref.as_deref()).await?;
    let name = file.rsplit('/').next().unwrap_or(&file).to_string();
    Ok(Json(json!({
        "file_name": name, "file_path": file, "size": blob.size, "encoding": "base64",
        "content": STANDARD.encode(&blob.data), "ref": q.git_ref.unwrap_or(commit.clone()), "blob_id": blob.id,
        "commit_id": commit, "last_commit_id": commit, "execute_filemode": false,
    })))
}

pub async fn file_raw(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, file)): Path<(String, String)>,
    Query(q): Query<RefQuery>,
) -> ApiResult<Response> {
    let (_p, _, blob) = read_file(&state, &v, &id, &file, q.git_ref.as_deref()).await?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], Body::from(blob.data)).into_response())
}

// ------------------------------------------------------------------ branches and commits

async fn branch_json(
    state: &AppState,
    p: &Project,
    name: &str,
    sha: &str,
    rules: &[forgust_db::protected_branches::ProtectedBranch],
) -> ApiResult<Value> {
    let c = state.storage.commit(p.id, sha).await?;
    let protected = rules.iter().any(|r| r.matches(name));
    Ok(json!({
        "name": name, "merged": false, "protected": protected, "default": Some(name) == p.default_branch.as_deref(),
        "developers_can_push": !protected, "developers_can_merge": !protected, "can_push": true,
        "web_url": format!("{}/{}/-/tree/{name}", j::base(&state.cfg), p.full_path),
        "commit": c.as_ref().map(|c| j::commit(&state.cfg, p, c)),
    }))
}

#[derive(Deserialize)]
pub struct BranchesQuery {
    #[serde(default)]
    search: String,
}

pub async fn branches(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<BranchesQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let mut refs: Vec<(String, String)> = state
        .storage
        .refs(p.id, "refs/heads/")
        .await?
        .into_iter()
        .filter_map(|r| r.commit.map(|c| (r.name.trim_start_matches("refs/heads/").to_string(), c.id)))
        .filter(|(n, _)| q.search.is_empty() || n.contains(&q.search))
        .collect();
    refs.sort();
    let total = refs.len() as i64;
    let rules = forgust_db::protected_branches::list(&state.db, p.id).await?;
    let rows: Vec<_> = refs
        .iter()
        .skip(pg.offset() as usize)
        .take(pg.per_page() as usize)
        .map(|(name, sha)| branch_json(&state, &p, name, sha, &rules))
        .collect();
    let items: Vec<Value> = futures::stream::iter(rows).buffered(8).collect::<Vec<_>>().await.into_iter().collect::<ApiResult<_>>()?;
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

pub async fn branch(State(state): State<AppState>, Viewer(v): Viewer, Path((id, name)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    if !forgust_storage::validate_rev(&name) {
        return Err(ApiError::NotFound);
    }
    let sha = state.storage.resolve(p.id, &format!("refs/heads/{name}")).await?.ok_or(ApiError::NotFound)?;
    let rules = forgust_db::protected_branches::list(&state.db, p.id).await?;
    Ok(Json(branch_json(&state, &p, &name, &sha, &rules).await?))
}

pub async fn commit(State(state): State<AppState>, Viewer(v): Viewer, Path((id, sha)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let full = commit_of(&state, &p, Some(&sha)).await?;
    let c = state.storage.commit(p.id, &full).await?.ok_or(ApiError::NotFound)?;
    let mut out = j::commit(&state.cfg, &p, &c);
    let last = forgust_db::ci::list_pipelines(&state.db, p.id, None, None, Some(&full), 1, 0).await?;
    out["last_pipeline"] = last.first().map(|pl| j::pipeline(&state.cfg, &p, pl)).unwrap_or(Value::Null);
    out["status"] = last.first().map(|pl| json!(pl.status)).unwrap_or(Value::Null);
    Ok(Json(out))
}

/// The commit's statuses: jobs of its newest pipeline, then statuses set from outside.
pub async fn statuses(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path((id, sha)): Path<(String, String)>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let full = commit_of(&state, &p, Some(&sha)).await?;
    let mut items = Vec::new();
    if let Some(pl) = forgust_db::ci::list_pipelines(&state.db, p.id, None, None, Some(&full), 1, 0).await?.first() {
        for b in forgust_db::ci::builds_of_pipeline(&state.db, pl.id).await?.iter().filter(|b| !b.retried) {
            items.push(json!({
                "id": b.id, "sha": full, "ref": pl.git_ref, "status": b.status, "name": b.name, "allow_failure": b.allow_failure,
                "target_url": format!("{}/{}/-/jobs/{}", j::base(&state.cfg), p.full_path, b.id), "description": null,
                "created_at": j::ts(&b.created_at), "started_at": b.started_at.as_ref().map(j::ts), "finished_at": b.finished_at.as_ref().map(j::ts),
                "pipeline_id": pl.id,
            }));
        }
    }
    for s in forgust_db::ci::external_statuses(&state.db, p.id, &full).await? {
        items.push(status_json(&s));
    }
    let total = items.len() as i64;
    let items: Vec<Value> = items.into_iter().skip(pg.offset() as usize).take(pg.per_page() as usize).collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

fn status_json(s: &forgust_db::ci::ExternalStatus) -> Value {
    json!({
        "id": s.id, "sha": s.sha, "ref": s.git_ref, "status": s.status, "name": s.name, "allow_failure": false,
        "target_url": s.target_url, "description": s.description, "created_at": j::ts(&s.created_at),
        "started_at": null, "finished_at": if matches!(s.status.as_str(), "pending" | "running") { Value::Null } else { json!(j::ts(&s.updated_at)) },
    })
}

#[derive(Deserialize)]
pub struct SetStatus {
    state: String,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    name: Option<String>,
    context: Option<String>,
    target_url: Option<String>,
    description: Option<String>,
}

/// POST /projects/:id/statuses/:sha (Renovate's stability checks, outside CI).
pub async fn set_status(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, sha)): Path<(String, String)>,
    Json(req): Json<SetStatus>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Push).await?;
    let full = commit_of(&state, &p, Some(&sha)).await?;
    let status = match req.state.as_str() {
        "pending" | "running" | "success" | "failed" | "canceled" | "skipped" => req.state.as_str(),
        _ => return Err(ApiError::bad("state must be pending, running, success, failed, canceled or skipped")),
    };
    let name = req.name.or(req.context).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "default".into());
    if let Some(u) = &req.target_url
        && !(u.starts_with("https://") || u.starts_with("http://"))
    {
        return Err(ApiError::bad("target_url must be http(s)"));
    }
    let s = forgust_db::ci::set_external_status(
        &state.db,
        forgust_db::ci::NewExternalStatus {
            project_id: p.id,
            sha: &full,
            git_ref: req.git_ref.as_deref(),
            name: name.trim(),
            status,
            description: req.description.as_deref(),
            target_url: req.target_url.as_deref(),
            author_id: v.direct_user().map(|u| u.id),
        },
    )
    .await?;
    Ok(Json(status_json(&s)))
}

#[derive(Deserialize)]
pub struct V4NewBranch {
    branch: Option<String>,
    #[serde(rename = "ref")]
    from: Option<String>,
}

/// POST .../repository/branches (branch and ref in the query or the body).
pub async fn create_branch(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Query(q): Query<V4NewBranch>,
    body: Option<Json<V4NewBranch>>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let b = body.map(|Json(b)| b);
    let name = q.branch.or_else(|| b.as_ref().and_then(|b| b.branch.clone())).ok_or_else(|| ApiError::bad("branch is missing"))?;
    let from = q.from.or_else(|| b.as_ref().and_then(|b| b.from.clone())).ok_or_else(|| ApiError::bad("ref is missing"))?;
    let sha = crate::services::edits::create_branch(&state, &v, &p, &name, &from).await?;
    let rules = forgust_db::protected_branches::list(&state.db, p.id).await?;
    Ok((axum::http::StatusCode::CREATED, Json(branch_json(&state, &p, name.trim(), &sha, &rules).await?)).into_response())
}

pub async fn delete_branch(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<axum::http::StatusCode> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    crate::services::edits::delete_branch(&state, &v, &p, &name).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct V4Commit {
    branch: String,
    commit_message: String,
    start_branch: Option<String>,
    /// GitLab's `start_sha`: the commit a new branch starts from.
    start_sha: Option<String>,
    #[serde(default)]
    force: bool,
    actions: Vec<crate::services::edits::FileAction>,
}

/// POST .../repository/commits: GitLab's multi-file commit.
pub async fn create_commit(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<V4Commit>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let c = crate::services::edits::commit(
        &state,
        &v,
        &p,
        crate::services::edits::CommitRequest {
            branch: &req.branch,
            start_branch: req.start_sha.as_deref().or(req.start_branch.as_deref()),
            message: &req.commit_message,
            actions: &req.actions,
            expected_head: None,
            force: req.force,
        },
    )
    .await?;
    let info = state.storage.commit(p.id, &c.id).await?.ok_or(ApiError::NotFound)?;
    Ok((axum::http::StatusCode::CREATED, Json(j::commit(&state.cfg, &p, &info))).into_response())
}

#[derive(Deserialize, Default)]
pub struct TreeQuery {
    #[serde(default)]
    path: String,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    #[serde(default)]
    recursive: bool,
}

/// At most this many entries in a recursive tree (GitLab pages through them).
const MAX_TREE: usize = 50_000;

/// `GET /projects/:id/repository/tree`: a directory, or everything under it.
pub async fn tree(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<TreeQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    use forgust_storage::EntryKind;
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let commit = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    let root = q.path.trim_matches('/').to_string();
    let mut out = Vec::new();
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        let entries = match state.storage.tree(p.id, &commit, &dir).await {
            Ok(e) => e,
            // a path that is not there (or not a directory): GitLab's 404
            Err(_) if out.is_empty() => return Err(ApiError::NotFound),
            Err(e) => return Err(e.into()),
        };
        for e in entries {
            let (kind, mode) = match e.kind {
                EntryKind::Tree => ("tree", "040000"),
                EntryKind::Blob => ("blob", "100644"),
                EntryKind::Executable => ("blob", "100755"),
                EntryKind::Symlink => ("blob", "120000"),
                EntryKind::Submodule => ("commit", "160000"),
            };
            if q.recursive && e.kind == EntryKind::Tree {
                dirs.push(e.path.clone());
            }
            out.push((e.path.clone(), json!({ "id": e.id, "name": e.name, "type": kind, "path": e.path, "mode": mode })));
            if out.len() >= MAX_TREE {
                dirs.clear();
                break;
            }
        }
    }
    // GitLab's order: trees first, then by path
    out.sort_by(|a, b| (a.1["type"] != "tree", &a.0).cmp(&(b.1["type"] != "tree", &b.0)));
    let total = out.len() as i64;
    let page: Vec<Value> = out.into_iter().skip(pg.offset() as usize).take(pg.per_page() as usize).map(|(_, v)| v).collect();
    Ok(paged(&state, &uri, &pg, page, Some(total)))
}

#[derive(Deserialize, Default)]
pub struct CommitsQuery {
    ref_name: Option<String>,
    path: Option<String>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
}

/// `GET /projects/:id/repository/commits`: newest first, by ref and path.
pub async fn commits(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<CommitsQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    if state.storage.is_empty(p.id).await.unwrap_or(true) {
        return Ok(paged(&state, &uri, &pg, Vec::<Value>::new(), Some(0)));
    }
    let commit = commit_of(&state, &p, q.ref_name.as_deref()).await?;
    let path = q.path.as_deref().map(|s| s.trim_matches('/')).filter(|s| !s.is_empty());
    let in_range = |c: &forgust_storage::CommitInfo| {
        q.since.is_none_or(|s| c.committer.time >= s.timestamp()) && q.until.is_none_or(|u| c.committer.time <= u.timestamp())
    };
    let items: Vec<Value> = if q.since.is_some() || q.until.is_some() {
        // a date range: walk a bounded window and filter
        let all = state.storage.log(p.id, &commit, path, 0, 5_000).await?;
        all.iter()
            .filter(|c| in_range(c))
            .skip(pg.offset() as usize)
            .take(pg.probe() as usize)
            .map(|c| j::commit(&state.cfg, &p, c))
            .collect()
    } else {
        let list = state.storage.log(p.id, &commit, path, pg.offset() as usize, pg.probe() as usize).await?;
        list.iter().map(|c| j::commit(&state.cfg, &p, c)).collect()
    };
    Ok(paged(&state, &uri, &pg, items, None))
}
