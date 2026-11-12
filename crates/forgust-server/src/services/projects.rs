use forgust_db::access::{MAINTAINER, OWNER};
use forgust_db::namespaces::Namespace;
use forgust_db::projects::{NewProject, Project};
use forgust_storage::{CreateRepoOptions, RefFormat};
use serde_json::json;

use super::{audit, paths};
use crate::auth::Principal;
use crate::auth::authz::{GroupAction, ProjectAction, require_group, require_project};
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::state::AppState;

pub struct CreateProject<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub description: &'a str,
    pub visibility: i16,
    pub default_branch: Option<&'a str>,
    pub ref_format: RefFormat,
}

async fn check_visibility(state: &AppState, ns: &Namespace, wanted: i16) -> ApiResult<()> {
    super::groups::check_visibility_in(state, ns, wanted, "group's").await
}

pub async fn create(state: &AppState, principal: &Principal, ns: &Namespace, req: CreateProject<'_>) -> ApiResult<Project> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    require_group(state, principal, GroupAction::CreateProject, ns).await?;
    if !ns.is_group() {
        let settings = forgust_db::settings::get(&state.db).await?;
        if !settings.allow_personal_projects && !user.is_admin {
            return Err(ApiError::forbidden("personal projects are disabled on this instance"));
        }
    }
    paths::validate_name(req.name)?;
    paths::validate_segment(req.path)?;
    check_visibility(state, ns, req.visibility).await?;
    let settings = forgust_db::settings::get(&state.db).await?;
    let branch = req.default_branch.filter(|b| !b.is_empty()).unwrap_or(&settings.default_branch_name).to_string();
    if !forgust_storage::validate_rev(&branch) || branch.contains('/') && branch.ends_with('/') {
        return Err(ApiError::bad("invalid default branch name"));
    }
    let full_path = format!("{}/{}", ns.full_path, req.path);

    let mut tx = state.db.begin().await?;
    if forgust_db::routes::path_taken(&mut *tx, &full_path).await? {
        return Err(ApiError::conflict(format!("`{full_path}` is already taken")));
    }
    let id = forgust_db::projects::insert(
        &mut *tx,
        NewProject {
            namespace_id: ns.id,
            name: req.name,
            path: req.path,
            description: req.description,
            visibility: req.visibility,
            default_branch: &branch,
            ref_format: req.ref_format.as_str(),
        },
    )
    .await?;
    forgust_db::routes::insert(&mut *tx, "project", id, &full_path).await?;
    // the default branch is protected from the start: Maintainers push and merge
    forgust_db::protected_branches::upsert(&mut *tx, id, &branch, MAINTAINER, MAINTAINER, false, false).await?;
    let inherited = forgust_db::authz::namespace_level(&mut *tx, user.id, ns.id).await?.unwrap_or(0);
    if inherited < MAINTAINER {
        forgust_db::members::upsert(&mut *tx, "project", id, user.id, if ns.is_group() { MAINTAINER } else { OWNER }, None, Some(user.id))
            .await?;
    }
    let mut affected = forgust_db::members::affected_by_namespace(&mut *tx, ns.id).await?;
    affected.push(user.id);
    forgust_db::members::refresh_authorizations(&mut tx, &affected).await?;
    tx.commit().await?;

    if let Err(e) = state.storage.create_repo(id, CreateRepoOptions { default_branch: branch.clone(), ref_format: req.ref_format }).await {
        // roll back the metadata so the path is free again
        let mut tx = state.db.begin().await?;
        forgust_db::routes::delete_for(&mut tx, "project", id).await?;
        forgust_db::projects::delete(&mut *tx, id).await?;
        tx.commit().await?;
        return Err(e.into());
    }
    audit(state, principal, "project_created", Some(("project", id)), json!({ "path": full_path }), None).await;
    let project = forgust_db::projects::find(&state.db, id).await?.ok_or(ApiError::NotFound)?;
    // a group above turned dependency updates on for its projects
    super::renovate::on_project_created(state, principal, &project).await;
    super::scanning::on_project_created(state, principal, &project).await;
    Ok(project)
}

