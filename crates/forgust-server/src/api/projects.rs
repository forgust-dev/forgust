use axum::Json;
use axum::extract::{Path, Query, State};
use forgust_db::audit::AuditEvent;
use forgust_db::ssh_keys::DeployKey;
use forgust_db::tokens::DeployToken;
use forgust_storage::RefFormat;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::dto::{AccessTokenDto, Breadcrumb, MemberDto, NewToken, ProjectDetail, ProjectDto, ProtectedBranchDto, ShareDto};
use super::groups::{
    AddMember, CreateAccessToken, CreateDeployToken, ShareRequest, create_bot_token, create_deploy, list_bot_tokens, parse_level,
    resolve_member, revoke_bot,
};
use super::{Paging, load_namespace, load_project, project_for};
use crate::auth::authz::{ProjectAction, project_role};
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::{self, members, projects};
use crate::state::AppState;

#[utoipa::path(get, path = "/api/v1/projects", responses((status = 200, body = [ProjectDto])), tag = "projects")]
pub async fn explore(State(state): State<AppState>, Viewer(p): Viewer, Query(pg): Query<Paging>) -> ApiResult<Json<Vec<ProjectDto>>> {
    let signed_in = p.sees_internal();
    let list = forgust_db::projects::explore(&state.db, signed_in, &pg.search, pg.limit(), pg.offset()).await?;
    Ok(Json(list.iter().map(|x| ProjectDto::from(x, &state.cfg)).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateProject {
    pub namespace_id: Option<i64>,
    pub name: String,
    pub path: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "private")]
    pub visibility: String,
    pub default_branch: Option<String>,
    /// `files` (default) or `reftable` (ADR-0011).
    pub ref_format: Option<String>,
}

fn private() -> String {
    "private".into()
}

#[utoipa::path(post, path = "/api/v1/projects", request_body = CreateProject, responses((status = 200, body = ProjectDto)), tag = "projects")]
pub async fn create(
    State(state): State<AppState>,
    SignedIn(u, p): SignedIn,
    Json(req): Json<CreateProject>,
) -> ApiResult<Json<ProjectDto>> {
    let ns_id = req.namespace_id.or(u.namespace_id).ok_or_else(|| ApiError::bad("namespace_id is required"))?;
    let ns = load_namespace(&state, &ns_id.to_string()).await?;
    let path = req.path.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| services::paths::slug(&req.name));
    let ref_format = match req.ref_format.as_deref() {
        None | Some("") => RefFormat::Files,
        Some(f) => RefFormat::parse(f).ok_or_else(|| ApiError::bad("ref_format must be files or reftable"))?,
    };
    let proj = projects::create(
        &state,
        &p,
        &ns,
        projects::CreateProject {
            name: &req.name,
            path: &path,
            description: &req.description,
            visibility: forgust_db::visibility::parse(&req.visibility).ok_or_else(|| ApiError::bad("bad visibility"))?,
            default_branch: req.default_branch.as_deref(),
            ref_format,
        },
    )
    .await?;
    Ok(Json(ProjectDto::from(&proj, &state.cfg)))
}

/// Archive the project: read-only for everyone until its Owner unarchives it.
pub async fn archive(State(state): State<AppState>, SignedIn(_, p): SignedIn, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    set_archived(&state, &p, &id, true).await
}

pub async fn unarchive(State(state): State<AppState>, SignedIn(_, p): SignedIn, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    set_archived(&state, &p, &id, false).await
}

async fn set_archived(state: &AppState, p: &crate::auth::Principal, id: &str, archived: bool) -> ApiResult<Json<Value>> {
    let proj = project_for(state, p, id, ProjectAction::Own).await?;
    let at = forgust_db::projects::set_archived(&state.db, proj.id, archived).await?;
    crate::services::audit(
        state,
        p,
        if archived { "project_archived" } else { "project_unarchived" },
        Some(("project", proj.id)),
        json!({}),
        None,
    )
    .await;
    state.events.publish(crate::events::Event::Push { project_id: proj.id, refs: vec![] });
    Ok(Json(json!({ "archived_at": at })))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}", responses((status = 200, body = ProjectDetail)), tag = "projects")]
