//! `@user`, `#12` and `!45` references (M6): links in rendered Markdown,
//! to-dos and emails for mentions, "mentioned in" notes on the referenced
//! issue or MR, and `Closes #12` when a merge request merges.

use std::collections::HashSet;

use forgust_db::access::{GUEST, REPORTER};
use forgust_db::issues::{self as idb, NewIssueNote};
use forgust_db::merge_requests::{self as mrdb, Position};
use forgust_db::projects::Project;
use forgust_db::todos::{self, NewTodo};
use forgust_db::users::User;

use crate::render::{self, RefTargets, find_refs};
use crate::state::AppState;

/// Look up what the references in `texts` point at (one query per kind).
/// Merge request titles only for a reader who sees code (`code`): a Guest
/// reading an issue must not learn them from `!12` links.
pub async fn targets(state: &AppState, p: &Project, texts: &[&str], code: bool) -> RefTargets {
    let mut t = RefTargets { project_url: format!("/{}", p.full_path), ..Default::default() };
    let (mut users, mut issues, mut mrs) = (Vec::new(), Vec::new(), Vec::new());
    for text in texts {
        let r = find_refs(text);
        users.extend(r.users);
        issues.extend(r.issues);
        mrs.extend(r.mrs);
    }
    if !users.is_empty()
        && let Ok(list) = forgust_db::people::by_usernames(&state.db, &users).await
    {
        t.users =
            list.into_iter().map(|u| (u.username.to_ascii_lowercase(), if u.name.is_empty() { u.username } else { u.name })).collect();
    }
    if !issues.is_empty()
        && let Ok(list) = idb::titles(&state.db, p.id, &issues).await
    {
        t.issues = list.into_iter().map(|(iid, title, st)| (iid, (title, st))).collect();
    }
    if code
        && !mrs.is_empty()
        && let Ok(list) = mrdb::titles(&state.db, p.id, &mrs).await
    {
        t.mrs = list.into_iter().map(|(iid, title, st)| (iid, (title, st))).collect();
    }
    t
}

/// Markdown of an issue or MR (description, comment) → sanitized HTML with links.
pub async fn render(state: &AppState, p: &Project, text: &str, git_ref: &str, code: bool) -> String {
    let t = targets(state, p, &[text], code).await;
    render::link_refs(&render::markdown_with(text, &t.project_url, git_ref, "", state.images.as_ref()), &t)
}

/// Several texts of one page with a single lookup.
pub async fn render_many(state: &AppState, p: &Project, texts: &[&str], git_ref: &str, code: bool) -> Vec<String> {
    let t = targets(state, p, texts, code).await;
    texts
        .iter()
        .map(|text| render::link_refs(&render::markdown_with(text, &t.project_url, git_ref, "", state.images.as_ref()), &t))
        .collect()
}

/// Texts each rendered at its own ref (a release's tag), with one lookup.
pub async fn render_at(state: &AppState, p: &Project, items: &[(&str, &str)], code: bool) -> Vec<String> {
    let texts: Vec<&str> = items.iter().map(|(t, _)| *t).collect();
    let t = targets(state, p, &texts, code).await;
    items
        .iter()
        .map(|(text, git_ref)| render::link_refs(&render::markdown_with(text, &t.project_url, git_ref, "", state.images.as_ref()), &t))
        .collect()
}

