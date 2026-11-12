//! Memberships and sharing (ADR-0006).

use chrono::{DateTime, Utc};
use forgust_db::access::OWNER;
use forgust_db::namespaces::Namespace;
use forgust_db::projects::Project;
use serde_json::json;

use super::audit;
use crate::auth::Principal;
use crate::auth::authz::{self, GroupAction, ProjectAction, require_group, require_project};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Clone, Copy)]
pub enum Source<'a> {
    Group(&'a Namespace),
    Project(&'a Project),
}

impl Source<'_> {
    /// `(source_type, source_id)` as stored in `members`.
    pub(crate) fn key(&self) -> (&'static str, i64) {
        match self {
            Source::Group(n) => ("namespace", n.id),
            Source::Project(p) => ("project", p.id),
        }
    }
}

async fn granter_level(state: &AppState, principal: &Principal, source: Source<'_>) -> ApiResult<i16> {
    match source {
        Source::Group(ns) => {
            require_group(state, principal, GroupAction::ManageMembers, ns).await?;
            Ok(authz::group_level(state, principal, ns).await?.0)
        }
        Source::Project(p) => {
            require_project(state, principal, ProjectAction::Admin, p).await?;
            Ok(authz::project_role(state, principal, p).await?.level)
        }
    }
}

pub async fn add(
    state: &AppState,
    principal: &Principal,
    source: Source<'_>,
    user_id: i64,
    level: i16,
    expires_at: Option<DateTime<Utc>>,
) -> ApiResult<()> {
    if let Source::Group(ns) = source
        && !ns.is_group()
    {
        return Err(ApiError::bad("personal namespaces have no members"));
    }
    if ![10, 20, 30, 40, 50].contains(&level) {
        return Err(ApiError::bad("invalid access level"));
    }
    let mine = granter_level(state, principal, source).await?;
    if !authz::may_grant(mine, level) {
        return Err(ApiError::forbidden("you cannot grant a role higher than your own"));
    }
    let target = forgust_db::users::find(&state.db, user_id).await?.ok_or(ApiError::NotFound)?;
    let (st, sid) = source.key();
    if let Some(existing) = forgust_db::members::level_of(&state.db, st, sid, user_id).await? {
        if existing == OWNER && level < OWNER && st == "namespace" && forgust_db::members::count_owners(&state.db, st, sid).await? <= 1 {
            return Err(ApiError::conflict("a group needs at least one owner"));
        }
        if !authz::may_grant(mine, existing) {
            return Err(ApiError::forbidden("you cannot change the role of a member above you"));
        }
    }
    let mut tx = state.db.begin().await?;
    forgust_db::members::upsert(&mut *tx, st, sid, user_id, level, expires_at, principal.id()).await?;
    forgust_db::members::refresh_authorizations(&mut tx, &[user_id]).await?;
    tx.commit().await?;
    audit(state, principal, "member_set", Some((st, sid)), json!({ "user": target.username, "level": level }), None).await;
    Ok(())
}

pub async fn remove(state: &AppState, principal: &Principal, source: Source<'_>, user_id: i64) -> ApiResult<()> {
    let (st, sid) = source.key();
    let is_self = principal.direct_user().is_some_and(|u| u.id == user_id);
    let existing = forgust_db::members::level_of(&state.db, st, sid, user_id).await?.ok_or(ApiError::NotFound)?;
    if !is_self {
        let mine = granter_level(state, principal, source).await?;
        if !authz::may_grant(mine, existing) {
            return Err(ApiError::forbidden("you cannot remove a member above you"));
        }
    }
    if existing == OWNER && st == "namespace" && forgust_db::members::count_owners(&state.db, st, sid).await? <= 1 {
        return Err(ApiError::conflict("a group needs at least one owner"));
    }
    let mut tx = state.db.begin().await?;
    forgust_db::members::remove(&mut *tx, st, sid, user_id).await?;
    forgust_db::members::refresh_authorizations(&mut tx, &[user_id]).await?;
    tx.commit().await?;
    audit(state, principal, "member_removed", Some((st, sid)), json!({ "user_id": user_id }), None).await;
    Ok(())
}

/// Share a project or group with another group, capped at `max`.
pub async fn share(
    state: &AppState,
    principal: &Principal,
    source: Source<'_>,
    with_group: &Namespace,
    max: i16,
    expires_at: Option<DateTime<Utc>>,
) -> ApiResult<()> {
    if !with_group.is_group() {
        return Err(ApiError::bad("you can only share with a group"));
    }
    if ![10, 20, 30, 40, 50].contains(&max) {
        return Err(ApiError::bad("invalid access level"));
    }
    let mine = granter_level(state, principal, source).await?;
    if !authz::may_grant(mine, max) {
        return Err(ApiError::forbidden("you cannot share with a role higher than your own"));
    }
    require_group(state, principal, GroupAction::Read, with_group).await?;
    let mut tx = state.db.begin().await?;
    match source {
        Source::Project(p) => forgust_db::links::share_project(&mut *tx, p.id, with_group.id, max, expires_at).await?,
        Source::Group(ns) => {
            if ns.id == with_group.id || with_group.traversal_ids.contains(&ns.id) || ns.traversal_ids.contains(&with_group.id) {
                return Err(ApiError::bad("a group cannot be shared with itself, its ancestors or descendants"));
            }
            forgust_db::links::share_group(&mut *tx, ns.id, with_group.id, max, expires_at).await?
        }
    }
    let users = forgust_db::members::group_member_ids_with_ancestors(&mut *tx, with_group.id).await?;
    forgust_db::members::refresh_authorizations(&mut tx, &users).await?;
    tx.commit().await?;
    let (st, sid) = source.key();
    audit(state, principal, "shared_with_group", Some((st, sid)), json!({ "group": with_group.full_path, "max": max }), None).await;
    Ok(())
}

pub async fn unshare(state: &AppState, principal: &Principal, source: Source<'_>, with_group: &Namespace) -> ApiResult<()> {
    granter_level(state, principal, source).await?;
    let mut tx = state.db.begin().await?;
    let removed = match source {
        Source::Project(p) => forgust_db::links::unshare_project(&mut *tx, p.id, with_group.id).await?,
        Source::Group(ns) => forgust_db::links::unshare_group(&mut *tx, ns.id, with_group.id).await?,
    };
    if !removed {
        return Err(ApiError::NotFound);
    }
    let users = forgust_db::members::group_member_ids_with_ancestors(&mut *tx, with_group.id).await?;
    forgust_db::members::refresh_authorizations(&mut tx, &users).await?;
    tx.commit().await?;
    let (st, sid) = source.key();
    audit(state, principal, "unshared_with_group", Some((st, sid)), json!({ "group": with_group.full_path }), None).await;
    Ok(())
}