pub async fn show(State(state): State<AppState>, Viewer(p): Viewer, Path(id): Path<String>) -> ApiResult<Json<ProjectDetail>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Read).await?;
    let role = project_role(&state, &p, &proj).await?;
    let ancestors = forgust_db::namespaces::find_many(&state.db, &proj.traversal_ids).await?;
    let empty = state.storage.is_empty(proj.id).await.unwrap_or(true);
    Ok(Json(ProjectDetail {
        access_level: forgust_db::access::name(if role.bypass { 0 } else { role.level }).into(),
        can_read_code: role.can_read_code,
        can_push: role.can_push,
        // archived: settings are read-only too (the Owner keeps unarchive, transfer, delete)
        can_admin: p.direct_user().is_some() && role.level >= forgust_db::access::MAINTAINER && !proj.archived(),
        can_own: p.direct_user().is_some() && role.level >= forgust_db::access::OWNER,
        empty_repo: empty,
        ancestors: ancestors.iter().map(|a| Breadcrumb { id: a.id, name: a.name.clone(), full_path: a.full_path.clone() }).collect(),
        redirects: if role.level >= forgust_db::access::MAINTAINER {
            forgust_db::routes::redirects_for(&state.db, "project", proj.id).await?
        } else {
            vec![]
        },
        storage_warning: if role.can_push { forgust_db::quotas::warning(&state.db, proj.id).await? } else { None },
        project: ProjectDto::from(&proj, &state.cfg),
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateProject {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub visibility: String,
    pub default_branch: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<UpdateProject>,
) -> ApiResult<Json<ProjectDto>> {
    let proj = load_project(&state, &id).await?;
    let vis = forgust_db::visibility::parse(&req.visibility).ok_or_else(|| ApiError::bad("bad visibility"))?;
    projects::update(&state, &p, &proj, &req.name, &req.description, vis, req.default_branch.as_deref()).await?;
    let proj = load_project(&state, &proj.id.to_string()).await?;
    Ok(Json(ProjectDto::from(&proj, &state.cfg)))
}

#[derive(Deserialize, ToSchema)]
pub struct NewPath {
    pub path: String,
}

pub async fn rename(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<NewPath>,
) -> ApiResult<Json<ProjectDto>> {
    let proj = load_project(&state, &id).await?;
    let proj = projects::rename_path(&state, &p, &proj, &req.path).await?;
    Ok(Json(ProjectDto::from(&proj, &state.cfg)))
}

#[derive(Deserialize, ToSchema)]
pub struct TransferProject {
    pub namespace_id: i64,
}

pub async fn transfer(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<TransferProject>,
) -> ApiResult<Json<ProjectDto>> {
    let proj = load_project(&state, &id).await?;
    let target = load_namespace(&state, &req.namespace_id.to_string()).await?;
    let proj = projects::transfer(&state, &p, &proj, &target).await?;
    Ok(Json(ProjectDto::from(&proj, &state.cfg)))
}

pub async fn delete(State(state): State<AppState>, SignedIn(_, p): SignedIn, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let proj = load_project(&state, &id).await?;
    projects::delete(&state, &p, &proj).await?;
    Ok(Json(json!({ "ok": true })))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/members", responses((status = 200, body = [MemberDto])), tag = "projects")]
pub async fn members(State(state): State<AppState>, SignedIn(_, p): SignedIn, Path(id): Path<String>) -> ApiResult<Json<Vec<MemberDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Read).await?;
    Ok(Json(forgust_db::members::list(&state.db, "project", proj.id).await?.into_iter().map(Into::into).collect()))
}

pub async fn add_member(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<AddMember>,
) -> ApiResult<Json<Value>> {
    let proj = load_project(&state, &id).await?;
    let uid = resolve_member(&state, &req).await?;
    members::add(&state, &p, members::Source::Project(&proj), uid, parse_level(&req.access_level)?, req.expires_at).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove_member(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, user_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let proj = load_project(&state, &id).await?;
    members::remove(&state, &p, members::Source::Project(&proj), user_id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn shares(State(state): State<AppState>, SignedIn(_, p): SignedIn, Path(id): Path<String>) -> ApiResult<Json<Vec<ShareDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Read).await?;
    Ok(Json(super::groups::visible_shares(&state, &p, forgust_db::links::project_shares(&state.db, proj.id).await?).await?))
}

pub async fn share(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<ShareRequest>,
) -> ApiResult<Json<Value>> {
    let proj = load_project(&state, &id).await?;
    let with = forgust_db::namespaces::find(&state.db, req.group_id).await?.ok_or(ApiError::NotFound)?;
    members::share(&state, &p, members::Source::Project(&proj), &with, parse_level(&req.max_access_level)?, req.expires_at).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn unshare(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, group_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let proj = load_project(&state, &id).await?;
    let with = forgust_db::namespaces::find(&state.db, group_id).await?.ok_or(ApiError::NotFound)?;
    members::unshare(&state, &p, members::Source::Project(&proj), &with).await?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ protected branches

#[utoipa::path(get, path = "/api/v1/projects/{id}/protected_branches", responses((status = 200, body = [ProtectedBranchDto])), tag = "projects")]
pub async fn protected_branches(
    State(state): State<AppState>,
    Viewer(p): Viewer,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<ProtectedBranchDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::ReadCode).await?;
    Ok(Json(forgust_db::protected_branches::list(&state.db, proj.id).await?.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct ProtectBranch {
    /// Branch name or glob, e.g. `release/*`.
    pub name: String,
    /// `no_one`, `developer` or `maintainer`.
    #[serde(default = "maintainer")]
    pub push_access_level: String,
    #[serde(default = "maintainer")]
    pub merge_access_level: String,
    #[serde(default)]
    pub allow_force_push: bool,
    #[serde(default)]
    pub allow_deploy_keys: bool,
    /// Merge requests into it need an approval from the code owners of the
    /// files they change (`CODEOWNERS`).
    #[serde(default)]
    pub code_owner_approval_required: bool,
}

fn maintainer() -> String {
    "maintainer".into()
}

fn push_level(s: &str) -> ApiResult<i16> {
    match s {
        "no_one" => Ok(0),
        "developer" => Ok(30),
        "maintainer" => Ok(40),
        _ => Err(ApiError::bad("level must be no_one, developer or maintainer")),
    }
}

pub async fn protect_branch(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<ProtectBranch>,
) -> ApiResult<Json<Vec<ProtectedBranchDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    if req.name.is_empty()
        || req.name.len() > 255
        || req.name.starts_with('-')
        || req.name.contains("..")
        || globset::Glob::new(&req.name).is_err()
    {
        return Err(ApiError::bad("invalid branch name or pattern"));
    }
    let pb = forgust_db::protected_branches::upsert(
        &state.db,
        proj.id,
        &req.name,
        push_level(&req.push_access_level)?,
        push_level(&req.merge_access_level)?,
        req.allow_force_push,
        req.allow_deploy_keys,
    )
    .await?;
    forgust_db::protected_branches::set_code_owner_approval(&state.db, pb, req.code_owner_approval_required).await?;
    services::audit(
        &state,
        &p,
        "branch_protected",
        Some(("project", proj.id)),
        json!({ "name": req.name, "push": req.push_access_level, "force_push": req.allow_force_push }),
        None,
    )
    .await;
    Ok(Json(forgust_db::protected_branches::list(&state.db, proj.id).await?.into_iter().map(Into::into).collect()))
}

pub async fn unprotect_branch(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, pb_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    if !forgust_db::protected_branches::delete(&state.db, proj.id, pb_id).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(&state, &p, "branch_unprotected", Some(("project", proj.id)), json!({ "id": pb_id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ deploy keys and tokens

pub async fn deploy_keys(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<DeployKey>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(forgust_db::ssh_keys::deploy_keys_for_project(&state.db, proj.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct AddDeployKey {
    pub title: String,
    pub key: String,
    #[serde(default)]
    pub can_push: bool,
}

pub async fn add_deploy_key(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<AddDeployKey>,
) -> ApiResult<Json<Vec<DeployKey>>> {
    p.may_mint_credentials()?;
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    let (key, fp) = super::user::parse_public_key(&req.key)?;
    let key_id = match forgust_db::ssh_keys::find_by_fingerprint(&state.db, &fp).await? {
        // an existing deploy key can be attached to more projects; a user key cannot
        Some(k) if k.kind == "deploy" => k.id,
        Some(_) => return Err(ApiError::conflict("this key is already used as a personal key")),
        None => {
            forgust_db::ssh_keys::insert(&state.db, "deploy", None, &req.title, &key, &fp, None, forgust_db::ssh_keys::KeyUsage::Auth)
                .await?
        }
    };
    forgust_db::ssh_keys::link_deploy_key(&state.db, key_id, proj.id, req.can_push).await?;
    services::audit(
        &state,
        &p,
        "deploy_key_added",
        Some(("project", proj.id)),
        json!({ "fingerprint": fp, "can_push": req.can_push }),
        None,
    )
    .await;
    Ok(Json(forgust_db::ssh_keys::deploy_keys_for_project(&state.db, proj.id).await?))
}

pub async fn remove_deploy_key(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, key_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    if !forgust_db::ssh_keys::unlink_deploy_key(&state.db, key_id, proj.id).await? {
        return Err(ApiError::NotFound);
    }
    forgust_db::ssh_keys::delete_orphan_deploy_key(&state.db, key_id).await?;
    services::audit(&state, &p, "deploy_key_removed", Some(("project", proj.id)), json!({ "key_id": key_id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_deploy_tokens(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<DeployToken>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(forgust_db::tokens::list_deploy_tokens(&state.db, Some(proj.id), None).await?))
}

pub async fn create_deploy_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<CreateDeployToken>,
) -> ApiResult<Json<NewToken>> {
    p.may_mint_credentials()?;
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(create_deploy(&state, &p, Some(proj.id), None, req).await?))
}

pub async fn revoke_deploy_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, token_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    if !forgust_db::tokens::revoke_deploy_token(&state.db, token_id, Some(proj.id), None).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(&state, &p, "deploy_token_revoked", Some(("project", proj.id)), json!({ "token_id": token_id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_access_tokens(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<AccessTokenDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(list_bot_tokens(&state, "project", proj.id).await?))
}

pub async fn create_access_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<CreateAccessToken>,
) -> ApiResult<Json<NewToken>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(create_bot_token(&state, &p, members::Source::Project(&proj), req).await?))
}

pub async fn revoke_access_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, bot_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    revoke_bot(&state, &p, members::Source::Project(&proj), bot_id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    before: Option<i64>,
}

pub async fn audit_log(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Vec<AuditEvent>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    Ok(Json(forgust_db::audit::list(&state.db, Some("project"), Some(proj.id), 100, q.before).await?))
}

#[derive(Deserialize)]
pub struct UsersQuery {
    #[serde(default)]
    search: String,
}

/// People who can be assigned or asked to review (Reporter+), for pickers.
#[utoipa::path(get, path = "/api/v1/projects/{id}/users", responses((status = 200, body = [forgust_db::people::UserBrief])), tag = "projects")]
pub async fn users(
    State(state): State<AppState>,
    // who works on a project is for signed-in people, as `/users` and members
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Query(q): Query<UsersQuery>,
) -> ApiResult<Json<Vec<forgust_db::people::UserBrief>>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let search = q.search.trim().trim_start_matches('@');
    Ok(Json(forgust_db::people::project_users(&state.db, p.id, forgust_db::access::REPORTER, search, 20).await?))
}

// ------------------------------------------------------------------ protected tags

#[derive(serde::Serialize, ToSchema)]
pub struct ProtectedTagDto {
    pub id: i64,
    pub name: String,
    /// `no_one`, `developer` or `maintainer`.
    pub create_access_level: String,
}

impl From<forgust_db::protected_branches::ProtectedTag> for ProtectedTagDto {
    fn from(t: forgust_db::protected_branches::ProtectedTag) -> Self {
        ProtectedTagDto { id: t.id, name: t.name, create_access_level: crate::api::dto::push_level_name(t.create_access_level).into() }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ProtectTag {
    /// Tag name or glob, e.g. `v*`.
    pub name: String,
    /// `no_one`, `developer` or `maintainer`.
    #[serde(default = "maintainer")]
    pub create_access_level: String,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/protected_tags", responses((status = 200, body = [ProtectedTagDto])), tag = "projects")]
pub async fn protected_tags(
    State(state): State<AppState>,
    Viewer(p): Viewer,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<ProtectedTagDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::ReadCode).await?;
    Ok(Json(forgust_db::protected_branches::tags(&state.db, proj.id).await?.into_iter().map(Into::into).collect()))
}

/// Protect tags matching a name or glob (Maintainers).
#[utoipa::path(post, path = "/api/v1/projects/{id}/protected_tags", request_body = ProtectTag, responses((status = 200, body = [ProtectedTagDto])), tag = "projects")]
pub async fn protect_tag(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<ProtectTag>,
) -> ApiResult<Json<Vec<ProtectedTagDto>>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    services::protected_refs::protect_tag(&state, &p, &proj, &req.name, push_level(&req.create_access_level)?).await?;
    Ok(Json(forgust_db::protected_branches::tags(&state.db, proj.id).await?.into_iter().map(Into::into).collect()))
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/protected_tags/{pt_id}", responses((status = 200)), tag = "projects")]
pub async fn unprotect_tag(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, pt_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    services::protected_refs::unprotect_tag(&state, &p, &proj, pt_id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// What the project keeps in the blob store (Maintainers).
#[utoipa::path(get, path = "/api/v1/projects/{id}/storage", responses((status = 200, body = super::dto::ProjectStorageUse)), tag = "projects")]
pub async fn storage(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<super::dto::ProjectStorageUse>> {
    let proj = project_for(&state, &p, &id, ProjectAction::Admin).await?;
    let kinds = forgust_db::releases::storage_use(&state.db, proj.id).await?;
    let total = forgust_db::quotas::project_used(&state.db, proj.id).await?;
    let limits = forgust_db::quotas::limits(&state.db, proj.id).await?.ok_or(ApiError::NotFound)?;
    let namespace = match limits.namespace_quota {
        Some(q) => Some(super::dto::QuotaUse {
            name: limits.root_path.clone(),
            used: forgust_db::quotas::namespace_used(&state.db, limits.root_id).await?,
            quota: q,
        }),
        None => None,
    };
    Ok(Json(super::dto::ProjectStorageUse {
        repository_bytes: total - kinds.uploads_bytes - kinds.artifacts_bytes - kinds.lfs_bytes - kinds.registry_bytes,
        total_bytes: total,
        quota: limits.project_quota.map(|q| super::dto::QuotaUse { name: proj.full_path.clone(), used: total, quota: q }),
        namespace,
        kinds,
    }))
}

/// The project's Pages site.
#[derive(Serialize, ToSchema)]
pub struct PagesInfo {
    /// The instance serves sites (`pages.domain` is set).
    pub enabled: bool,
    /// Where the site is (or would be).
    pub url: Option<String>,
    /// Only public projects' sites are served.
    pub public: bool,
    pub deployment: Option<forgust_db::pages::PagesDeployment>,
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/pages", responses((status = 200, body = PagesInfo)), tag = "projects")]
pub async fn pages(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<PagesInfo>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    Ok(Json(PagesInfo {
        enabled: state.cfg.pages.enabled(),
        url: state.cfg.pages.url(&p.full_path),
        public: p.visibility == forgust_db::visibility::PUBLIC,
        deployment: forgust_db::pages::deployment(&state.db, p.id).await?,
    }))
}

/// Take the site down (Maintainers); the next Pages job puts it back.
pub async fn remove_pages(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    let mut conn = state.db.acquire().await?;
    if !forgust_db::pages::remove(&mut conn, p.id).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(&state, &principal, "pages_removed", Some(("project", p.id)), json!({}), None).await;
    Ok(Json(json!({ "removed": true })))
}
