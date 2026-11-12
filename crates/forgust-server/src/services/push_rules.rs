//! Push rules and Git LFS locks, checked on every push after the protected
//! branch rules: over git (the pre-receive hook passes its quarantine
//! directory, so the pushed objects are visible) and for commits Forgust makes
//! for someone (browser edits, suggestions, rebases).

use std::path::Path;

use forgust_db::push_rules::PushRules;

use crate::auth::Principal;
use crate::error::{ApiError, ApiResult};
use forgust_storage::ZERO_OID;

use crate::hooks::HookUpdate;
use crate::state::AppState;

/// Commits one push may add and still be checked one by one.
const MAX_COMMITS: usize = 10_000;
/// Files one push may add and still be checked.
const MAX_BLOBS: usize = 500_000;

/// Paths that hold secrets far more often than not (GitLab's list, in short).
fn secret_paths() -> &'static regex::RegexSet {
    static RE: std::sync::OnceLock<regex::RegexSet> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::RegexSet::new([
            r"(^|/)id_(rsa|dsa|ecdsa|ed25519)$",
            r"(^|/)(ssh|config)/(personal|server)_(rsa|dsa|ed\d+|ecdsa)$",
            r"\.(p12|pfx|keychain|kdbx?|agilekeychain|tblk)$",
            r"(^|/)\.?(bash|zsh|sh|mysql|psql|irb|python|node_repl)_history$",
            r"(^|/)\.?(git-credentials|netrc|pgpass|htpasswd|s3cfg)$",
            r"(^|/)\.?aws/credentials$",
            r"(^|/)\.?docker/config\.json$",
            r"(^|/)(secret_token|carrierwave|omniauth)\.rb$",
            r"(^|/)(key(store|ring))$",
        ])
        .expect("valid patterns")
    })
}

/// Check a rule's regex when it is saved.
pub fn validate(r: &PushRules) -> ApiResult<()> {
    for (name, re) in [
        ("commit message", &r.commit_message_regex),
        ("rejected commit message", &r.commit_message_negative_regex),
        ("author email", &r.author_email_regex),
        ("branch name", &r.branch_name_regex),
    ] {
        if !re.is_empty() {
            regex::RegexBuilder::new(re)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| ApiError::bad(format!("the {name} pattern is not a valid regular expression: {e}")))?;
        }
    }
    if r.max_file_size_mb < 0 {
        return Err(ApiError::bad("the file size limit cannot be negative"));
    }
    Ok(())
}

fn compile(re: &str) -> Option<regex::Regex> {
    (!re.is_empty()).then(|| regex::RegexBuilder::new(re).size_limit(1 << 20).build().ok()).flatten()
}

/// Header value of a raw commit (`author`, ...) and its message.
fn header<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    raw.split("\n\n").next()?.lines().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' ')))
}

fn email_of(ident: &str) -> &str {
    match (ident.find('<'), ident.find('>')) {
        (Some(a), Some(b)) if a < b => &ident[a + 1..b],
        _ => "",
    }
}

