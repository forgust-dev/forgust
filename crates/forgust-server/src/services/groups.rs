//! Orgs (top-level groups) and subgroups (ADR-0013).

use forgust_db::access::OWNER;
use forgust_db::namespaces::{Namespace, NewNamespace};
use serde_json::json;

use super::{audit, paths};
use crate::auth::Principal;
use crate::auth::authz::{GroupAction, require_group};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub struct CreateGroup<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub parent_id: Option<i64>,
    pub visibility: i16,
    pub description: &'a str,
}

/// The organization's visibility cap for anything under `ns` (none: public).
pub async fn org_cap(state: &AppState, ns: &Namespace) -> ApiResult<i16> {
    Ok(match forgust_db::namespaces::root_policy(&state.db, ns.id).await? {
        Some((Some(max), _)) => max,
        _ => forgust_db::visibility::PUBLIC,
    })
}

/// The widest visibility something may have in `ns`: its group's, and its
/// organization's maximum.
pub async fn max_visibility(state: &AppState, ns: &Namespace) -> ApiResult<i16> {
    let group = if ns.is_group() { ns.visibility } else { forgust_db::visibility::PUBLIC };
    Ok(group.min(org_cap(state, ns).await?))
}

/// What a group or project in `ns` may not exceed, said plainly.
pub async fn check_visibility_in(state: &AppState, ns: &Namespace, wanted: i16, what: &str) -> ApiResult<()> {
    if ns.is_group() && wanted > ns.visibility {
        return Err(ApiError::bad(format!("visibility cannot be wider than the {what} ({})", forgust_db::visibility::name(ns.visibility))));
    }
    let cap = org_cap(state, ns).await?;
    if wanted > cap {
        return Err(ApiError::bad(format!("the organization allows at most {} visibility", forgust_db::visibility::name(cap))));
    }
    Ok(())
}

/// A group's visibility may not be wider than its parent's or the org's maximum.
pub async fn check_visibility(state: &AppState, parent: Option<&Namespace>, wanted: i16) -> ApiResult<()> {
    match parent {
        Some(p) => check_visibility_in(state, p, wanted, "parent group's").await,
        None => Ok(()),
    }
}

pub async fn create(state: &AppState, principal: &Principal, req: CreateGroup<'_>) -> ApiResult<Namespace> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    if user.is_bot() {
        return Err(ApiError::forbidden("bots cannot create groups"));
    }
    paths::validate_name(req.name)?;
    let parent = match req.parent_id {
        Some(pid) => {
            let p = forgust_db::namespaces::find(&state.db, pid).await?.ok_or(ApiError::NotFound)?;
            if !p.is_group() {
                return Err(ApiError::bad("subgroups can only be created inside groups"));
            }
            require_group(state, principal, GroupAction::CreateSubgroup, &p).await?;
            if p.depth() >= paths::MAX_DEPTH {
                return Err(ApiError::bad(format!("groups can be nested at most {} levels deep", paths::MAX_DEPTH)));
            }
            paths::validate_segment(req.path)?;
            Some(p)
        }
        None => {
            paths::validate_top_level(req.path)?;
            None
        }
    };
    check_visibility(state, parent.as_ref(), req.visibility).await?;
    let full_path = paths::join(parent.as_ref().map(|p| p.full_path.as_str()), req.path);

    let mut tx = state.db.begin().await?;
    if forgust_db::routes::path_taken(&mut *tx, &full_path).await? {
        return Err(ApiError::conflict(format!("`{full_path}` is already taken")));
    }
    let id = forgust_db::namespaces::insert(
        &mut tx,
        NewNamespace {
            kind: "group",
            name: req.name,
            path: req.path,
            parent_id: req.parent_id,
            visibility: req.visibility,
            description: req.description,
            owner_id: None,
        },
    )
    .await?;
    forgust_db::routes::insert(&mut *tx, "namespace", id, &full_path).await?;
    // the creator owns what they create (inherited ownership makes this a no-op
    // for subgroups of groups they already own)
    let inherited = match &parent {
        Some(p) => forgust_db::authz::namespace_level(&mut *tx, user.id, p.id).await?.unwrap_or(0),
        None => 0,
    };
    if inherited < OWNER {
        forgust_db::members::upsert(&mut *tx, "namespace", id, user.id, OWNER, None, Some(user.id)).await?;
    }
    let affected = forgust_db::members::affected_by_namespace(&mut *tx, id).await?;
    forgust_db::members::refresh_authorizations(&mut tx, &affected).await?;
    tx.commit().await?;

    audit(state, principal, "group_created", Some(("namespace", id)), json!({ "path": full_path }), None).await;
    forgust_db::namespaces::find(&state.db, id).await?.ok_or(ApiError::NotFound)
}

