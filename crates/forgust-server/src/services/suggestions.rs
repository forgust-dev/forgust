//! Suggested changes (M6): a diff comment with a ```suggestion block proposes
//! new content for the lines it is on; someone who may push to the source
//! branch applies it as a commit, without a working copy.
//!
//! ```suggestion:-1+2 replaces the commented line, one line above and two
//! below (GitLab's syntax); a bare ```suggestion replaces the line alone.

use forgust_db::merge_requests::{self as mrdb, MergeRequest, Position};
use forgust_db::projects::Project;
use forgust_storage::{FileChange, Identity};
use serde_json::json;

use super::mr::{committer, map_position};
use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::jobs::{PostReceive, RefUpdate};
use crate::state::AppState;

#[derive(Debug, PartialEq)]
pub struct Suggestion {
    /// Lines above the commented one that the suggestion replaces.
    pub above: usize,
    /// Lines below it.
    pub below: usize,
    /// The replacement, one entry per line (empty: delete the lines).
    pub lines: Vec<String>,
}

/// The first suggestion block of a comment.
pub fn parse(body: &str) -> Option<Suggestion> {
    let mut lines = body.lines();
    while let Some(l) = lines.next() {
        let t = l.trim_start();
        let Some(rest) = t.strip_prefix("```suggestion") else { continue };
        let (above, below) = match rest.trim().strip_prefix(':') {
            None if rest.trim().is_empty() => (0, 0),
            None => continue,
            Some(range) => {
                let caps = range.trim();
                let (a, b) = caps.split_once('+')?;
                (a.trim().strip_prefix('-')?.parse().ok()?, b.parse().ok()?)
            }
        };
        if above > 100 || below > 100 {
            return None;
        }
        let mut out = Vec::new();
        for l in lines.by_ref() {
            if l.trim_start().starts_with("```") {
                return Some(Suggestion { above, below, lines: out });
            }
            out.push(l.to_string());
        }
        return None; // unterminated
    }
    None
}

/// `text` with lines `first..=last` (1-based) replaced, keeping the file's
/// line ending and final newline.
fn replace_lines(text: &str, first: usize, last: usize, with: &[String]) -> Option<String> {
    let crlf = text.contains("\r\n");
    let nl = if crlf { "\r\n" } else { "\n" };
    let ends_with_nl = text.ends_with('\n');
    let mut lines: Vec<&str> = text.lines().collect();
    if first == 0 || last < first || last > lines.len() {
        return None;
    }
    let tail = lines.split_off(last);
    lines.truncate(first - 1);
    let mut out: Vec<String> = lines.into_iter().map(str::to_string).collect();
    out.extend(with.iter().cloned());
    out.extend(tail.into_iter().map(str::to_string));
    let mut s = out.join(nl);
    if ends_with_nl && !s.is_empty() {
        s.push_str(nl);
    }
    Some(s)
}

fn lines_of(text: &str, first: usize, last: usize) -> Option<Vec<String>> {
    let all: Vec<&str> = text.lines().collect();
    if first == 0 || last > all.len() || last < first {
        return None;
    }
    Some(all[first - 1..last].iter().map(|s| s.to_string()).collect())
}