pub async fn update(
    state: &AppState,
    principal: &Principal,
    p: &Project,
    name: &str,
    description: &str,
    visibility: i16,
    default_branch: Option<&str>,
) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Admin, p).await?;
    // who sees the project (making private code public) is the Owners' call,
    // as a group's visibility is
    if visibility != p.visibility {
        require_project(state, principal, ProjectAction::Own, p).await?;
    }
    paths::validate_name(name)?;
    let ns = forgust_db::namespaces::find(&state.db, p.namespace_id).await?.ok_or(ApiError::NotFound)?;
    check_visibility(state, &ns, visibility).await?;
    if let Some(b) = default_branch.filter(|b| Some(*b) != p.default_branch.as_deref()) {
        if state.storage.resolve(p.id, &format!("refs/heads/{b}")).await?.is_none() {
            return Err(ApiError::bad(format!("branch `{b}` does not exist")));
        }
        state.storage.set_default_branch(p.id, b).await?;
        forgust_db::projects::set_default_branch(&state.db, p.id, b).await?;
    }
    forgust_db::projects::update_details(&state.db, p.id, name, description, visibility).await?;
    audit(state, principal, "project_updated", Some(("project", p.id)), json!({ "visibility": visibility }), None).await;
    Ok(())
}

pub async fn rename_path(state: &AppState, principal: &Principal, p: &Project, new_path: &str) -> ApiResult<Project> {
    require_project(state, principal, ProjectAction::Own, p).await?;
    paths::validate_segment(new_path)?;
    let ns_full = p.full_path.rsplit_once('/').map(|(a, _)| a).unwrap_or("");
    let new_full = format!("{ns_full}/{new_path}");
    let mut tx = state.db.begin().await?;
    if !new_full.eq_ignore_ascii_case(&p.full_path) && forgust_db::routes::path_taken(&mut *tx, &new_full).await? {
        return Err(ApiError::conflict(format!("`{new_full}` is already taken")));
    }
    forgust_db::projects::set_path(&mut *tx, p.id, new_path).await?;
    forgust_db::routes::move_prefix(&mut tx, &p.full_path, &new_full).await?;
    tx.commit().await?;
    audit(state, principal, "project_path_changed", Some(("project", p.id)), json!({ "from": p.full_path, "to": new_full }), None).await;
    forgust_db::projects::find(&state.db, p.id).await?.ok_or(ApiError::NotFound)
}

/// Move a project to another namespace. Repo files never move (ADR-0009).
pub async fn transfer(state: &AppState, principal: &Principal, p: &Project, target: &Namespace) -> ApiResult<Project> {
    require_project(state, principal, ProjectAction::Own, p).await?;
    require_group(state, principal, GroupAction::CreateProject, target).await?;
    if target.id == p.namespace_id {
        return Ok(p.clone());
    }
    // the same rule as creating one there
    if !target.is_group()
        && !principal.direct_user().is_some_and(|u| u.is_admin)
        && !forgust_db::settings::get(&state.db).await?.allow_personal_projects
    {
        return Err(ApiError::forbidden("personal projects are disabled on this instance"));
    }
    check_visibility(state, target, p.visibility).await?;
    let new_full = format!("{}/{}", target.full_path, p.path);
    let mut tx = state.db.begin().await?;
    if forgust_db::routes::path_taken(&mut *tx, &new_full).await? {
        return Err(ApiError::conflict(format!("`{new_full}` is already taken")));
    }
    let mut affected = forgust_db::members::affected_by_namespace(&mut *tx, p.namespace_id).await?;
    affected.extend(forgust_db::members::affected_by_namespace(&mut *tx, target.id).await?);
    affected.extend(forgust_db::members::project_member_ids(&mut *tx, p.id).await?);
    affected.sort_unstable();
    affected.dedup();
    forgust_db::projects::set_namespace(&mut *tx, p.id, target.id).await?;
    forgust_db::routes::move_prefix(&mut tx, &p.full_path, &new_full).await?;
    forgust_db::members::refresh_authorizations(&mut tx, &affected).await?;
    tx.commit().await?;
    audit(state, principal, "project_transferred", Some(("project", p.id)), json!({ "from": p.full_path, "to": new_full }), None).await;
    forgust_db::projects::find(&state.db, p.id).await?.ok_or(ApiError::NotFound)
}

pub async fn delete(state: &AppState, principal: &Principal, p: &Project) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Own, p).await?;
    let mut tx = state.db.begin().await?;
    forgust_db::routes::delete_for(&mut tx, "project", p.id).await?;
    forgust_db::projects::delete(&mut *tx, p.id).await?;
    tx.commit().await?;
    match state.storage.delete_repo(p.id).await {
        Ok(()) | Err(forgust_storage::StorageError::RepoNotFound(_)) => {}
        Err(e) => tracing::error!(project = p.id, error = %e, "repo files not removed"),
    }
    state.events.publish(Event::ProjectDeleted { project_id: p.id });
    audit(state, principal, "project_deleted", Some(("project", p.id)), json!({ "path": p.full_path }), None).await;
    Ok(())
}