/// Where something was written.
#[derive(Debug, Clone, Copy)]
pub enum Target<'a> {
    MergeRequest { id: i64, iid: i64, title: &'a str },
    Issue { id: i64, iid: i64, title: &'a str },
}

impl Target<'_> {
    /// What seeing it takes: issues are for anyone who sees the project, merge
    /// requests for those who see its code.
    fn need(&self) -> crate::auth::authz::ProjectAction {
        match self {
            Target::MergeRequest { .. } => crate::auth::authz::ProjectAction::ReadCode,
            Target::Issue { .. } => crate::auth::authz::ProjectAction::Read,
        }
    }

    fn thread(&self) -> Option<String> {
        match self {
            Target::MergeRequest { id, .. } => super::notify::mr_thread(*id),
            Target::Issue { id, .. } => super::notify::issue_thread(*id),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Target::MergeRequest { .. } => "merge_request",
            Target::Issue { .. } => "issue",
        }
    }
    fn iid(&self) -> i64 {
        match self {
            Target::MergeRequest { iid, .. } | Target::Issue { iid, .. } => *iid,
        }
    }
    fn label(&self) -> String {
        match self {
            Target::MergeRequest { iid, title, .. } => format!("merge request !{iid} \"{title}\""),
            Target::Issue { iid, title, .. } => format!("issue #{iid} \"{title}\""),
        }
    }
    fn short(&self) -> String {
        match self {
            Target::MergeRequest { iid, .. } => format!("merge request !{iid}"),
            Target::Issue { iid, .. } => format!("issue #{iid}"),
        }
    }
    fn url(&self, state: &AppState, p: &Project) -> String {
        let path = match self {
            Target::MergeRequest { .. } => "merge_requests",
            Target::Issue { .. } => "issues",
        };
        state.cfg.project_page(&p.full_path, &format!("{path}/{}", self.iid()))
    }
}

fn snippet(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 200 { format!("{}…", flat.chars().take(200).collect::<String>()) } else { flat }
}

/// After `actor` wrote `text` on `target` (a comment, or a description that was
/// `previous` before): new mentions get a to-do and an email, newly referenced
/// issues and MRs get a "mentioned in" note. Returns the ids of the users
/// mentioned, so the comment email can leave them out. Failures are logged.
pub async fn on_write(state: &AppState, p: &Project, actor: &User, target: Target<'_>, text: &str, previous: Option<&str>) -> Vec<i64> {
    let now = render::refs_in_prose(text);
    let before = previous.map(render::refs_in_prose).unwrap_or_default();
    let mut mentioned = Vec::new();

    // mentions: users who can see the project, not the writer
    let names: Vec<String> = now.users.iter().filter(|u| !before.users.contains(u)).cloned().collect();
    if !names.is_empty() {
        let users = forgust_db::people::by_usernames(&state.db, &names).await.unwrap_or_default();
        for u in users.into_iter().filter(|u| u.id != actor.id) {
            // a merge request (code) is not for a Guest: no to-do with its title and text
            let level = forgust_db::authz::project_level(&state.db, u.id, p.id).await.ok().flatten().unwrap_or(0);
            let need = if matches!(target, Target::MergeRequest { .. }) { REPORTER } else { GUEST };
            if level < need {
                continue;
            }
            let r = todos::add(
                &state.db,
                NewTodo {
                    user_id: u.id,
                    project_id: p.id,
                    target_type: target.kind(),
                    target_iid: target.iid(),
                    action: "mentioned",
                    author_id: Some(actor.id),
                    body: &snippet(text),
                },
            )
            .await;
            if let Err(e) = r {
                tracing::warn!(error = %e, "mention to-do not added");
            }
            mentioned.push(u.id);
        }
        let to = forgust_db::users::find_many(&state.db, &mentioned).await.unwrap_or_default();
        super::notify::tell(
            state,
            p,
            target.need(),
            actor.id,
            &to,
            super::notify::Reason::Mention,
            target.thread(),
            &format!("{} {}: {}", p.full_path, target_ref(&target), title_of(&target)),
            &format!("{} mentioned you in {}:\n\n{}\n\n{}\n", actor.username, target.label(), text.trim(), target.url(state, p)),
        )
        .await;
    }

    // cross-references: a note on each newly referenced issue or MR
    let from = target.short();
    let note = format!("mentioned in {from}");
    let issues: HashSet<i64> = now.issues.iter().filter(|i| !before.issues.contains(i)).copied().collect();
    for iid in issues {
        if matches!(target, Target::Issue { iid: own, .. } if own == iid) {
            continue;
        }
        if let Ok(Some(issue)) = idb::find(&state.db, p.id, iid).await {
            let seen = idb::notes(&state.db, issue.id).await.unwrap_or_default().iter().any(|n| n.system && n.body == note);
            if !seen {
                let n = NewIssueNote {
                    issue_id: issue.id,
                    author_id: Some(actor.id),
                    imported_author: None,
                    body: &note,
                    system: true,
                    created_at: None,
                };
                if let Err(e) = idb::add_note(&state.db, n).await {
                    tracing::warn!(error = %e, "cross-reference note not added");
                }
            }
        }
    }
    let mrs: HashSet<i64> = now.mrs.iter().filter(|i| !before.mrs.contains(i)).copied().collect();
    for iid in mrs {
        if matches!(target, Target::MergeRequest { iid: own, .. } if own == iid) {
            continue;
        }
        if let Ok(Some(m)) = mrdb::find(&state.db, p.id, iid).await {
            let seen = mrdb::notes(&state.db, m.id).await.unwrap_or_default().iter().any(|n| n.system && n.body == note);
            if !seen {
                if let Err(e) = mrdb::add_note(&state.db, m.id, Some(actor.id), None, &note, true, Position::default()).await {
                    tracing::warn!(error = %e, "cross-reference note not added");
                }
                state.events.publish(crate::events::Event::MergeRequest { project_id: p.id, iid });
            }
        }
    }
    mentioned
}