/// Apply the suggestion of `note_id` to the source branch. Returns the new head.
pub async fn apply(state: &AppState, principal: &Principal, p: &Project, mr: &MergeRequest, note_id: i64) -> ApiResult<String> {
    let user = principal.direct_user().ok_or(ApiError::Unauthorized)?.clone();
    // check_push below only applies the protected-branch rules: the right to
    // push at all (Developer+) is checked here, as git-receive-pack does
    require_project(state, principal, ProjectAction::Push, p).await?;
    if mr.state != "opened" {
        return Err(ApiError::conflict("only open merge requests take suggestions"));
    }
    let note = mrdb::note(&state.db, mr.id, note_id).await?.ok_or(ApiError::NotFound)?;
    if note.suggestion_applied_sha.is_some() {
        return Err(ApiError::conflict("this suggestion was applied already"));
    }
    let s = parse(&note.body).ok_or_else(|| ApiError::bad("the comment has no suggestion"))?;
    // the thread's first note carries the position
    let root = match note.parent_id {
        Some(pid) => mrdb::note(&state.db, mr.id, pid).await?.ok_or(ApiError::NotFound)?,
        None => note.clone(),
    };
    let (Some(orig_head), Some(path), Some(orig_line)) = (root.head_sha.clone(), root.new_path.clone(), root.new_line) else {
        return Err(ApiError::bad("suggestions apply to a line of the new version of a file"));
    };
    let latest = mrdb::latest_diff(&state.db, mr.id).await?.ok_or(ApiError::NotFound)?;
    let head = state
        .storage
        .resolve(p.id, &format!("refs/heads/{}", mr.source_branch))
        .await?
        .ok_or_else(|| ApiError::conflict(format!("branch `{}` no longer exists", mr.source_branch)))?;
    if head != latest.head_sha {
        return Err(ApiError::conflict("the source branch just moved; reload and try again"));
    }
    let mapped = map_position(state, p.id, &root, &latest)
        .await?
        .and_then(|m| m.new_line)
        .ok_or_else(|| ApiError::conflict("the line changed since the suggestion was made (the comment is outdated)"))?;

    let (orig_line, line) = (orig_line as usize, mapped as usize);
    if orig_line <= s.above || line <= s.above {
        return Err(ApiError::bad("the suggestion starts before the first line"));
    }
    let max = 4 << 20;
    let read = |rev: String| {
        let path = path.clone();
        async move {
            let b = state.storage.blob(p.id, &rev, &path, max).await?;
            if b.truncated {
                return Err(ApiError::bad("the file is too large for suggestions"));
            }
            String::from_utf8(b.data).map_err(|_| ApiError::bad("suggestions apply to text files"))
        }
    };
    let was = read(orig_head).await?;
    let now = read(head.clone()).await?;
    let before = lines_of(&was, orig_line - s.above, orig_line + s.below);
    let current = lines_of(&now, line - s.above, line + s.below);
    if before.is_none() || before != current {
        return Err(ApiError::conflict("those lines changed since the suggestion was made"));
    }
    let content =
        replace_lines(&now, line - s.above, line + s.below, &s.lines).ok_or_else(|| ApiError::bad("the lines are out of range"))?;

    let tree =
        state.storage.tree_with_changes(p.id, &head, &[FileChange { path: path.clone(), content: Some(content.into_bytes()) }]).await?;
    let author = Identity { name: if user.name.is_empty() { user.username.clone() } else { user.name.clone() }, email: user.email.clone() };
    let msg = format!("Apply suggestion to {path}\n\nFrom merge request !{}", mr.iid);
    let commit = state.storage.commit_tree(p.id, &tree, std::slice::from_ref(&head), &msg, &author, &committer(state)).await?;

    // a push to the source branch: its rules apply, under the repo lock
    let branch = format!("refs/heads/{}", mr.source_branch);
    let check = crate::hooks::check_push(
        state,
        principal,
        p.id,
        &[crate::hooks::HookUpdate { old: head.clone(), new: commit.clone(), name: branch.clone(), force: false }],
        None,
    )
    .await?;
    if !check.allowed {
        return Err(ApiError::forbidden(check.message));
    }
    {
        let _lock = state.repo_locks.get(p.id).read_owned().await;
        state.storage.update_ref(p.id, &branch, &commit, Some(&head)).await.map_err(super::mr::conflict_error)?;
    }
    let mut tx = state.db.begin().await?;
    mrdb::set_suggestion_applied(&mut *tx, note.id, &commit).await?;
    mrdb::set_resolved(&mut *tx, root.id, Some(user.id)).await?;
    let text = format!("applied a suggestion to `{path}` ({})", &commit[..8]);
    mrdb::add_note(&mut *tx, mr.id, Some(user.id), None, &text, true, Position::default()).await?;
    state
        .jobs
        .enqueue(
            &mut tx,
            PostReceive {
                project_id: p.id,
                updates: vec![RefUpdate { old: head, new: commit.clone(), name: branch }],
                principal: Some(principal.encode()),
            }
            .request(),
        )
        .await?;
    tx.commit().await?;
    state.events.publish(Event::MergeRequest { project_id: p.id, iid: mr.iid });
    super::audit(
        state,
        principal,
        "suggestion_applied",
        Some(("project", p.id)),
        json!({ "iid": mr.iid, "note": note.id, "sha": commit }),
        None,
    )
    .await;
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_suggestions() {
        assert_eq!(parse("```suggestion\nlet x = 1;\n```"), Some(Suggestion { above: 0, below: 0, lines: vec!["let x = 1;".into()] }));
        assert_eq!(
            parse("Try:\n```suggestion:-1+2\na\nb\n```\nthanks"),
            Some(Suggestion { above: 1, below: 2, lines: vec!["a".into(), "b".into()] })
        );
        assert_eq!(parse("```suggestion\n```"), Some(Suggestion { above: 0, below: 0, lines: vec![] }));
        assert_eq!(parse("```rust\nx\n```"), None);
        assert_eq!(parse("```suggestion\nunterminated"), None);
        assert_eq!(parse("```suggestion:+1\nx\n```"), None);
    }

    #[test]
    fn replaces_lines_keeping_endings() {
        assert_eq!(replace_lines("a\nb\nc\n", 2, 2, &["B".into()]).unwrap(), "a\nB\nc\n");
        assert_eq!(replace_lines("a\nb\nc", 1, 2, &["x".into(), "y".into(), "z".into()]).unwrap(), "x\ny\nz\nc");
        assert_eq!(replace_lines("a\r\nb\r\n", 2, 2, &[]).unwrap(), "a\r\n");
        assert!(replace_lines("a\n", 2, 2, &[]).is_none());
    }
}
