use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use forgust_db::access;
use forgust_db::tokens::DeployToken;
use serde::Deserialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::dto::{AccessTokenDto, Breadcrumb, GroupDetail, GroupDto, MemberDto, NewToken, OrgPolicyDto, ProjectDto, ShareDto};
use super::{group_for, load_namespace};
use crate::auth::authz::{GroupAction, can_group, group_level, require_group};
use crate::auth::{Principal, SignedIn, Viewer};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::services::{self, groups, members};
use crate::state::AppState;

fn parse_visibility(v: &str) -> ApiResult<i16> {
    forgust_db::visibility::parse(v).ok_or_else(|| ApiError::bad("visibility must be private, internal or public"))
}

pub fn parse_level(v: &str) -> ApiResult<i16> {
    access::parse(v).ok_or_else(|| ApiError::bad("access level must be guest, reporter, developer, maintainer or owner"))
}

#[utoipa::path(get, path = "/api/v1/groups", responses((status = 200, body = [GroupDto])), tag = "groups")]
pub async fn list_top(State(state): State<AppState>, Viewer(p): Viewer) -> ApiResult<Json<Vec<GroupDto>>> {
    let uid = p.direct_user().map(|u| u.id);
    let signed_in = p.sees_internal();
    let list = forgust_db::namespaces::top_level_visible(&state.db, uid, p.is_admin(), signed_in).await?;
    Ok(Json(list.iter().map(|n| GroupDto::from(n, &state.cfg)).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateGroup {
    pub name: String,
    pub path: Option<String>,
    pub parent_id: Option<i64>,
    #[serde(default = "private")]
    pub visibility: String,
    #[serde(default)]
    pub description: String,
}

fn private() -> String {
    "private".into()
}

#[utoipa::path(post, path = "/api/v1/groups", request_body = CreateGroup, responses((status = 200, body = GroupDto)), tag = "groups")]
pub async fn create(State(state): State<AppState>, SignedIn(_, p): SignedIn, Json(req): Json<CreateGroup>) -> ApiResult<Json<GroupDto>> {
    let path = req.path.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| services::paths::slug(&req.name));
    let ns = groups::create(
        &state,
        &p,
        groups::CreateGroup {
            name: &req.name,
            path: &path,
            parent_id: req.parent_id,
            visibility: parse_visibility(&req.visibility)?,
            description: &req.description,
        },
    )
    .await?;
    Ok(Json(GroupDto::from(&ns, &state.cfg)))
}

#[utoipa::path(get, path = "/api/v1/groups/{id}", responses((status = 200, body = GroupDetail)), tag = "groups")]
pub async fn show(State(state): State<AppState>, Viewer(p): Viewer, Path(id): Path<String>) -> ApiResult<Json<GroupDetail>> {
    let ns = group_for(&state, &p, &id, GroupAction::Read).await?;
    let (level, _) = group_level(&state, &p, &ns).await?;
    let ancestors = forgust_db::namespaces::find_many(&state.db, &ns.traversal_ids[..ns.traversal_ids.len().saturating_sub(1)]).await?;
    let policy = (ns.is_group() && ns.parent_id.is_none()).then(|| OrgPolicyDto {
        max_visibility: ns.max_visibility.map(|v| forgust_db::visibility::name(v).into()),
        require_two_factor: ns.require_two_factor,
        project_creation_level: access::name(ns.project_creation_level).into(),
        subgroup_creation_level: access::name(ns.subgroup_creation_level).into(),
    });
    Ok(Json(GroupDetail {
        can_create_project: can_group(&state, &p, GroupAction::CreateProject, &ns).await?,
        can_create_subgroup: can_group(&state, &p, GroupAction::CreateSubgroup, &ns).await?,
        can_admin: can_group(&state, &p, GroupAction::Admin, &ns).await?,
        access_level: access::name(level).into(),
        ancestors: ancestors.iter().map(|a| Breadcrumb { id: a.id, name: a.name.clone(), full_path: a.full_path.clone() }).collect(),
        policy,
        group: GroupDto::from(&ns, &state.cfg),
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateGroup {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub visibility: String,
}

pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<UpdateGroup>,
) -> ApiResult<Json<GroupDto>> {
    let ns = load_namespace(&state, &id).await?;
    groups::update(&state, &p, &ns, &req.name, &req.description, parse_visibility(&req.visibility)?).await?;
    let ns = load_namespace(&state, &ns.id.to_string()).await?;
    Ok(Json(GroupDto::from(&ns, &state.cfg)))
}

#[derive(Deserialize, ToSchema)]
pub struct PolicyRequest {
    pub max_visibility: Option<String>,
    pub require_two_factor: bool,
    pub project_creation_level: String,
    pub subgroup_creation_level: String,
}

pub async fn set_policy(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<PolicyRequest>,
) -> ApiResult<Json<Value>> {
    let ns = load_namespace(&state, &id).await?;
    let max = match req.max_visibility.as_deref() {
        None | Some("") => None,
        Some(v) => Some(parse_visibility(v)?),
    };
    groups::set_policy(
        &state,
        &p,
        &ns,
        groups::Policy {
            max_visibility: max,
            require_two_factor: req.require_two_factor,
            project_creation_level: parse_level(&req.project_creation_level)?,
            subgroup_creation_level: parse_level(&req.subgroup_creation_level)?,
        },
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
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
) -> ApiResult<Json<GroupDto>> {
    let ns = load_namespace(&state, &id).await?;
    let ns = groups::rename_path(&state, &p, &ns, &req.path).await?;
    Ok(Json(GroupDto::from(&ns, &state.cfg)))
}

#[derive(Deserialize, ToSchema)]
pub struct TransferGroup {
    /// New parent group, or null to make it an org.
    pub parent_id: Option<i64>,
}

pub async fn transfer(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<TransferGroup>,
) -> ApiResult<Json<GroupDto>> {
    let ns = load_namespace(&state, &id).await?;
    let ns = groups::transfer(&state, &p, &ns, req.parent_id).await?;
    Ok(Json(GroupDto::from(&ns, &state.cfg)))
}

pub async fn delete(State(state): State<AppState>, SignedIn(_, p): SignedIn, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let ns = load_namespace(&state, &id).await?;
    groups::delete(&state, &p, &ns).await?;
    Ok(Json(json!({ "ok": true })))
}

#[utoipa::path(get, path = "/api/v1/groups/{id}/subgroups", responses((status = 200, body = [GroupDto])), tag = "groups")]
pub async fn subgroups(State(state): State<AppState>, Viewer(p): Viewer, Path(id): Path<String>) -> ApiResult<Json<Vec<GroupDto>>> {
    let ns = group_for(&state, &p, &id, GroupAction::Read).await?;
    let signed_in = p.sees_internal();
    let list = forgust_db::namespaces::children_visible(&state.db, ns.id, p.direct_user().map(|u| u.id), p.is_admin(), signed_in).await?;
    Ok(Json(list.iter().map(|n| GroupDto::from(n, &state.cfg)).collect()))
}

#[utoipa::path(get, path = "/api/v1/groups/{id}/projects", responses((status = 200, body = [ProjectDto])), tag = "groups")]
pub async fn projects(State(state): State<AppState>, Viewer(p): Viewer, Path(id): Path<String>) -> ApiResult<Json<Vec<ProjectDto>>> {
    let ns = group_for(&state, &p, &id, GroupAction::Read).await?;
    let signed_in = p.sees_internal();
    let list = forgust_db::projects::in_namespace_visible(&state.db, ns.id, p.direct_user().map(|u| u.id), p.is_admin(), signed_in).await?;
    Ok(Json(list.iter().map(|x| ProjectDto::from(x, &state.cfg)).collect()))
}

#[utoipa::path(get, path = "/api/v1/groups/{id}/members", responses((status = 200, body = [MemberDto])), tag = "groups")]
pub async fn members(State(state): State<AppState>, Viewer(p): Viewer, Path(id): Path<String>) -> ApiResult<Json<Vec<MemberDto>>> {
    let ns = group_for(&state, &p, &id, GroupAction::Read).await?;
    if p.direct_user().is_none() {
        return Err(ApiError::Unauthorized);
    }
    Ok(Json(forgust_db::members::list(&state.db, "namespace", ns.id).await?.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct AddMember {
    pub user_id: Option<i64>,
    pub username: Option<String>,
    pub access_level: String,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn resolve_member(state: &AppState, req: &AddMember) -> ApiResult<i64> {
    if let Some(id) = req.user_id {
        return Ok(id);
    }
    let name = req.username.as_deref().ok_or_else(|| ApiError::bad("user_id or username is required"))?;
    Ok(forgust_db::users::find_by_username(&state.db, name).await?.ok_or(ApiError::NotFound)?.id)
}

pub async fn add_member(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<AddMember>,
) -> ApiResult<Json<Value>> {
    let ns = load_namespace(&state, &id).await?;
    let uid = resolve_member(&state, &req).await?;
    members::add(&state, &p, members::Source::Group(&ns), uid, parse_level(&req.access_level)?, req.expires_at).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove_member(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, user_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let ns = load_namespace(&state, &id).await?;
    members::remove(&state, &p, members::Source::Group(&ns), user_id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn shares(State(state): State<AppState>, SignedIn(_, p): SignedIn, Path(id): Path<String>) -> ApiResult<Json<Vec<ShareDto>>> {
    let ns = group_for(&state, &p, &id, GroupAction::Read).await?;
    Ok(Json(visible_shares(&state, &p, forgust_db::links::group_shares(&state.db, ns.id).await?).await?))
}

/// Shares whose invited group the viewer may see: a private group's path is
/// not told to whoever reads a public project.
pub(crate) async fn visible_shares(state: &AppState, p: &Principal, links: Vec<forgust_db::links::ShareLink>) -> ApiResult<Vec<ShareDto>> {
    let mut out = Vec::new();
    for l in links {
        if let Some(g) = forgust_db::namespaces::find(&state.db, l.group_id).await?
            && can_group(state, p, GroupAction::Read, &g).await?
        {
            out.push(l.into());
        }
    }
    Ok(out)
}

#[derive(Deserialize, ToSchema)]
pub struct ShareRequest {
    pub group_id: i64,
    pub max_access_level: String,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn share(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<ShareRequest>,
) -> ApiResult<Json<Value>> {
    let ns = load_namespace(&state, &id).await?;
    let with = forgust_db::namespaces::find(&state.db, req.group_id).await?.ok_or(ApiError::NotFound)?;
    members::share(&state, &p, members::Source::Group(&ns), &with, parse_level(&req.max_access_level)?, req.expires_at).await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn unshare(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, group_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let ns = load_namespace(&state, &id).await?;
    let with = forgust_db::namespaces::find(&state.db, group_id).await?.ok_or(ApiError::NotFound)?;
    members::unshare(&state, &p, members::Source::Group(&ns), &with).await?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ access tokens (bots)

#[derive(Deserialize, ToSchema)]
pub struct CreateAccessToken {
    pub name: String,
    pub access_level: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Project/group access tokens are personal tokens of a bot user that is a member
/// of exactly that project or group (ADR-0015).
pub async fn create_bot_token(
    state: &AppState,
    principal: &Principal,
    source: members::Source<'_>,
    req: CreateAccessToken,
) -> ApiResult<NewToken> {
    if !matches!(principal, Principal::User { scopes: None, .. }) {
        return Err(ApiError::forbidden("tokens can only be created from a signed-in session"));
    }
    services::paths::validate_name(&req.name)?;
    super::user::check_scopes(&req.scopes)?;
    if req.scopes.iter().any(|s| s == "admin_mode") {
        return Err(ApiError::bad("bots are never administrators: admin_mode does not apply"));
    }
    let level = parse_level(&req.access_level)?;
    let (source_type, source_id) = source.key();
    let mut tx = state.db.begin().await?;
    let bot = crate::services::users::create_bot(&mut tx, source_type, source_id, &req.name).await?;
    tx.commit().await?;
    if let Err(e) = members::add(state, principal, source, bot.id, level, req.expires_at).await {
        forgust_db::users::delete(&state.db, bot.id).await?;
        return Err(e);
    }
    let token = crypto::random_token(crypto::prefix::PERSONAL);
    let id = forgust_db::tokens::create_pat(&state.db, bot.id, &req.name, &crypto::token_hash(&token), &req.scopes, req.expires_at).await?;
    services::audit(
        state,
        principal,
        "access_token_created",
        Some((source_type, source_id)),
        json!({ "bot": bot.username, "level": level }),
        None,
    )
    .await;
    Ok(NewToken { id, token, username: Some(bot.username) })
}

pub async fn list_bot_tokens(state: &AppState, source_type: &str, source_id: i64) -> ApiResult<Vec<AccessTokenDto>> {
    let mut out = Vec::new();
    // every member's role in one query (not one per bot)
    let levels: std::collections::HashMap<i64, i16> =
        forgust_db::members::list(&state.db, source_type, source_id).await?.into_iter().map(|m| (m.user_id, m.access_level)).collect();
    // Forgust's own bots' (Renovate, scanners) run tokens are not the project's access tokens
    for bot in forgust_db::users::bots_for(&state.db, source_type, source_id).await?.into_iter().filter(|b| !b.is_builtin_bot()) {
        let level = levels.get(&bot.id).copied().unwrap_or(0);
        for t in forgust_db::tokens::list_pats(&state.db, bot.id).await? {
            out.push(AccessTokenDto {
                bot_user_id: bot.id,
                bot_username: bot.username.clone(),
                name: t.name,
                access_level: access::name(level).into(),
                scopes: t.scopes,
                expires_at: t.expires_at,
                revoked: t.revoked || bot.state != "active",
                last_used_at: t.last_used_at,
            });
        }
    }
    Ok(out)
}

/// Revoking blocks the bot and removes its membership.
pub async fn revoke_bot(state: &AppState, principal: &Principal, source: members::Source<'_>, bot_id: i64) -> ApiResult<()> {
    let (source_type, source_id) = source.key();
    let bot = forgust_db::users::find(&state.db, bot_id).await?.ok_or(ApiError::NotFound)?;
    if bot.is_builtin_bot() || !forgust_db::users::bots_for(&state.db, source_type, source_id).await?.iter().any(|b| b.id == bot.id) {
        return Err(ApiError::NotFound);
    }
    members::remove(state, principal, source, bot.id).await?;
    forgust_db::users::set_state(&state.db, bot.id, "blocked").await?;
    services::audit(state, principal, "access_token_revoked", Some((source_type, source_id)), json!({ "bot": bot.username }), None).await;
    Ok(())
}

pub async fn list_access_tokens(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<AccessTokenDto>>> {
    let ns = group_for(&state, &p, &id, GroupAction::Admin).await?;
    Ok(Json(list_bot_tokens(&state, "namespace", ns.id).await?))
}

pub async fn create_access_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<CreateAccessToken>,
) -> ApiResult<Json<NewToken>> {
    let ns = group_for(&state, &p, &id, GroupAction::Admin).await?;
    Ok(Json(create_bot_token(&state, &p, members::Source::Group(&ns), req).await?))
}

pub async fn revoke_access_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, bot_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let ns = group_for(&state, &p, &id, GroupAction::Admin).await?;
    revoke_bot(&state, &p, members::Source::Group(&ns), bot_id).await?;
    Ok(Json(json!({ "ok": true })))
}

// ------------------------------------------------------------------ deploy tokens

#[derive(Deserialize, ToSchema)]
pub struct CreateDeployToken {
    pub name: String,
    /// `read_repository`, `write_repository`, `read_registry`, `write_registry`.
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn create_deploy(
    state: &AppState,
    principal: &Principal,
    project_id: Option<i64>,
    namespace_id: Option<i64>,
    req: CreateDeployToken,
) -> ApiResult<NewToken> {
    services::paths::validate_name(&req.name)?;
    const DEPLOY_SCOPES: &[&str] = &["read_repository", "write_repository", "read_registry", "write_registry"];
    if req.scopes.is_empty() || req.scopes.iter().any(|s| !DEPLOY_SCOPES.contains(&s.as_str())) {
        return Err(ApiError::bad("deploy token scopes are read_repository, write_repository, read_registry and write_registry"));
    }
    let token = crypto::random_token(crypto::prefix::DEPLOY);
    let username = format!("forgust+deploy-token-{}", crypto::random_urlsafe(6).to_lowercase().replace(['-', '_'], "x"));
    let id = forgust_db::tokens::create_deploy_token(
        &state.db,
        &req.name,
        &username,
        &crypto::token_hash(&token),
        project_id,
        namespace_id,
        &req.scopes,
        req.expires_at,
    )
    .await?;
    let target = project_id.map(|i| ("project", i)).or(namespace_id.map(|i| ("namespace", i)));
    services::audit(state, principal, "deploy_token_created", target, json!({ "name": req.name, "scopes": req.scopes }), None).await;
    Ok(NewToken { id, token, username: Some(username) })
}

pub async fn list_deploy_tokens(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<DeployToken>>> {
    let ns = group_for(&state, &p, &id, GroupAction::Admin).await?;
    Ok(Json(forgust_db::tokens::list_deploy_tokens(&state.db, None, Some(ns.id)).await?))
}

pub async fn create_deploy_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<CreateDeployToken>,
) -> ApiResult<Json<NewToken>> {
    p.may_mint_credentials()?;
    let ns = group_for(&state, &p, &id, GroupAction::Admin).await?;
    Ok(Json(create_deploy(&state, &p, None, Some(ns.id), req).await?))
}

pub async fn revoke_deploy_token(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path((id, token_id)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let ns = group_for(&state, &p, &id, GroupAction::Admin).await?;
    if !forgust_db::tokens::revoke_deploy_token(&state.db, token_id, None, Some(ns.id)).await? {
        return Err(ApiError::NotFound);
    }
    services::audit(&state, &p, "deploy_token_revoked", Some(("namespace", ns.id)), json!({ "token_id": token_id }), None).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct Search {
    #[serde(default)]
    pub search: String,
}

pub type SearchQuery = Query<Search>;

/// What the group's organization keeps, against its storage quota.
#[utoipa::path(get, path = "/api/v1/groups/{id}/storage", responses((status = 200, body = super::dto::GroupStorageUse)), tag = "groups")]
pub async fn storage(
    State(state): State<AppState>,
    SignedIn(_, p): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<super::dto::GroupStorageUse>> {
    let ns = group_for(&state, &p, &id, GroupAction::Admin).await?;
    let root_id = ns.traversal_ids.first().copied().unwrap_or(ns.id);
    let root = forgust_db::namespaces::find(&state.db, root_id).await?.ok_or(ApiError::NotFound)?;
    // the organisation's figures are its Owners' (not a subgroup's)
    require_group(&state, &p, GroupAction::Admin, &root).await?;
    let quota = match forgust_db::quotas::namespace_own_quota(&state.db, root_id).await? {
        Some(0) => None,
        Some(q) => Some(q),
        None => forgust_db::quotas::default_quota(&state.db).await?,
    };
    Ok(Json(super::dto::GroupStorageUse {
        namespace: root.full_path,
        used: forgust_db::quotas::namespace_used(&state.db, root_id).await?,
        quota,
    }))
}
