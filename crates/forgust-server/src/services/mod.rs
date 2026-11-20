//! Business rules that span several tables. Handlers call these; these call the
//! per-domain query modules in `forgust-db`.

pub mod accounts;
pub mod ai_review;
pub mod audit_events;
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
    if cfg!(debug_assertions)
        && let Some(e) = audit_events::mismatch(action, target.map(|t| t.0), &details)
    {
        panic!("{e}");
    }
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

/// Append what the server did on its own (a session running out) to the
/// audit log, as principal `system`.
pub async fn audit_system(state: &AppState, action: &str, target: Option<(&str, i64)>, details: Value, ip: Option<&str>) {
    if cfg!(debug_assertions)
        && let Some(e) = audit_events::mismatch(action, target.map(|t| t.0), &details)
    {
        panic!("{e}");
    }
    let ev = forgust_db::audit::NewEvent {
        principal_kind: "system",
        principal_id: None,
        action,
        target_type: target.map(|t| t.0),
        target_id: target.map(|t| t.1),
        details,
        ip,
    };
    if let Err(e) = forgust_db::audit::insert(&state.db, ev).await {
        tracing::warn!(error = %e, action, "failed to write audit event");
    }
}

/// A clone, fetch or archive download of a private or internal repository,
/// when `audit.repository_reads` is on. CI jobs' fetches are left out (one
/// per job), as are public repositories.
pub async fn audit_repository_read(
    state: &AppState,
    principal: &Principal,
    project: &forgust_db::projects::Project,
    via: &str,
    ip: Option<&str>,
) {
    if !state.cfg.audit.repository_reads
        || project.visibility == forgust_db::visibility::PUBLIC
        || matches!(principal, Principal::Job { .. } | Principal::Anonymous)
    {
        return;
    }
    audit_throttled(state, principal, "repository_read", Some(("project", project.id)), serde_json::json!({ "via": via }), ip).await;
}

/// An event that can come in floods (a refusal a client retries in a loop, a
/// fetch git makes in several requests): at most one row a minute per
/// principal, action, target and `details` (route, method, transport...), the
/// count of those held back (`repeated`) written with the next row or when
/// the minute ends ([`flush_held_audit`]).
pub async fn audit_throttled(
    state: &AppState,
    principal: &Principal,
    action: &str,
    target: Option<(&str, i64)>,
    details: Value,
    ip: Option<&str>,
) {
    let key = format!(
        "{}:{}|{action}|{}|{details}",
        principal.kind(),
        principal.id().unwrap_or_default(),
        target.map(|(t, id)| format!("{t}:{id}")).unwrap_or_default(),
    );
    let mut held = details.clone();
    if let (Some(admin), Value::Object(m)) = (principal.impersonator(), &mut held) {
        m.insert("impersonated_by".into(), Value::from(admin));
    }
    let event = crate::state::HeldEvent {
        principal_kind: principal.kind(),
        principal_id: principal.id(),
        action: action.to_string(),
        target: target.map(|(t, id)| (t.to_string(), id)),
        details: held,
        ip: ip.map(str::to_string),
    };
    let Some(repeated) = state.audit_throttle.admit(&key, event) else { return };
    audit(state, principal, action, target, with_repeated(details, repeated), ip).await;
}

fn with_repeated(details: Value, repeated: u32) -> Value {
    match details {
        Value::Object(mut m) if repeated > 0 => {
            m.insert("repeated".into(), Value::from(repeated));
            Value::Object(m)
        }
        d => d,
    }
}

/// Write what throttled bursts held back once their minute is over: a copy
/// of each burst's first row with `repeated`, so a burst that stops is
/// counted too. Runs every 30 seconds in every server process.
pub async fn flush_held_audit(state: &AppState) {
    for (e, repeated) in state.audit_throttle.overdue() {
        let ev = forgust_db::audit::NewEvent {
            principal_kind: e.principal_kind,
            principal_id: e.principal_id,
            action: &e.action,
            target_type: e.target.as_ref().map(|t| t.0.as_str()),
            target_id: e.target.as_ref().map(|t| t.1),
            details: with_repeated(e.details, repeated),
            ip: e.ip.as_deref(),
        };
        if let Err(err) = forgust_db::audit::insert(&state.db, ev).await {
            tracing::warn!(error = %err, action = e.action, "failed to write audit event");
        }
    }
}
