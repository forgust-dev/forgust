//! The audit event catalogue: every action the audit log holds, what it is
//! about (`target_type`) and the fields of its `details`. `forgust
//! audit-events` prints it for the docs, the admin API serves it, and a test
//! checks it against the code, so an event can't be added without it.
//!
//! [`VERSION`] goes up when an event is renamed or removed, or a field
//! changes meaning or goes away; new events and new fields keep it.
//!
//! Conventions:
//! - a change records what it replaced: `previous_<field>` for one value,
//!   `previous` (an object, null when there was none) for several;
//! - `impersonated_by` (an administrator's user id) is on every event done
//!   while an administrator is signed in as the user;
//! - `repeated` is on throttled events (at most one row a minute per
//!   principal, action and target): how many were held back since the last
//!   row;
//! - people are referenced by id, never by name or address, so that erasing
//!   a person leaves nothing of them in `details` (the API looks names up
//!   when the log is read: `target_name`, and `user_name` for `user_id`, the
//!   other person an event is about).

use serde::Serialize;

/// The catalogue's version, see the module docs.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
pub struct EventType {
    pub name: &'static str,
    /// Area, for grouping in the docs.
    pub area: &'static str,
    /// `target_type`s it is written with (`project`, `namespace`, `user`);
    /// empty: none (instance-wide).
    pub targets: &'static [&'static str],
    /// Keys of `details` (besides the conventional ones); `["*"]`: the
    /// whole of some settings object, whatever its keys.
    pub fields: &'static [&'static str],
    pub description: &'static str,
}

const fn e(
    name: &'static str,
    area: &'static str,
    targets: &'static [&'static str],
    fields: &'static [&'static str],
    description: &'static str,
) -> EventType {
    EventType { name, area, targets, fields, description }
}

const SIGN_IN: &str = "Sign-in and sessions";
const ACCOUNT: &str = "Accounts and credentials";
const ADMIN: &str = "Administration";
const SPACES: &str = "Groups and projects";
const ACCESS: &str = "Access and protections";
const CODE: &str = "Repositories and code";
const BUILTIN: &str = "CI, registry and built-in services";
const REFUSED: &str = "Refusals";

const U: &[&str] = &["user"];
const P: &[&str] = &["project"];
const N: &[&str] = &["namespace"];
const PN: &[&str] = &["project", "namespace"];
const NONE: &[&str] = &[];

