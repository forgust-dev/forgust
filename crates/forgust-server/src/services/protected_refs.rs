//! Protected tags (rules on which roles may create matching tags), for both
//! APIs: validation and audit live here.

use forgust_db::projects::Project;
use serde_json::json;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// A name or glob a rule may use: not empty, no `..`, not an option, a valid glob.
pub fn check_pattern(name: &str) -> ApiResult<&str> {
    let n = name.trim();
    if n.is_empty() || n.len() > 255 || n.starts_with('-') || n.contains("..") || globset::Glob::new(n).is_err() {
        return Err(ApiError::bad("invalid name or pattern"));
    }
    Ok(n)
}

/// `create_access_level`: 0 (no one), 30 (Developers) or 40 (Maintainers).
pub async fn protect_tag(state: &AppState, principal: &Principal, p: &Project, name: &str, create: i16) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Admin, p).await?;
    let name = check_pattern(name)?;
    if ![0, 30, 40].contains(&create) {
        return Err(ApiError::bad("the level must be no one, developer or maintainer"));
    }
    let previous = previous_tag(state, p, |t| t.name == name).await?;
    forgust_db::protected_branches::upsert_tag(&state.db, p.id, name, create).await?;
    let details = json!({ "name": name, "create": create, "previous_create": previous.map(|t| t.create_access_level) });
    super::audit(state, principal, "tag_protected", Some(("project", p.id)), details, None).await;
    Ok(())
}

pub async fn unprotect_tag(state: &AppState, principal: &Principal, p: &Project, id: i64) -> ApiResult<()> {
    require_project(state, principal, ProjectAction::Admin, p).await?;
    let previous = previous_tag(state, p, |t| t.id == id).await?;
    if !forgust_db::protected_branches::delete_tag(&state.db, p.id, id).await? {
        return Err(ApiError::NotFound);
    }
    let details = json!({
        "id": id,
        "name": previous.as_ref().map(|t| &t.name),
        "previous_create": previous.as_ref().map(|t| t.create_access_level),
    });
    super::audit(state, principal, "tag_unprotected", Some(("project", p.id)), details, None).await;
    Ok(())
}

async fn previous_tag(
    state: &AppState,
    p: &Project,
    which: impl Fn(&forgust_db::protected_branches::ProtectedTag) -> bool,
) -> ApiResult<Option<forgust_db::protected_branches::ProtectedTag>> {
    Ok(forgust_db::protected_branches::tags(&state.db, p.id).await?.into_iter().find(which))
}
