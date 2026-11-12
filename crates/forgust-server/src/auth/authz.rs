//! The one authorization function (ADR-0015): `can(principal, action, resource)`.
//! Handlers never compare roles themselves.

use forgust_db::access::{DEVELOPER, GUEST, MAINTAINER, OWNER, REPORTER};
use forgust_db::namespaces::Namespace;
use forgust_db::projects::Project;
use forgust_db::visibility::{INTERNAL, PUBLIC};
use serde_json::json;

use super::Principal;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectAction {
    /// See that the project exists (name, description).
    Read,
    /// Clone, browse files and history.
    ReadCode,
    /// Push (protected branches are checked again in the pre-receive hook).
    Push,
    /// Manage members, protected branches, deploy keys, tokens.
    Admin,
    /// Rename the path, transfer, delete.
    Own,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupAction {
    Read,
    CreateProject,
    CreateSubgroup,
    ManageMembers,
    Admin,
    Own,
}

/// Effective role of a principal on a project, after visibility and policy.
#[derive(Debug, Clone, Copy)]
pub struct ProjectRole {
    /// Membership level (0 = none). Admins get Owner via bypass.
    pub level: i16,
    pub bypass: bool,
    pub can_read: bool,
    pub can_read_code: bool,
    pub can_push: bool,
}

fn visible_by_visibility(principal: &Principal, visibility: i16) -> bool {
    match visibility {
        PUBLIC => true,
        INTERNAL => principal.sees_internal(),
        _ => false,
    }
}

pub async fn project_role(state: &AppState, principal: &Principal, project: &Project) -> ApiResult<ProjectRole> {
    let open = visible_by_visibility(principal, project.visibility);
    let mut role = ProjectRole { level: 0, bypass: false, can_read: open, can_read_code: open, can_push: false };
    match principal {
        Principal::Anonymous => {}
        Principal::User { user, scopes } => {
            let root = project.traversal_ids.first().copied().unwrap_or(project.namespace_id);
            let (level, require_2fa) = forgust_db::authz::project_level_and_2fa(&state.db, user.id, project.id, root).await?;
            let level = level.unwrap_or(0);
            if level > 0 && require_2fa && !user.is_bot() && !user.totp_enabled && !user.is_admin {
                return Err(ApiError::TwoFactorRequired);
            }
            role.level = level;
            if user.is_admin && level < OWNER {
                role.level = OWNER;
                role.bypass = true;
            }
            role.can_read |= role.level >= GUEST;
            role.can_read_code |= role.level >= REPORTER;
            role.can_push = role.level >= DEVELOPER;
            if let Some(scopes) = scopes {
                // token scopes narrow what the user could do
                let read_ok =
                    scopes.iter().any(|s| matches!(s.as_str(), "api" | "read_api" | "read_repository" | "write_repository" | "mcp"));
                // `mcp_write` (OAuth consent for an MCP client): REST refuses it (see
                // `Viewer`), so it only writes through MCP's tools and git
                let write_ok = scopes.iter().any(|s| matches!(s.as_str(), "api" | "write_repository" | "mcp_write"));
                role.can_read_code &= read_ok;
                role.can_push &= write_ok;
            }
        }
        Principal::DeployKey { key_id } => {
            if let Some(can_push) = forgust_db::ssh_keys::deploy_key_access(&state.db, *key_id, project.id).await? {
                role.can_read = true;
                role.can_read_code = true;
                role.can_push = can_push;
            }
        }
        Principal::DeployToken { token } => {
            let applies = token.project_id == Some(project.id) || token.namespace_id.is_some_and(|ns| project.traversal_ids.contains(&ns));
            if applies {
                let read = token.scopes.iter().any(|s| s == "read_repository" || s == "write_repository");
                role.can_read |= read;
                role.can_read_code |= read;
                role.can_push = token.scopes.iter().any(|s| s == "write_repository");
            }
        }
        Principal::Job { project_id, user, .. } => {
            // its own project, or one whose job token allowlist names it
            // (read-only, and never more than the job's user may see)
            if *project_id == project.id || forgust_db::ci::job_token_allowed(&state.db, project.id, *project_id).await? {
                let level = forgust_db::authz::project_level(&state.db, user.id, project.id).await?.unwrap_or(0);
                role.can_read = *project_id == project.id || level > 0 || open || user.is_admin;
                role.can_read_code = level >= REPORTER || open || user.is_admin;
            }
        }
    }
    // archived: read-only for everyone (its Owner may still unarchive, transfer, delete)
    if project.archived() {
        role.can_push = false;
    }
    Ok(role)
}

pub const ARCHIVED: &str = "this project is archived: it is read-only (its Owner can unarchive it in Settings)";

/// Refuse writes to an archived project, for writes that need less than
/// `Push` (comments, issues, reactions, uploads, approvals...).
pub fn writable(project: &Project) -> ApiResult<()> {
    if project.archived() { Err(ApiError::forbidden(ARCHIVED)) } else { Ok(()) }
}

/// What `role` lets the principal do (no queries).
fn allows(principal: &Principal, role: &ProjectRole, action: ProjectAction, project: &Project) -> bool {
    match action {
        ProjectAction::Read => role.can_read,
        ProjectAction::ReadCode => role.can_read_code,
        ProjectAction::Push => role.can_push,
        ProjectAction::Admin => principal.direct_user().is_some() && role.level >= MAINTAINER && !project.archived(),
        ProjectAction::Own => principal.direct_user().is_some() && role.level >= OWNER,
    }
}

/// `can(principal, action, project)`. Admin bypasses on write actions are audited.
pub async fn can_project(state: &AppState, principal: &Principal, action: ProjectAction, project: &Project) -> ApiResult<bool> {
    let role = project_role(state, principal, project).await?;
    let ok = allows(principal, &role, action, project);
    if ok && role.bypass && !matches!(action, ProjectAction::Read | ProjectAction::ReadCode) {
        audit_bypass(state, principal, "project", project.id, &format!("{action:?}")).await;
    }
    Ok(ok)
}

/// Like [`can_project`], but hides projects the principal cannot see (404) and
/// returns 403 when it can see but not act (the role worked out once).
pub async fn require_project(state: &AppState, principal: &Principal, action: ProjectAction, project: &Project) -> ApiResult<()> {
    let role = project_role(state, principal, project).await?;
    if allows(principal, &role, action, project) {
        if role.bypass && !matches!(action, ProjectAction::Read | ProjectAction::ReadCode) {
            audit_bypass(state, principal, "project", project.id, &format!("{action:?}")).await;
        }
        return Ok(());
    }
    if action != ProjectAction::Read && allows(principal, &role, ProjectAction::Read, project) {
        return Err(if matches!(principal, Principal::Anonymous) {
            ApiError::Unauthorized
        } else if project.archived() && matches!(action, ProjectAction::Push | ProjectAction::Admin) {
            ApiError::forbidden(ARCHIVED)
        } else {
            ApiError::forbidden("insufficient permissions")
        });
    }
    Err(ApiError::NotFound)
}

pub async fn group_level(state: &AppState, principal: &Principal, ns: &Namespace) -> ApiResult<(i16, bool)> {
    let Some(user) = principal.direct_user() else { return Ok((0, false)) };
    let (level, require_2fa) = forgust_db::authz::namespace_level_and_2fa(&state.db, user.id, ns.id, ns.root_id()).await?;
    let level = level.unwrap_or(0);
    if level > 0 && require_2fa && !user.is_bot() && !user.totp_enabled && !user.is_admin {
        return Err(ApiError::TwoFactorRequired);
    }
    if user.is_admin && level < OWNER {
        return Ok((OWNER, true));
    }
    Ok((level, false))
}

pub async fn can_group(state: &AppState, principal: &Principal, action: GroupAction, ns: &Namespace) -> ApiResult<bool> {
    let (level, bypass) = group_level(state, principal, ns).await?;
    let ok = group_allows(state, principal, action, ns, level).await?;
    if ok && bypass && action != GroupAction::Read {
        audit_bypass(state, principal, "namespace", ns.id, &format!("{action:?}")).await;
    }
    Ok(ok)
}

/// What `level` (from [`group_level`]) lets the principal do in `ns`.
async fn group_allows(state: &AppState, principal: &Principal, action: GroupAction, ns: &Namespace, level: i16) -> ApiResult<bool> {
    let root_policy = || async {
        // the group itself when it is the organisation
        if ns.root_id() == ns.id {
            return Ok::<_, ApiError>((ns.project_creation_level, ns.subgroup_creation_level));
        }
        let root = forgust_db::namespaces::find(&state.db, ns.root_id()).await?.ok_or(ApiError::NotFound)?;
        Ok((root.project_creation_level, root.subgroup_creation_level))
    };
    let ok = match action {
        GroupAction::Read => {
            level >= GUEST
                || visible_by_visibility(principal, ns.visibility)
                || match principal.direct_user() {
                    Some(u) => forgust_db::authz::has_descendant_access(&state.db, u.id, ns.id).await?,
                    None => false,
                }
        }
        GroupAction::CreateProject => {
            if !ns.is_group() {
                // personal namespace: only its owner
                principal.direct_user().is_some_and(|u| ns.owner_id == Some(u.id) || u.is_admin)
            } else {
                level >= root_policy().await?.0.max(DEVELOPER)
            }
        }
        GroupAction::CreateSubgroup => ns.is_group() && level >= root_policy().await?.1.max(MAINTAINER),
        GroupAction::ManageMembers => ns.is_group() && level >= MAINTAINER,
        GroupAction::Admin => level >= OWNER,
        GroupAction::Own => level >= OWNER,
    };
    Ok(ok)
}

pub async fn require_group(state: &AppState, principal: &Principal, action: GroupAction, ns: &Namespace) -> ApiResult<()> {
    // the level worked out once, for the action and for the 404/403 choice
    let (level, bypass) = group_level(state, principal, ns).await?;
    if group_allows(state, principal, action, ns, level).await? {
        if bypass && action != GroupAction::Read {
            audit_bypass(state, principal, "namespace", ns.id, &format!("{action:?}")).await;
        }
        return Ok(());
    }
    if action != GroupAction::Read && group_allows(state, principal, GroupAction::Read, ns, level).await? {
        return Err(ApiError::forbidden("insufficient permissions"));
    }
    Err(ApiError::NotFound)
}

async fn audit_bypass(state: &AppState, principal: &Principal, target_type: &str, target_id: i64, action: &str) {
    let r = forgust_db::audit::insert(
        &state.db,
        forgust_db::audit::NewEvent {
            principal_kind: "admin",
            principal_id: principal.id(),
            action: "admin_bypass",
            target_type: Some(target_type),
            target_id: Some(target_id),
            details: json!({ "action": action }),
            ip: None,
        },
    )
    .await;
    if let Err(e) = r {
        tracing::warn!(error = %e, "failed to audit admin bypass");
    }
}

/// Members may grant at most their own level; only owners grant Owner.
pub fn may_grant(granter_level: i16, wanted: i16) -> bool {
    granter_level >= OWNER || (granter_level >= MAINTAINER && wanted <= MAINTAINER)
}