pub const EVENTS: &[EventType] = &[
    // sign-in and sessions
    e(
        "login",
        SIGN_IN,
        U,
        &["method"],
        "Signed in. `method`: password, totp, recovery_code, security_key, passkey, oidc, registration or setup.",
    ),
    e(
        "login_failed",
        SIGN_IN,
        U,
        &["reason", "method", "credential"],
        "A sign-in to this account was refused. `reason`: password, second_factor, passkey (`credential`: its id), blocked or bot; `method`: the step that failed.",
    ),
    e("logout", SIGN_IN, U, &[], "Signed out of a browser session."),
    e(
        "session_expired",
        SIGN_IN,
        U,
        &["session_id", "impersonated_by"],
        "A browser session ran out (principal `system`); the row's IP is the session's.",
    ),
    e("sessions_revoked", SIGN_IN, U, &[], "An administrator signed the user out everywhere."),
    e("recovery_code_used", SIGN_IN, U, &["remaining"], "A recovery code stood in for the second factor."),
    e(
        "webauthn_clone_suspected",
        SIGN_IN,
        U,
        &["credential"],
        "A security key's signature counter went backwards: it may have been cloned.",
    ),
    e("impersonation_started", SIGN_IN, U, &[], "An administrator signed in as the user."),
    e("impersonation_stopped", SIGN_IN, U, &[], "The administrator went back to their own account."),
    e(
        "impersonated_request",
        SIGN_IN,
        NONE,
        &["method", "path"],
        "A change made while an administrator was signed in as someone (`path`: the route).",
    ),
    e(
        "oauth_authorized",
        SIGN_IN,
        U,
        &["client", "client_id", "scopes"],
        "The user let an OAuth application (an MCP client) act for them.",
    ),
    e("first_admin_created", SIGN_IN, U, &[], "The first administrator was created at setup."),
    // accounts and credentials
    e("password_changed", ACCOUNT, U, &[], "The user changed their password."),
    e("password_reset_requested", ACCOUNT, U, &[], "A password reset email was asked for."),
    e("password_reset", ACCOUNT, U, &[], "The password was reset, by email link or by an administrator."),
    e(
        "email_changed",
        ACCOUNT,
        U,
        &["confirmed"],
        "The account's address changed (`confirmed`: an administrator vouched for the new one).",
    ),
    e("email_confirmed", ACCOUNT, U, &["by"], "An address was confirmed (`by`: administrator when one vouched for it)."),
    e("confirmation_resent", ACCOUNT, U, &[], "An administrator sent the confirmation email again."),
    e("profile_updated", ACCOUNT, U, &[], "The user changed their profile."),
    e(
        "profile_synced",
        ACCOUNT,
        U,
        &["provider", "name", "email"],
        "A sign-in provider updated the name or address (`name`, `email`: whether each changed).",
    ),
    e("identity_linked", ACCOUNT, U, &["provider"], "A sign-in provider identity was linked to the account."),
    e("identity_unlinked", ACCOUNT, U, &["provider"], "A sign-in provider identity was unlinked."),
    e("two_factor_enabled", ACCOUNT, U, &[], "Two-factor authentication turned on."),
    e("two_factor_disabled", ACCOUNT, U, &[], "Two-factor authentication turned off by the user."),
    e("two_factor_reset", ACCOUNT, U, &[], "An administrator turned the user's two-factor authentication off."),
    e("recovery_codes_reset", ACCOUNT, U, &[], "New recovery codes replaced the old ones."),
    e("webauthn_registered", ACCOUNT, U, &["kind", "name", "credential"], "A security key or passkey was added."),
    e("webauthn_renamed", ACCOUNT, U, &["credential", "name"], "A security key or passkey was renamed."),
    e("webauthn_removed", ACCOUNT, U, &["kind", "name", "credential"], "A security key or passkey was removed."),
    e("ssh_key_added", ACCOUNT, U, &["fingerprint", "usage_type"], "An SSH key was added."),
    e("ssh_key_removed", ACCOUNT, U, &["key_id"], "An SSH key was removed."),
    e("gpg_key_added", ACCOUNT, U, &["fingerprint"], "A GPG key was added."),
    e("gpg_key_removed", ACCOUNT, U, &["key_id"], "A GPG key was removed."),
    e("token_created", ACCOUNT, U, &["name", "scopes"], "A personal access token was created."),
    e("token_revoked", ACCOUNT, U, &["token_id"], "A personal access token was revoked."),
    e("tokens_revoked", ACCOUNT, U, &["count"], "An administrator revoked all of the user's personal access tokens."),
    // administration
    e("user_created", ADMIN, U, &["admin"], "An administrator created an account."),
    e("user_deleted", ADMIN, U, &["projects_deleted"], "An administrator deleted an account (who it was is not kept: erasure)."),
    e("user_blocked", ADMIN, U, &["previous_state"], "An administrator blocked the account."),
    e("user_unblocked", ADMIN, U, &["previous_state"], "An administrator unblocked the account."),
    e("user_made_external", ADMIN, U, &[], "The user became external (sees internal projects no more)."),
    e("user_made_internal", ADMIN, U, &[], "The user is internal again."),
    e("admin_granted", ADMIN, U, &["provider"], "The user became an administrator (`provider`: when a sign-in provider's groups said so)."),
    e("admin_revoked", ADMIN, U, &["provider"], "The user is no longer an administrator."),
    e("admin_bypass", ADMIN, PN, &["action"], "An administrator did something their own role would not allow (principal `admin`)."),
    e("instance_settings_updated", ADMIN, NONE, &["*"], "Instance settings changed; `details` holds every setting after the change."),
    e("banner_set", ADMIN, NONE, &["text", "until"], "The instance banner changed."),
    e(
        "storage_quota_changed",
        ADMIN,
        &["namespace", "project"],
        &["scope", "bytes"],
        "A storage quota changed (no target, `scope: default`: the default one).",
    ),
    e("backup_started", ADMIN, NONE, &[], "An administrator started a backup."),
    e("backup_deleted", ADMIN, NONE, &["name"], "An administrator deleted a backup."),
    e("blob_scan_started", ADMIN, NONE, &[], "An administrator started a scan of stored files."),
    e("repository_maintenance", ADMIN, P, &[], "An administrator queued repository maintenance."),
    e("task_run", ADMIN, NONE, &["task"], "An administrator ran a background task now."),
    e("job_retried", ADMIN, NONE, &["job"], "An administrator retried a failed background job."),
    e("job_dropped", ADMIN, NONE, &["job"], "An administrator dropped a failed background job."),
    e("test_email_sent", ADMIN, NONE, &[], "An administrator sent a test email."),
    e(
        "ai_review_instance_updated",
        ADMIN,
        NONE,
        &["*"],
        "AI review settings changed; `details` holds them, with `api_key` and `secret_options` as kept, removed or changed (the names set), never their values.",
    ),
    e("renovate_instance_updated", ADMIN, NONE, &["*"], "Renovate settings changed; `details` holds them (no secrets)."),
    e(
        "renovate_host_rule_added",
        ADMIN,
        P,
        &["host"],
        "A Renovate host rule (credentials for a registry) was added, for a project or (no target) the instance.",
    ),
    e("renovate_host_rule_removed", ADMIN, P, &["id"], "A Renovate host rule was removed, from a project or (no target) the instance."),
    e("security_scanning_settings", ADMIN, NONE, &[], "Security scanning settings changed."),
    // groups and projects
    e("group_created", SPACES, N, &["path"], "A group was created."),
    e("group_updated", SPACES, N, &["visibility", "previous_visibility"], "A group's name, description or visibility changed."),
    e("group_path_changed", SPACES, N, &["from", "to"], "A group was renamed (its path)."),
    e("group_transferred", SPACES, N, &["from", "to"], "A group moved under another parent."),
    e("group_deleted", SPACES, N, &["path"], "A group was deleted."),
    e(
        "org_policy_updated",
        SPACES,
        N,
        &["max_visibility", "require_two_factor", "project_creation_level", "subgroup_creation_level", "previous"],
        "An organization's policy changed.",
    ),
    e("project_created", SPACES, P, &["path"], "A project was created."),
    e(
        "project_updated",
        SPACES,
        P,
        &["visibility", "previous_visibility"],
        "A project's name, description, default branch or visibility changed.",
    ),
    e("project_path_changed", SPACES, P, &["from", "to"], "A project was renamed (its path)."),
    e("project_transferred", SPACES, P, &["from", "to"], "A project moved to another namespace."),
    e("project_archived", SPACES, P, &[], "A project was archived."),
    e("project_unarchived", SPACES, P, &[], "A project was unarchived."),
    e("project_deleted", SPACES, P, &["path"], "A project was deleted."),
    e("import_started", SPACES, N, &["import", "source", "url", "projects"], "An import from GitLab or GitHub started."),
    e("pages_removed", SPACES, P, &[], "A project's Pages site was removed."),
    // access and protections
    e(
        "member_set",
        ACCESS,
        PN,
        &["user_id", "level", "previous_level"],
        "A member was added or their role changed (`previous_level` null: new member).",
    ),
    e("member_removed", ACCESS, PN, &["user_id", "previous_level"], "A member was removed."),
    e("shared_with_group", ACCESS, PN, &["group", "group_id", "max", "previous_max"], "Shared with a group, or the share's role changed."),
    e("unshared_with_group", ACCESS, PN, &["group", "group_id", "previous_max"], "A share with a group ended."),
    e("access_token_created", ACCESS, PN, &["bot", "level"], "A project or group access token (and its bot) was created."),
    e("access_token_revoked", ACCESS, PN, &["bot"], "A project or group access token was revoked."),
    e("deploy_token_created", ACCESS, PN, &["name", "scopes"], "A deploy token was created."),
    e("deploy_token_revoked", ACCESS, PN, &["token_id"], "A deploy token was revoked."),
    e("deploy_key_added", ACCESS, P, &["fingerprint", "can_push"], "A deploy key was added."),
    e("deploy_key_removed", ACCESS, P, &["key_id"], "A deploy key was removed."),
    e(
        "branch_protected",
        ACCESS,
        P,
        &["name", "push", "merge", "force_push", "deploy_keys", "code_owner_approval", "previous"],
        "A branch rule was added or changed (`previous`: the rule it replaced). Levels as stored: 0 no one, 30 developer, 40 maintainer.",
    ),
    e("branch_unprotected", ACCESS, P, &["id", "name", "previous"], "A branch rule was removed."),
    e("tag_protected", ACCESS, P, &["name", "create", "previous_create"], "A tag rule was added or changed."),
    e("tag_unprotected", ACCESS, P, &["id", "name", "previous_create"], "A tag rule was removed."),
    e("environment_protected", ACCESS, P, &["name", "level", "previous_level"], "An environment rule was added or changed."),
    e("environment_unprotected", ACCESS, P, &["id", "name", "previous_level"], "An environment rule was removed."),
    e(
        "approval_rule_set",
        ACCESS,
        P,
        &["name", "required", "min_access_level", "previous"],
        "A merge request approval rule was added or changed.",
    ),
    e("approval_rule_removed", ACCESS, P, &["id", "name", "previous"], "An approval rule was removed."),
    e("push_rules_changed", ACCESS, P, &["*"], "Push rules changed; `details` holds them."),
    e("webhook_added", ACCESS, PN, &["url", "events", "format"], "A webhook was added."),
    // repositories and code
    e(
        "push_rejected",
        CODE,
        P,
        &["reason", "refs", "via"],
        "A push was refused: by a branch rule or push rule (`refs`), or for the pusher's role (`via`: http or ssh; throttled).",
    ),
    e(
        "repository_read",
        CODE,
        P,
        &["via"],
        "A clone, fetch or archive of a private or internal repository (`via`: http, ssh or archive). Only with `audit.repository_reads`; throttled.",
    ),
    e("branch_created", CODE, P, &["branch", "sha"], "A branch was created from the web or the API."),
    e("branch_deleted", CODE, P, &["branch"], "A branch was deleted from the web or the API."),
    e("tag_created", CODE, P, &["tag", "commit"], "A tag was created from the web or the API."),
    e("tag_deleted", CODE, P, &["tag"], "A tag was deleted from the web or the API."),
    e("web_commit", CODE, P, &["branch", "sha", "files"], "A commit made in the browser or through the API."),
    e("suggestion_applied", CODE, P, &["iid", "note", "sha"], "A review suggestion was applied as a commit."),
    e("lfs_lock_removed", CODE, P, &["path", "user_id"], "Someone else's LFS lock was removed (`user_id`: its owner)."),
    e("merge_request_opened", CODE, P, &["iid"], "A merge request was opened."),
    e("merge_request_merged", CODE, P, &["iid", "method", "sha"], "A merge request was merged."),
    e("merge_train_added", CODE, P, &["iid"], "A merge request joined the merge train."),
    e("milestone_created", CODE, P, &["title"], "A milestone was created."),
    e("milestone_deleted", CODE, P, &["title"], "A milestone was deleted."),
    e("release_created", CODE, P, &["tag"], "A release was created."),
    e("release_deleted", CODE, P, &["tag"], "A release was deleted."),
    // CI, registry and built-in services
    e("environment_stopped", BUILTIN, P, &["environment"], "An environment was stopped."),
    e("registry_tag_deleted", BUILTIN, P, &["repository", "tag"], "A container image tag was deleted."),
    e("registry_repository_deleted", BUILTIN, P, &["repository"], "A container image repository was deleted."),
    e("registry_cleanup_policy_changed", BUILTIN, P, &["*"], "The registry cleanup policy changed; `details` holds it."),
    e("renovate_enabled", BUILTIN, P, &[], "Built-in Renovate turned on for the project."),
    e("renovate_disabled", BUILTIN, P, &[], "Built-in Renovate turned off for the project."),
    e("renovate_group_default", BUILTIN, N, &["enabled", "projects"], "A group turned Renovate on or off for its projects."),
    e("renovate_run", BUILTIN, P, &["pipeline_iid"], "A Renovate run was started by hand."),
    e("security_scanning_enabled", BUILTIN, P, &["scanners"], "Built-in security scanning turned on for the project."),
    e("security_scanning_disabled", BUILTIN, P, &["scanners"], "Built-in security scanning turned off for the project."),
    e(
        "security_scanning_group_default",
        BUILTIN,
        N,
        &["enabled", "projects"],
        "A group turned security scanning on or off for its projects.",
    ),
    e("security_finding_dismissed", BUILTIN, P, &["finding", "reason"], "A security finding was dismissed."),
    e("security_finding_reopened", BUILTIN, P, &["finding"], "A dismissed security finding was reopened."),
    e("ai_review_enabled", BUILTIN, P, &[], "AI review turned on for the project."),
    e("ai_review_disabled", BUILTIN, P, &[], "AI review turned off for the project."),
    e("ai_review_requested", BUILTIN, P, &["merge_request", "pipeline_id"], "An AI review of a merge request was asked for."),
    e("mcp_tool_call", BUILTIN, P, &["tool", "via", "token_id"], "An MCP client changed something through a tool."),
    // refusals
    e(
        "token_scope_refused",
        REFUSED,
        U,
        &["method", "path"],
        "A token was used for a request its scopes do not allow (`path`: the route, as `/api/v1/projects/{id}`; throttled).",
    ),
    e(
        "admin_access_refused",
        REFUSED,
        U,
        &["method", "path", "reason"],
        "The admin API was refused: `reason` not_admin, or token_scope (a token without `admin_mode`; throttled).",
    ),
];