/// Why the push is refused, or None.
pub async fn check(
    state: &AppState,
    principal: &Principal,
    project_id: i64,
    quarantine: Option<&Path>,
    updates: &[HookUpdate],
) -> ApiResult<Option<String>> {
    if let Some(msg) = check_locks(state, principal, project_id, quarantine, updates).await? {
        return Ok(Some(msg));
    }
    let rules = forgust_db::push_rules::get(&state.db, project_id).await?;
    if rules.is_empty() {
        return Ok(None);
    }
    if let Some(re) = compile(&rules.branch_name_regex) {
        for u in updates.iter().filter(|u| u.old == ZERO_OID && u.new != ZERO_OID) {
            if let Some(b) = u.name.strip_prefix("refs/heads/")
                && !re.is_match(b)
            {
                return Ok(Some(format!("Branch name {b} does not follow the pattern '{}'.", rules.branch_name_regex)));
            }
        }
    }
    let tips: Vec<String> = updates.iter().filter(|u| u.new != ZERO_OID).map(|u| u.new.clone()).collect();
    if tips.is_empty() {
        return Ok(None);
    }
    let (message, negative, author) =
        (compile(&rules.commit_message_regex), compile(&rules.commit_message_negative_regex), compile(&rules.author_email_regex));
    if message.is_some() || negative.is_some() || author.is_some() || rules.reject_unsigned_commits {
        let shas = state.storage.new_commits(project_id, quarantine, &tips, MAX_COMMITS + 1).await?;
        if shas.len() > MAX_COMMITS {
            return Ok(Some(format!(
                "This push adds more than {MAX_COMMITS} commits: push them in smaller parts so the push rules can check them."
            )));
        }
        for chunk in shas.chunks(500) {
            for (sha, raw) in state.storage.raw_commits(project_id, quarantine, chunk).await? {
                let short = &sha[..sha.len().min(8)];
                let text = String::from_utf8_lossy(&raw);
                let msg = text.split_once("\n\n").map(|(_, m)| m).unwrap_or("");
                if let Some(re) = &message
                    && !re.is_match(msg)
                {
                    return Ok(Some(format!("Commit {short}: its message does not follow the pattern '{}'.", rules.commit_message_regex)));
                }
                if let Some(re) = &negative
                    && re.is_match(msg)
                {
                    return Ok(Some(format!(
                        "Commit {short}: its message matches the refused pattern '{}'.",
                        rules.commit_message_negative_regex
                    )));
                }
                if let Some(re) = &author {
                    let email = header(&text, "author").map(email_of).unwrap_or("");
                    if !re.is_match(email) {
                        return Ok(Some(format!(
                            "Commit {short}: author email {email} does not follow the pattern '{}'.",
                            rules.author_email_regex
                        )));
                    }
                }
                if rules.reject_unsigned_commits {
                    match crate::services::signatures::verify(state, &raw).await {
                        Some(v) if v.verified() => {}
                        Some(v) => {
                            return Ok(Some(format!(
                                "Commit {short}: its signature is not verified ({}). Only verified signed commits can be pushed: add your key to your profile and commit with your confirmed email.",
                                v.verification_status.replace('_', " ")
                            )));
                        }
                        None => {
                            return Ok(Some(format!("Commit {short} is not signed. Only signed commits can be pushed to this project.")));
                        }
                    }
                }
            }
        }
    }
    if rules.max_file_size_mb > 0 || rules.prevent_secrets {
        let limit = (rules.max_file_size_mb as u64) << 20;
        for (path, size) in state.storage.new_blobs(project_id, quarantine, &tips, MAX_BLOBS).await? {
            if limit > 0 && size > limit {
                return Ok(Some(format!(
                    "File {path} is {:.1} MB; this project accepts files up to {} MB (use Git LFS for large files).",
                    size as f64 / (1 << 20) as f64,
                    rules.max_file_size_mb
                )));
            }
            if rules.prevent_secrets && secret_paths().is_match(&path) {
                return Ok(Some(format!("File {path} looks like a secret (a key or credentials) and cannot be pushed to this project.")));
            }
        }
    }
    Ok(None)
}

/// Files locked with Git LFS by someone else cannot change on the default
/// branch (as in GitLab).
async fn check_locks(
    state: &AppState,
    principal: &Principal,
    project_id: i64,
    quarantine: Option<&Path>,
    updates: &[HookUpdate],
) -> ApiResult<Option<String>> {
    let Some(default) = state.storage.default_branch(project_id).await? else { return Ok(None) };
    let target = format!("refs/heads/{default}");
    let Some(u) = updates.iter().find(|u| u.name == target && u.old != ZERO_OID && u.new != ZERO_OID) else { return Ok(None) };
    if forgust_db::lfs_locks::list(&state.db, project_id, None, None, 0, 1).await?.is_empty() {
        return Ok(None);
    }
    let paths = state.storage.changed_paths(project_id, quarantine, &u.old, &u.new).await?;
    let me = match principal {
        Principal::User { user, .. } | Principal::Job { user, .. } => user.id,
        _ => 0,
    };
    let held = forgust_db::lfs_locks::held_by_others(&state.db, project_id, &paths, me).await?;
    if held.is_empty() {
        return Ok(None);
    }
    let list = held.iter().take(10).map(|(p, who)| format!("{p} (locked by @{who})")).collect::<Vec<_>>().join(", ");
    Ok(Some(format!("These files are locked and cannot change on {default}: {list}.")))
}
