//! Imports from GitLab and GitHub (M5, ADR-0018).

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use forgust_db::imports::{self as idb, Import, ImportProject, ImportUser};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::authz::{GroupAction, require_group};
use crate::auth::{Principal, SignedIn};
use crate::error::{ApiError, ApiResult};
use crate::import::model::{SourceError, SourceProject};
use crate::import::{self, ImportJob, Options};
use crate::state::AppState;

const SOURCES: [&str; 2] = ["gitlab", "github"];
const MAX_PROJECTS: usize = 300;

fn source_error(e: SourceError) -> ApiError {
    match e {
        SourceError::Refused(m) | SourceError::Other(m) => ApiError::bad(m),
        SourceError::NotFound(m) => ApiError::bad(format!("not found on the source: {m}")),
        SourceError::Transient(m) => ApiError::bad(format!("the source did not answer: {m}")),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ConnectBody {
    /// `gitlab` (or `github`).
    pub source: String,
    /// The forge's address, e.g. https://gitlab.example.com.
    pub url: String,
    /// Optional for public projects.
    #[serde(default)]
    pub token: String,
    /// Only projects under this group (GitLab) or owner (GitHub).
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub search: String,
}

#[derive(Serialize, ToSchema)]
pub struct Connected {
    /// The token's user on the source.
    pub username: String,
    pub name: String,
    pub projects: Vec<SourceProject>,
}

/// Check a token and list the projects it can import.
#[utoipa::path(post, path = "/api/v1/imports/connect", request_body = ConnectBody, responses((status = 200, body = Connected)), tag = "imports")]
pub async fn connect(State(state): State<AppState>, SignedIn(_, _p): SignedIn, Json(req): Json<ConnectBody>) -> ApiResult<Json<Connected>> {
    if !SOURCES.contains(&req.source.as_str()) {
        return Err(ApiError::bad("source must be gitlab or github"));
    }
    let src = import::connect(&state, &req.source, &req.url, &req.token).await.map_err(source_error)?;
    if req.token.trim().is_empty() {
        // anonymous: public projects of a group or owner, or added by path
        let projects = if req.scope.trim().is_empty() {
            Vec::new()
        } else {
            src.list_projects(&req.scope, &req.search).await.map_err(source_error)?
        };
        return Ok(Json(Connected { username: String::new(), name: String::new(), projects }));
    }
    let me = src.whoami().await.map_err(source_error)?;
    let projects = src.list_projects(&req.scope, &req.search).await.map_err(source_error)?;
    Ok(Json(Connected { username: me.username, name: me.name, projects }))
}

#[derive(Deserialize, ToSchema)]
pub struct LookupBody {
    pub source: String,
    pub url: String,
    #[serde(default)]
    pub token: String,
    /// `group/project`, `owner/repo`, or the project's URL.
    pub path: String,
}

/// One project by path or URL, for public projects the token is not a member of.
#[utoipa::path(post, path = "/api/v1/imports/lookup", request_body = LookupBody, responses((status = 200, body = SourceProject)), tag = "imports")]
pub async fn lookup(
    State(state): State<AppState>,
    SignedIn(_, _p): SignedIn,
    Json(req): Json<LookupBody>,
) -> ApiResult<Json<SourceProject>> {
    if !SOURCES.contains(&req.source.as_str()) {
        return Err(ApiError::bad("source must be gitlab or github"));
    }
    let path = import::project_path(&req.source, &req.url, &req.path)
        .ok_or_else(|| ApiError::bad("give a path like group/project, or the project's URL"))?;
    let src = import::connect(&state, &req.source, &req.url, &req.token).await.map_err(source_error)?;
    match src.project(&path).await {
        Ok(p) => Ok(Json(p)),
        Err(SourceError::NotFound(_)) => Err(ApiError::bad(format!("`{path}` was not found, or is private and the token cannot read it"))),
        Err(e) => Err(source_error(e)),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ImportProjectBody {
    pub source_id: String,
    pub source_path: String,
    /// Path under the target group; `sub/project` creates the subgroup.
    pub target_path: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateImportBody {
    pub source: String,
    pub url: String,
    /// Optional for public projects.
    #[serde(default)]
    pub token: String,
    pub target_namespace_id: i64,
    pub projects: Vec<ImportProjectBody>,
    #[serde(default)]
    pub options: Option<Options>,
}

#[derive(Serialize, ToSchema)]
pub struct ImportDetail {
    #[serde(flatten)]
    pub import: Import,
    pub projects: Vec<ImportProject>,
    /// Source users seen so far and who they are here.
    pub users: Vec<ImportUser>,
}

async fn detail(state: &AppState, import: Import) -> ApiResult<ImportDetail> {
    let projects = idb::projects(&state.db, import.id).await?;
    let users = idb::users(&state.db, import.id).await?;
    Ok(ImportDetail { import, projects, users })
}

fn check_target_path(p: &str) -> ApiResult<()> {
    let segs: Vec<&str> = p.split('/').collect();
    if segs.is_empty() || segs.len() > 10 {
        return Err(ApiError::bad(format!("bad target path `{p}`")));
    }
    for s in segs {
        crate::services::paths::validate_segment(s)?;
    }
    Ok(())
}

/// Start an import: it plans every project first (a dry run), then waits for `start`.
#[utoipa::path(post, path = "/api/v1/imports", request_body = CreateImportBody, responses((status = 200, body = ImportDetail)), tag = "imports")]
pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Json(req): Json<CreateImportBody>,
) -> ApiResult<Json<ImportDetail>> {
    let user = p0.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    if !SOURCES.contains(&req.source.as_str()) {
        return Err(ApiError::bad("source must be gitlab or github"));
    }
    if req.projects.is_empty() || req.projects.len() > MAX_PROJECTS {
        return Err(ApiError::bad(format!("choose between 1 and {MAX_PROJECTS} projects")));
    }
    let ns = forgust_db::namespaces::find(&state.db, req.target_namespace_id).await?.ok_or(ApiError::NotFound)?;
    require_group(&state, &p0, GroupAction::CreateProject, &ns).await?;
    let mut seen = HashSet::new();
    for p in &req.projects {
        check_target_path(&p.target_path)?;
        if !seen.insert(p.target_path.to_lowercase()) {
            return Err(ApiError::bad(format!("two projects would land on `{}`", p.target_path)));
        }
    }
    // check the address and token now rather than in the background
    let src = import::connect(&state, &req.source, &req.url, &req.token).await.map_err(source_error)?;
    if !req.token.trim().is_empty() {
        src.whoami().await.map_err(source_error)?;
    }
    let base_url = url::Url::parse(req.url.trim()).map_err(|_| ApiError::bad("invalid URL"))?;
    let sealed = state.secrets.encrypt(req.token.trim().as_bytes()).map_err(ApiError::Internal)?;
    let options = serde_json::to_value(req.options.unwrap_or_default()).unwrap_or_default();

    let mut tx = state.db.begin().await?;
    let id = idb::insert(
        &mut *tx,
        idb::NewImport {
            user_id: user.id,
            source: &req.source,
            base_url: base_url.as_str().trim_end_matches('/'),
            token_enc: &sealed,
            target_namespace_id: ns.id,
            options: &options,
        },
    )
    .await?;
    for p in &req.projects {
        let row = idb::add_project(&mut *tx, id, &p.source_id, &p.source_path, p.target_path.trim_matches('/')).await?;
        state.jobs.enqueue(&mut tx, ImportJob { import_project_id: row, plan: true }.request(id)).await.map_err(ApiError::Internal)?;
    }
    tx.commit().await?;
    crate::services::audit(
        &state,
        &p0,
        "import_started",
        Some(("namespace", ns.id)),
        serde_json::json!({ "import": id, "source": req.source, "url": base_url.as_str(), "projects": req.projects.len() }),
        None,
    )
    .await;
    let import = idb::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(detail(&state, import).await?))
}

#[utoipa::path(get, path = "/api/v1/imports", responses((status = 200, body = [Import])), tag = "imports")]
pub async fn list(State(state): State<AppState>, SignedIn(_, p0): SignedIn) -> ApiResult<Json<Vec<Import>>> {
    let user = p0.direct_user().ok_or(ApiError::Unauthorized)?;
    Ok(Json(idb::list_for_user(&state.db, user.id, 50).await?))
}

async fn mine(state: &AppState, p0: &Principal, id: i64) -> ApiResult<Import> {
    let user = p0.direct_user().ok_or(ApiError::Unauthorized)?;
    let import = idb::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    if import.user_id != user.id && !user.is_admin {
        return Err(ApiError::NotFound);
    }
    Ok(import)
}

#[utoipa::path(get, path = "/api/v1/imports/{id}", responses((status = 200, body = ImportDetail)), tag = "imports")]
pub async fn show(State(state): State<AppState>, SignedIn(_, p0): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<ImportDetail>> {
    let import = mine(&state, &p0, id).await?;
    Ok(Json(detail(&state, import).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct StartBody {
    /// Import rows to skip (by id); the others are imported.
    #[serde(default)]
    pub skip: Vec<i64>,
    /// Target paths changed after the dry run (row id → path).
    #[serde(default)]
    pub paths: std::collections::HashMap<String, String>,
}

/// Refuse target paths that are taken here (projects not created yet).
async fn check_free(state: &AppState, import: &Import, rows: &[(ImportProject, String)]) -> ApiResult<()> {
    for (row, path) in rows {
        let full = format!("{}/{}", import.target_path, path.trim_matches('/'));
        if row.project_id.is_none() && forgust_db::routes::path_taken(&state.db, &full).await? {
            return Err(ApiError::conflict(format!("`{full}` is already taken: change its path or skip it")));
        }
    }
    Ok(())
}

/// After the dry run: import the projects for real.
#[utoipa::path(post, path = "/api/v1/imports/{id}/start", request_body = StartBody, responses((status = 200, body = ImportDetail)), tag = "imports")]
pub async fn start(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path(id): Path<i64>,
    Json(req): Json<StartBody>,
) -> ApiResult<Json<ImportDetail>> {
    let import = mine(&state, &p0, id).await?;
    if import.status != "planned" {
        return Err(ApiError::conflict(format!("the import is {}, not waiting to start", import.status)));
    }
    let rows = idb::projects(&state.db, id).await?;
    let mut chosen = Vec::new();
    for row in rows.iter().filter(|r| !req.skip.contains(&r.id)) {
        let path = req.paths.get(&row.id.to_string()).cloned().unwrap_or_else(|| row.target_path.clone());
        check_target_path(&path)?;
        chosen.push((row.clone(), path));
    }
    check_free(&state, &import, &chosen).await?;
    let mut tx = state.db.begin().await?;
    let mut any = false;
    for row in rows {
        if req.skip.contains(&row.id) {
            idb::set_project_status(&mut *tx, row.id, "skipped", None).await?;
            continue;
        }
        if let Some(path) = req.paths.get(&row.id.to_string()) {
            idb::set_target_path(&mut *tx, row.id, path.trim_matches('/')).await?;
        }
        idb::set_project_status(&mut *tx, row.id, "pending", None).await?;
        state.jobs.enqueue(&mut tx, ImportJob { import_project_id: row.id, plan: false }.request(id)).await.map_err(ApiError::Internal)?;
        any = true;
    }
    idb::set_status(&mut *tx, id, if any { "running" } else { "canceled" }).await?;
    tx.commit().await?;
    let import = idb::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(detail(&state, import).await?))
}

/// Stop: projects not started are canceled and the token is erased.
#[utoipa::path(post, path = "/api/v1/imports/{id}/cancel", responses((status = 200, body = ImportDetail)), tag = "imports")]
pub async fn cancel(State(state): State<AppState>, SignedIn(_, p0): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<ImportDetail>> {
    let import = mine(&state, &p0, id).await?;
    if matches!(import.status.as_str(), "finished" | "failed" | "canceled") {
        return Err(ApiError::conflict("the import has already ended"));
    }
    let mut tx = state.db.begin().await?;
    for row in idb::projects(&mut *tx, id).await? {
        if matches!(row.status.as_str(), "pending" | "planning" | "planned" | "running") {
            idb::set_project_status(&mut *tx, row.id, "canceled", None).await?;
        }
    }
    idb::set_status(&mut *tx, id, "canceled").await?;
    tx.commit().await?;
    let import = idb::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(detail(&state, import).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct RetryBody {
    /// Needed again once the import has ended (its token was erased).
    pub token: Option<String>,
    /// New target paths for failed projects not created yet (row id → path).
    #[serde(default)]
    pub paths: std::collections::HashMap<String, String>,
}

/// Run the failed projects again; what they imported already is skipped.
#[utoipa::path(post, path = "/api/v1/imports/{id}/retry", request_body = RetryBody, responses((status = 200, body = ImportDetail)), tag = "imports")]
pub async fn retry(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path(id): Path<i64>,
    Json(req): Json<RetryBody>,
) -> ApiResult<Json<ImportDetail>> {
    let import = mine(&state, &p0, id).await?;
    // rights are checked again: they may have changed since the import began
    let ns = forgust_db::namespaces::find(&state.db, import.target_namespace_id).await?.ok_or(ApiError::NotFound)?;
    require_group(&state, &p0, GroupAction::CreateProject, &ns).await?;
    let rows: Vec<ImportProject> = idb::projects(&state.db, id).await?.into_iter().filter(|r| r.status == "failed").collect();
    if rows.is_empty() {
        return Err(ApiError::bad("no project failed"));
    }
    let mut chosen = Vec::new();
    for row in &rows {
        let path = match req.paths.get(&row.id.to_string()) {
            Some(p) if row.project_id.is_none() => p.clone(),
            _ => row.target_path.clone(),
        };
        check_target_path(&path)?;
        chosen.push((row.clone(), path));
    }
    check_free(&state, &import, &chosen).await?;
    // without a new token: the stored one, or none (public projects)
    let token = match (req.token.as_deref().map(str::trim).filter(|t| !t.is_empty()), import.has_token) {
        (Some(t), _) => Some(t.to_string()),
        (None, true) => None,
        (None, false) => Some(String::new()),
    };
    if let Some(t) = token.as_ref().filter(|t| !t.is_empty()) {
        let src = import::connect(&state, &import.source, &import.base_url, t).await.map_err(source_error)?;
        src.whoami().await.map_err(source_error)?;
    }
    let mut tx = state.db.begin().await?;
    if let Some(t) = &token {
        let sealed = state.secrets.encrypt(t.as_bytes()).map_err(ApiError::Internal)?;
        idb::set_token(&mut *tx, id, &sealed).await?;
    }
    for (row, path) in &chosen {
        if *path != row.target_path {
            idb::set_target_path(&mut *tx, row.id, path.trim_matches('/')).await?;
        }
        idb::set_project_status(&mut *tx, row.id, "pending", None).await?;
        state.jobs.enqueue(&mut tx, ImportJob { import_project_id: row.id, plan: false }.request(id)).await.map_err(ApiError::Internal)?;
    }
    idb::set_status(&mut *tx, id, "running").await?;
    tx.commit().await?;
    let import = idb::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(detail(&state, import).await?))
}