/// In the catalogue.
pub fn known(action: &str) -> bool {
    EVENTS.iter().any(|e| e.name == action)
}

/// Keys every event may carry, see the module docs.
const CONVENTIONAL: &[&str] = &["impersonated_by", "repeated"];

/// What is wrong with an event about to be written, against its entry: not
/// catalogued, a target type it does not list, a `details` key it does not
/// list. Checked on every write in debug builds (so by the test suite).
pub fn mismatch(action: &str, target: Option<&str>, details: &serde_json::Value) -> Option<String> {
    let Some(e) = EVENTS.iter().find(|e| e.name == action) else {
        return Some(format!("`{action}` is not in the audit event catalogue"));
    };
    if let Some(t) = target
        && !e.targets.contains(&t)
    {
        return Some(format!("`{action}` is written about a `{t}`, the catalogue says {:?}", e.targets));
    }
    if e.fields == ["*"] {
        return None;
    }
    let unlisted: Vec<_> = details
        .as_object()
        .into_iter()
        .flat_map(|m| m.keys())
        .filter(|k| !e.fields.contains(&k.as_str()) && !CONVENTIONAL.contains(&k.as_str()))
        .collect();
    (!unlisted.is_empty()).then(|| format!("`{action}` details {unlisted:?} are not in the catalogue"))
}

