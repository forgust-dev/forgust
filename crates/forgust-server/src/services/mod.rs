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
pub mod vulnerability_dbs;
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
    let details = with_field(details, "impersonated_by", principal.impersonator());
    write_audit(state, principal.kind(), principal.id(), action, target, details, ip).await;
}

/// The one way an audit row is written: checked against the catalogue (in
/// debug builds, so by the tests), inserted, a failure logged.
pub(crate) async fn write_audit(
    state: &AppState,
    principal_kind: &str,
    principal_id: Option<i64>,
    action: &str,
    target: Option<(&str, i64)>,
    details: Value,
    ip: Option<&str>,
) {
    if cfg!(debug_assertions)
        && let Some(e) = audit_events::mismatch(action, target.map(|t| t.0), &details)
    {
        panic!("{e}");
    }
    let ev = forgust_db::audit::NewEvent {
        principal_kind,
        principal_id,
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

/// `details` with `key` set, when there is a value (details that are no
/// object are wrapped: `{"details": .., key: ..}`).
fn with_field(details: Value, key: &str, value: Option<impl Into<Value>>) -> Value {
    let Some(v) = value else { return details };
    match details {
        Value::Object(mut m) => {
            m.insert(key.into(), v.into());
            Value::Object(m)
        }
        other => serde_json::json!({ "details": other, key: v.into() }),
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

/// A push refused for who made it (their role, an archived project), over
/// HTTP or SSH; the hook's refusals of single refs are written by the hook.
pub async fn audit_push_refused(state: &AppState, principal: &Principal, project_id: i64, reason: &str, via: &str, ip: Option<&str>) {
    let details = serde_json::json!({ "reason": reason, "via": via });
    audit_throttled(state, principal, "push_rejected", Some(("project", project_id)), details, ip).await;
}

/// An event that can come in floods (a refusal a client retries in a loop, a
/// fetch made of several requests): at most one row a minute per principal,
/// action, target and `details` (route, method, transport...), the count of
/// those held back (`repeated`) written with the next row or when the minute
/// ends ([`flush_held_audit`]).
pub async fn audit_throttled(
    state: &AppState,
    principal: &Principal,
    action: &str,
    target: Option<(&str, i64)>,
    details: Value,
    ip: Option<&str>,
) {
    let details = with_field(details, "impersonated_by", principal.impersonator());
    let (kind, id) = (principal.kind(), principal.id());
    let key =
        format!("{kind}:{}|{action}|{}|{details}", id.unwrap_or_default(), target.map(|(t, id)| format!("{t}:{id}")).unwrap_or_default());
    let held = || crate::state::HeldEvent {
        principal_kind: kind,
        principal_id: id,
        action: action.to_string(),
        target: target.map(|(t, id)| (t.to_string(), id)),
        details: details.clone(),
        ip: ip.map(str::to_string),
    };
    let Some(repeated) = state.audit_throttle.admit(key, held) else { return };
    write_audit(state, kind, id, action, target, with_field(details, "repeated", (repeated > 0).then_some(repeated)), ip).await;
}

/// Write what throttled bursts held back once their minute is over: a copy
/// of each burst's first row with `repeated`, so a burst that stops is
/// counted too. Runs every 30 seconds in every server process.
pub async fn flush_held_audit(state: &AppState) {
    for (e, repeated) in state.audit_throttle.overdue() {
        let target = e.target.as_ref().map(|(t, id)| (t.as_str(), *id));
        let details = with_field(e.details, "repeated", Some(repeated));
        write_audit(state, e.principal_kind, e.principal_id, &e.action, target, details, e.ip.as_deref()).await;
    }
}