pub async fn update(
    state: &AppState,
    principal: &Principal,
    ns: &Namespace,
    name: &str,
    description: &str,
    visibility: i16,
) -> ApiResult<()> {
    require_group(state, principal, GroupAction::Admin, ns).await?;
    paths::validate_name(name)?;
    let parent = match ns.parent_id {
        Some(p) => forgust_db::namespaces::find(&state.db, p).await?,
        None => None,
    };
    check_visibility(state, parent.as_ref(), visibility).await?;
    if parent.is_none()
        && let Some(max) = ns.max_visibility
        && visibility > max
    {
        return Err(ApiError::bad("visibility is wider than the organization allows"));
    }
    let widest_child = forgust_db::namespaces::max_descendant_visibility(&state.db, ns.id).await?;
    if visibility < widest_child {
        return Err(ApiError::bad("a subgroup or project is more visible than this; restrict it first"));
    }
    forgust_db::namespaces::update_details(&state.db, ns.id, name, description, visibility).await?;
    audit(state, principal, "group_updated", Some(("namespace", ns.id)), json!({ "visibility": visibility }), None).await;
    Ok(())
}

pub struct Policy {
    pub max_visibility: Option<i16>,
    pub require_two_factor: bool,
    pub project_creation_level: i16,
    pub subgroup_creation_level: i16,
}

/// Org policy lives on top-level groups only.
pub async fn set_policy(state: &AppState, principal: &Principal, ns: &Namespace, p: Policy) -> ApiResult<()> {
    require_group(state, principal, GroupAction::Own, ns).await?;
    if ns.parent_id.is_some() || !ns.is_group() {
        return Err(ApiError::bad("policy can only be set on an organization (top-level group)"));
    }
    if let Some(max) = p.max_visibility {
        let widest = forgust_db::namespaces::max_descendant_visibility(&state.db, ns.id).await?.max(ns.visibility);
        if widest > max {
            return Err(ApiError::bad("some groups or projects are more visible than the new maximum; restrict them first"));
        }
    }
    for lvl in [p.project_creation_level, p.subgroup_creation_level] {
        if ![30, 40, 50].contains(&lvl) {
            return Err(ApiError::bad("creation levels must be developer, maintainer or owner"));
        }
    }
    forgust_db::namespaces::update_policy(
        &state.db,
        ns.id,
        p.max_visibility,
        p.require_two_factor,
        p.project_creation_level,
        p.subgroup_creation_level,
    )
    .await?;
    audit(
        state,
        principal,
        "org_policy_updated",
        Some(("namespace", ns.id)),
        json!({ "max_visibility": p.max_visibility, "require_two_factor": p.require_two_factor }),
        None,
    )
    .await;
    Ok(())
}

/// Change the last path segment; the old paths keep redirecting.
pub async fn rename_path(state: &AppState, principal: &Principal, ns: &Namespace, new_path: &str) -> ApiResult<Namespace> {
    require_group(state, principal, GroupAction::Own, ns).await?;
    if ns.parent_id.is_none() {
        paths::validate_top_level(new_path)?;
    } else {
        paths::validate_segment(new_path)?;
    }
    let parent_full = ns.full_path.rsplit_once('/').map(|(p, _)| p);
    let new_full = paths::join(parent_full, new_path);
    let mut tx = state.db.begin().await?;
    if !new_full.eq_ignore_ascii_case(&ns.full_path) && forgust_db::routes::path_taken(&mut *tx, &new_full).await? {
        return Err(ApiError::conflict(format!("`{new_full}` is already taken")));
    }
    forgust_db::namespaces::set_path(&mut *tx, ns.id, new_path).await?;
    forgust_db::routes::move_prefix(&mut tx, &ns.full_path, &new_full).await?;
    tx.commit().await?;
    audit(state, principal, "group_path_changed", Some(("namespace", ns.id)), json!({ "from": ns.full_path, "to": new_full }), None).await;
    forgust_db::namespaces::find(&state.db, ns.id).await?.ok_or(ApiError::NotFound)
}

