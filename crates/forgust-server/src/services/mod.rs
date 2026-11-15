//! Business rules that span several tables. Handlers call these; these call the
//! per-domain query modules in `forgust-db`.

pub mod accounts;
pub mod auto_merge;
pub mod avatars;
pub mod awards;
pub mod blob_scan;
pub mod builtin;
pub mod chat;
pub mod codeowners;
pub mod edits;
pub mod groups;
pub mod issues;
pub mod members;
pub mod merge_train;
pub mod milestones;
pub mod mr;
pub mod notify;
pub mod paths;
pub mod people;
pub mod projects;
pub mod protected_refs;
pub mod push_rules;
pub mod quotas;
pub mod refs;
pub mod releases;
pub mod renovate;
pub mod repos;
pub mod scanning;
pub mod security;
pub mod signatures;
pub mod stack;
pub mod suggestions;
pub mod updates;
pub mod uploads;
pub mod users;
pub mod webhooks;

use serde_json::Value;

use crate::auth::Principal;
use crate::state::AppState;

/// The rules for changing a comment (issues and merge requests alike): system
/// notes never change; authors edit theirs (not to empty); authors and
/// Maintainers delete. `body`: Some to edit, None to delete.
pub(crate) async fn check_note_change(
    state: &AppState,
    principal: &Principal,
    p: &forgust_db::projects::Project,
    author_id: Option<i64>,
    system: bool,
    body: Option<&str>,
) -> crate::error::ApiResult<()> {
    use crate::error::ApiError;
    crate::auth::authz::writable(p)?;
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?;
    if system {
        return Err(ApiError::forbidden("system notes cannot be changed"));
    }
    let own = author_id == Some(user.id);
    match body {
        Some(_) if !own => Err(ApiError::forbidden("you can only edit your own comments")),
        Some(b) if b.trim().is_empty() => Err(ApiError::bad("the comment is empty")),
        Some(_) => Ok(()),
        None if own || crate::auth::authz::project_role(state, principal, p).await?.level >= forgust_db::access::MAINTAINER => Ok(()),
        None => Err(ApiError::forbidden("only the author or a Maintainer can delete a comment")),
    }
}

/// Append to the audit log; failures are logged, never surfaced.
pub async fn audit(state: &AppState, principal: &Principal, action: &str, target: Option<(&str, i64)>, details: Value, ip: Option<&str>) {
    // done by an administrator signed in as the user: say so
    let details = match (principal.impersonator(), details) {
        (Some(admin), Value::Object(mut m)) => {
            m.insert("impersonated_by".into(), Value::from(admin));
            Value::Object(m)
        }
        (Some(admin), other) => serde_json::json!({ "details": other, "impersonated_by": admin }),
        (None, d) => d,
    };
    let r = forgust_db::audit::insert(
        &state.db,
        forgust_db::audit::NewEvent {
            principal_kind: principal.kind(),
            principal_id: principal.id(),
            action,
            target_type: target.map(|t| t.0),
            target_id: target.map(|t| t.1),
            details,
            ip,
        },
    )
    .await;
    if let Err(e) = r {
        tracing::warn!(error = %e, action, "failed to write audit event");
    }
}