/// The catalogue as Markdown, for the docs (`forgust audit-events`).
pub fn markdown() -> String {
    let mut out = format!("# Audit events\n\nCatalogue version {VERSION}.\n");
    let mut area = "";
    for e in EVENTS {
        if e.area != area {
            area = e.area;
            out.push_str(&format!("\n## {area}\n\n| Event | Target | Details | |\n|---|---|---|---|\n"));
        }
        let targets = if e.targets.is_empty() { "—".to_string() } else { e.targets.join(", ") };
        let fields = if e.fields == ["*"] {
            "(the settings)".to_string()
        } else {
            e.fields.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
        };
        out.push_str(&format!("| `{}` | {targets} | {fields} | {} |\n", e.name, e.description));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashSet};
    use std::path::Path;

    use super::*;

    /// Every `.rs` file under `src/`, with its path.
    fn sources() -> Vec<(String, String)> {
        fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|x| x == "rs") {
                    out.push((path.display().to_string(), std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
        out
    }

    /// The string literals in the action argument of each audit call: the
    /// third argument of `audit(..)`, `audit_throttled(..)`, `refused(..)`
    /// (the fourth) and the second of `audit_system(..)`; when it is a name
    /// (`action`), the literals of the `let` that set it just before.
    fn actions_in(src: &str) -> Vec<String> {
        let mut found = Vec::new();
        for (call, arg) in [("audit(", 2), ("audit_throttled(", 2), ("audit_system(", 1), ("refused(", 3)] {
            let mut from = 0;
            while let Some(i) = src[from..].find(call) {
                let start = from + i;
                from = start + call.len();
                let before = &src[..start];
                if before.ends_with("fn ") || before.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let Some(args) = split_args(&src[from..]) else { continue };
                let Some(a) = args.get(arg) else { continue };
                let mut lits = literals(a);
                if lits.is_empty() {
                    let name = a.trim();
                    if let Some(l) = before.rfind(&format!("let {name} =")) {
                        let end = before[l..].find(';').map_or(before.len(), |e| l + e);
                        lits = literals(&before[l..end]);
                    }
                }
                found.extend(lits);
            }
        }
        found
    }

    /// Top-level arguments of a call, from just after its `(`.
    fn split_args(s: &str) -> Option<Vec<String>> {
        let (mut depth, mut cur, mut out, mut in_str) = (0, String::new(), Vec::new(), false);
        for c in s.chars() {
            match c {
                '"' => in_str = !in_str,
                '(' | '[' | '{' if !in_str => depth += 1,
                ')' | ']' | '}' if !in_str => {
                    if depth == 0 {
                        out.push(cur);
                        return Some(out);
                    }
                    depth -= 1;
                }
                ',' if !in_str && depth == 0 => {
                    out.push(std::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            }
            cur.push(c);
        }
        None
    }

    fn literals(s: &str) -> Vec<String> {
        s.split('"')
            .skip(1)
            .step_by(2)
            .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            .map(String::from)
            .collect()
    }

    #[test]
    fn names_are_unique_and_snake_case() {
        let mut seen = HashSet::new();
        for e in EVENTS {
            assert!(seen.insert(e.name), "`{}` is in the catalogue twice", e.name);
            assert!(e.name.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "`{}`", e.name);
            assert!(!e.description.is_empty(), "`{}` has no description", e.name);
        }
    }

    #[test]
    fn every_audited_action_is_in_the_catalogue() {
        let mut missing = BTreeSet::new();
        for (path, src) in sources() {
            if path.ends_with("audit_events.rs") {
                continue;
            }
            for a in actions_in(&src) {
                if !known(&a) {
                    missing.insert(format!("{a} ({path})"));
                }
            }
        }
        assert!(missing.is_empty(), "audited but not in the catalogue: {missing:#?}");
    }

    #[test]
    fn every_catalogued_action_is_written_somewhere() {
        let all: String = sources().into_iter().filter(|(p, _)| !p.ends_with("audit_events.rs")).map(|(_, s)| s).collect();
        let stale: Vec<_> = EVENTS.iter().filter(|e| !all.contains(&format!("\"{}\"", e.name))).map(|e| e.name).collect();
        assert!(stale.is_empty(), "in the catalogue but never written: {stale:?}");
    }

    #[test]
    fn writes_are_checked_against_their_entry() {
        use serde_json::json;
        assert_eq!(mismatch("logout", Some("user"), &json!({})), None);
        assert_eq!(mismatch("logout", Some("user"), &json!({ "impersonated_by": 1 })), None, "conventional keys");
        assert!(mismatch("nope", None, &json!({})).is_some());
        assert!(mismatch("logout", Some("project"), &json!({})).is_some(), "a target it does not list");
        assert!(mismatch("logout", Some("user"), &json!({ "why": 1 })).is_some(), "a field it does not list");
        assert_eq!(mismatch("push_rules_changed", Some("project"), &json!({ "anything": true })), None, "a settings object");
    }

    #[test]
    fn markdown_lists_every_event() {
        let md = markdown();
        assert!(EVENTS.iter().all(|e| md.contains(&format!("| `{}` |", e.name))));
    }
}