/// Move a group under another group, or to the top level (it becomes an org).
pub async fn transfer(state: &AppState, principal: &Principal, ns: &Namespace, new_parent: Option<i64>) -> ApiResult<Namespace> {
    require_group(state, principal, GroupAction::Own, ns).await?;
    if !ns.is_group() {
        return Err(ApiError::bad("only groups can be transferred"));
    }
    let parent = match new_parent {
        Some(pid) => {
            let p = forgust_db::namespaces::find(&state.db, pid).await?.ok_or(ApiError::NotFound)?;
            if !p.is_group() {
                return Err(ApiError::bad("the target must be a group"));
            }
            if p.traversal_ids.contains(&ns.id) {
                return Err(ApiError::bad("a group cannot be moved inside itself"));
            }
            require_group(state, principal, GroupAction::CreateSubgroup, &p).await?;
            let height = forgust_db::namespaces::subtree_height(&state.db, ns.id).await? as usize;
            if p.depth() + height > paths::MAX_DEPTH {
                return Err(ApiError::bad(format!("the result would be deeper than {} levels", paths::MAX_DEPTH)));
            }
            Some(p)
        }
        None => {
            paths::validate_top_level(&ns.path)?;
            None
        }
    };
    if new_parent == ns.parent_id {
        return Ok(ns.clone());
    }
    let widest = forgust_db::namespaces::max_descendant_visibility(&state.db, ns.id).await?.max(ns.visibility);
    check_visibility(state, parent.as_ref(), widest).await?;
    let new_full = paths::join(parent.as_ref().map(|p| p.full_path.as_str()), &ns.path);

    let mut tx = state.db.begin().await?;
    if forgust_db::routes::path_taken(&mut *tx, &new_full).await? {
        return Err(ApiError::conflict(format!("`{new_full}` is already taken")));
    }
    let mut affected = forgust_db::members::affected_by_namespace(&mut *tx, ns.id).await?;
    forgust_db::namespaces::reparent(&mut tx, ns.id, new_parent).await?;
    forgust_db::routes::move_prefix(&mut tx, &ns.full_path, &new_full).await?;
    affected.extend(forgust_db::members::affected_by_namespace(&mut *tx, ns.id).await?);
    affected.sort_unstable();
    affected.dedup();
    forgust_db::members::refresh_authorizations(&mut tx, &affected).await?;
    // whoever moves a group keeps owning it (e.g. a subgroup made top-level has no
    // inherited owners any more)
    if let Some(user) = principal.direct_user() {
        let level = forgust_db::authz::namespace_level(&mut *tx, user.id, ns.id).await?.unwrap_or(0);
        if level < OWNER && !user.is_admin {
            forgust_db::members::upsert(&mut *tx, "namespace", ns.id, user.id, OWNER, None, Some(user.id)).await?;
            forgust_db::members::refresh_authorizations(&mut tx, &[user.id]).await?;
        }
    }
    tx.commit().await?;
    audit(state, principal, "group_transferred", Some(("namespace", ns.id)), json!({ "from": ns.full_path, "to": new_full }), None).await;
    forgust_db::namespaces::find(&state.db, ns.id).await?.ok_or(ApiError::NotFound)
}

/// Delete an empty group.
pub async fn delete(state: &AppState, principal: &Principal, ns: &Namespace) -> ApiResult<()> {
    require_group(state, principal, GroupAction::Own, ns).await?;
    if !ns.is_group() {
        return Err(ApiError::bad("personal namespaces are deleted with their user"));
    }
    if forgust_db::namespaces::has_children(&state.db, ns.id).await? {
        return Err(ApiError::conflict("delete or move its subgroups and projects first"));
    }
    let mut tx = state.db.begin().await?;
    let affected = forgust_db::members::affected_by_namespace(&mut *tx, ns.id).await?;
    forgust_db::members::remove_all_for_source(&mut *tx, "namespace", ns.id).await?;
    forgust_db::routes::delete_for(&mut tx, "namespace", ns.id).await?;
    forgust_db::namespaces::delete(&mut *tx, ns.id).await?;
    forgust_db::members::refresh_authorizations(&mut tx, &affected).await?;
    tx.commit().await?;
    audit(state, principal, "group_deleted", Some(("namespace", ns.id)), json!({ "path": ns.full_path }), None).await;
    Ok(())
}