fn target_ref(t: &Target<'_>) -> String {
    match t {
        Target::MergeRequest { iid, .. } => format!("!{iid}"),
        Target::Issue { iid, .. } => format!("#{iid}"),
    }
}

fn title_of<'a>(t: &Target<'a>) -> &'a str {
    match t {
        Target::MergeRequest { title, .. } | Target::Issue { title, .. } => title,
    }
}

fn closing_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\b(?:close[sd]?|closing|fix(?:e[sd]|ing)?|resolve[sd]?|resolving|implement(?:s|ed|ing)?)\b:?\s+(#\d+(?:(?:\s*,\s*|\s*,?\s+and\s+|\s+)#\d+)*)",
        )
        .unwrap()
    })
}

/// Issues a merge request closes when it merges: `Closes #12`, `fixes #3, #4`.
pub fn closing_issues(text: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for c in closing_re().captures_iter(text) {
        for n in c[1].split(|ch: char| !ch.is_ascii_digit()).filter_map(|s| s.parse::<i64>().ok()) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// After a merge into the default branch: close the issues its description names.
pub async fn close_issues(state: &AppState, p: &Project, actor: &User, mr_iid: i64, description: &str) {
    for iid in closing_issues(description) {
        let Ok(Some(issue)) = idb::find(&state.db, p.id, iid).await else { continue };
        if issue.state != "opened" {
            continue;
        }
        let r: anyhow::Result<()> = async {
            let mut tx = state.db.begin().await?;
            idb::set_state(&mut *tx, issue.id, false, Some(actor.id)).await?;
            let body = format!("closed via merge request !{mr_iid}");
            let n = NewIssueNote {
                issue_id: issue.id,
                author_id: Some(actor.id),
                imported_author: None,
                body: &body,
                system: true,
                created_at: None,
            };
            idb::add_note(&mut *tx, n).await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(iid, error = %e, "issue not closed by the merge");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_keywords() {
        assert_eq!(closing_issues("Closes #12"), vec![12]);
        assert_eq!(closing_issues("This fixes #3, #4 and #5.\nResolved: #9"), vec![3, 4, 5, 9]);
        assert_eq!(closing_issues("implements #7 #8"), vec![7, 8]);
        assert_eq!(closing_issues("see #12; closes nothing"), Vec::<i64>::new());
        assert_eq!(closing_issues("prefixes #1"), Vec::<i64>::new());
    }
}
