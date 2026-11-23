//! The bot's labels on merge requests (ADR-0026), by the `[labels]` rules of
//! `.forgust/bot.toml` on the target branch: the paths it changes, its size
//! (`size/XS` … `size/XL`) and the conventional-commit type of its title.
//!
//! Looked at when a merge request is opened, when its source branch moves,
//! and when its title or target changes. The bot remembers what the rules
//! asked for: a label is put on when a rule starts to ask for it, and taken
//! off when none does any more. So `size/S` gives way to `size/M`, and a
//! label somebody took off by hand is not put back at the next push.

use std::collections::BTreeSet;

use forgust_db::merge_requests::MergeRequest;
use forgust_db::projects::Project;
use serde_json::json;

use super::config::LabelRules;
use crate::auth::Principal;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// The type of a conventional-commit title, lowercase: `feat` of
/// `Draft: feat(api)!: add a thing`.
pub fn title_type(title: &str) -> Option<String> {
    let mut t = title.trim_start();
    for prefix in ["draft:", "[draft]", "(draft)", "wip:", "[wip]"] {
        if t.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix)) {
            t = t[prefix.len()..].trim_start();
            break;
        }
    }
    let (head, rest) = t.split_once(':')?;
    // `feat:x` and `http://…` are no types
    if !rest.starts_with(' ') {
        return None;
    }
    let head = head.strip_suffix('!').unwrap_or(head);
    let kind = match head.split_once('(') {
        Some((kind, scope)) if scope.ends_with(')') && !scope[..scope.len() - 1].contains(['(', ')']) => kind,
        Some(_) => return None,
        None => head,
    };
    (!kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphabetic())).then(|| kind.to_ascii_lowercase())
}

/// The labels the rules ask for.
pub fn wanted(rules: &LabelRules, title: &str, paths: &[String], lines: u32) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (label, patterns) in &rules.paths {
        let globs: Vec<_> = patterns.iter().flat_map(|p| crate::services::codeowners::matchers(p)).collect();
        if paths.iter().any(|path| globs.iter().any(|g| g.is_match(path))) {
            out.insert(label.clone());
        }
    }
    if let Some(sizes) = &rules.size {
        out.insert(sizes.label(lines).to_string());
    }
    if let Some(label) = title_type(title).and_then(|t| rules.title.get(&t)) {
        out.insert(label.clone());
    }
    out
}

/// A merge request was opened or changed: label it. It is opened and
/// updated whatever happens here: a failure is logged.
pub async fn on_merge_request(state: &AppState, p: &Project, mr: &MergeRequest) {
    if let Err(e) = sync(state, p, mr.iid).await {
        tracing::warn!(project_id = p.id, iid = mr.iid, error = ?e, "bot: merge request not labeled");
    }
}

async fn sync(state: &AppState, p: &Project, iid: i64) -> ApiResult<()> {
    if !super::is_on(state, p.id).await? {
        return Ok(());
    }
    // as it is now: its title or target may just have changed
    let Some(m) = forgust_db::merge_requests::find(&state.db, p.id, iid).await?.filter(|m| m.state == "opened") else { return Ok(()) };
    let rules = match super::rules(state, p, Some(&m.target_branch)).await? {
        Ok(rules) => rules.labels,
        // said on the settings page, and in the reply to the next command
        Err(e) => {
            tracing::debug!(project_id = p.id, iid, error = %e, "bot: rules not usable, merge request not labeled");
            return Ok(());
        }
    };
    let before: BTreeSet<String> = forgust_db::bot::auto_labels(&state.db, m.id).await?.into_iter().collect();
    if rules.is_empty() && before.is_empty() {
        return Ok(());
    }
    let Some(diff) = forgust_db::merge_requests::latest_diff(&state.db, m.id).await? else { return Ok(()) };
    let paths = match rules.paths.is_empty() {
        true => Vec::new(),
        false => state.storage.changed_paths(p.id, None, &diff.base_sha, &diff.head_sha).await?,
    };
    let lines = (diff.additions.max(0) as u32).saturating_add(diff.deletions.max(0) as u32);
    let wanted = wanted(&rules, &m.title, &paths, lines);
    if wanted == before {
        return Ok(());
    }
    let current: BTreeSet<String> = forgust_db::issues::labels_of_mrs(&state.db, &[m.id]).await?.into_iter().map(|(_, l)| l.name).collect();
    let added: Vec<String> = wanted.difference(&before).filter(|n| !current.contains(*n)).cloned().collect();
    let removed: Vec<String> = before.difference(&wanted).filter(|n| current.contains(*n)).cloned().collect();
    if !added.is_empty() || !removed.is_empty() {
        let bot = super::member(state, p).await?;
        let bot_id = bot.id;
        let bot = Principal::member(bot);
        let names: Vec<String> = current.iter().filter(|n| !removed.contains(n)).chain(&added).cloned().collect();
        match crate::services::issues::ensure_labels(state, &bot, p, &added).await {
            Ok(()) => {}
            // an archived project, a bot made a Reporter: nothing to label with
            Err(ApiError::Forbidden(_)) => return Ok(()),
            Err(e) => return Err(e),
        }
        crate::services::mr::set_labels(state, &bot, p, &m, &names).await?;
        let details = json!({ "merge_request": m.iid, "added": added, "removed": removed });
        crate::services::write_audit(state, "bot", Some(bot_id), "bot_labels", Some(("project", p.id)), details, None).await;
    }
    let mut conn = state.db.acquire().await?;
    forgust_db::bot::set_auto_labels(&mut conn, m.id, &wanted.into_iter().collect::<Vec<_>>()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::config;
    use super::*;

    #[test]
    fn the_type_of_a_title() {
        for (title, kind) in [
            ("feat: add a thing", Some("feat")),
            ("Fix(api)!: no more crash", Some("fix")),
            ("Draft: chore(deps): bump", Some("chore")),
            ("[WIP] docs: typo", Some("docs")),
            ("feat:no space", None),
            ("see http://example.com: it fails", None),
            ("Add a thing", None),
            ("fix(a(b)): x", None),
            ("fix (api): x", None),
            (": x", None),
        ] {
            assert_eq!(title_type(title).as_deref(), kind, "{title}");
        }
    }

    #[test]
    fn labels_by_path_size_and_title() {
        let rules = config::parse(
            "[labels.paths]\ndocs = [\"*.md\", \"/docs/\"]\n\"area/web\" = [\"/web/\"]\n[labels.size]\n[labels.title]\nfix = \"bug\"\n",
        )
        .unwrap()
        .labels;
        let paths = |ps: &[&str]| ps.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let got = |title: &str, ps: &[&str], lines| wanted(&rules, title, &paths(ps), lines).into_iter().collect::<Vec<_>>();
        assert_eq!(got("fix: crash", &["src/a.rs", "web/src/x/README.md"], 12), ["area/web", "bug", "docs", "size/S"]);
        assert_eq!(got("Tidy up", &["src/docs/a.rs"], 3), ["size/XS"]);
        assert_eq!(got("feat: x", &["docs/a/b.txt"], 5000), ["docs", "size/XL"]);
        assert!(wanted(&LabelRules::default(), "fix: x", &paths(&["a.md"]), 1).is_empty());
    }
}
