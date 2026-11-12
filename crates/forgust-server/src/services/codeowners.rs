//! `CODEOWNERS`, GitLab's format: the first of `CODEOWNERS`,
//! `docs/CODEOWNERS` and `.gitlab/CODEOWNERS` on the target branch.
//!
//! ```text
//! # default section
//! *            @lead
//! /docs/       @writers
//! *.rs         @rust-team alice@example.com
//!
//! [Backend][2] @backend          # two approvals; default owners
//! /api/
//! ^[Optional]  @someone          # never blocks a merge
//! ```
//!
//! Patterns are gitignore-like: `/x` is anchored at the root, a pattern with
//! no slash matches at any depth, `dir/` covers everything below. Within a
//! section the last matching line wins. Owners are `@user`, `@group/sub` (its
//! members) or an email; only those with at least the Developer role count.

use std::collections::{BTreeMap, BTreeSet};

use forgust_db::projects::Project;
use globset::{GlobBuilder, GlobMatcher};
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::ApiResult;
use crate::state::AppState;

pub const LOCATIONS: &[&str] = &["CODEOWNERS", "docs/CODEOWNERS", ".gitlab/CODEOWNERS"];

#[derive(Debug, Clone)]
pub struct Entry {
    pub pattern: String,
    matchers: Vec<GlobMatcher>,
    pub owners: Vec<String>,
}

impl Entry {
    pub fn matches(&self, path: &str) -> bool {
        self.matchers.iter().any(|m| m.is_match(path))
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    /// Empty for the lines before any `[Section]`.
    pub name: String,
    pub optional: bool,
    pub approvals: u32,
    pub entries: Vec<Entry>,
}

fn glob(p: &str) -> Option<GlobMatcher> {
    GlobBuilder::new(p).literal_separator(true).build().ok().map(|g| g.compile_matcher())
}

/// gitignore-like pattern → globs over repository paths.
fn matchers(pattern: &str) -> Vec<GlobMatcher> {
    let p = pattern.replace("\\ ", " ");
    if p == "*" {
        return glob("**").into_iter().collect();
    }
    let dir = p.ends_with('/');
    let anchored = p.starts_with('/') || p.trim_end_matches('/').contains('/');
    let base = p.trim_start_matches('/').trim_end_matches('/');
    if base.is_empty() {
        return Vec::new();
    }
    let mut globs = if dir { vec![format!("{base}/**")] } else { vec![base.to_string(), format!("{base}/**")] };
    if !anchored {
        globs = globs.into_iter().map(|g| format!("**/{g}")).collect();
    }
    globs.iter().filter_map(|g| glob(g)).collect()
}

/// Split a line into its pattern and owners (`\ ` escapes a space).
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                cur.push_str("\\ ");
                chars.next();
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn parse(text: &str) -> Vec<Section> {
    let mut sections = vec![Section { name: String::new(), optional: false, approvals: 1, entries: Vec::new() }];
    let mut defaults: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // a section header: ^[Name][N] @default owners
        let (optional, rest) = match line.strip_prefix('^') {
            Some(r) if r.starts_with('[') => (true, r),
            _ => (false, line),
        };
        if let Some(after) = rest.strip_prefix('[')
            && let Some(close) = after.find(']')
        {
            let name = after[..close].trim().to_string();
            let mut tail = &after[close + 1..];
            let mut approvals = 1;
            if let Some(n) = tail.strip_prefix('[')
                && let Some(c) = n.find(']')
            {
                approvals = n[..c].trim().parse().unwrap_or(1).max(1);
                tail = &n[c + 1..];
            }
            defaults = tokens(tail.split('#').next().unwrap_or(""));
            // the same name again continues that section (GitLab merges them)
            if let Some(i) = sections.iter().position(|s| s.name.eq_ignore_ascii_case(&name) && !name.is_empty()) {
                let s = sections.remove(i);
                sections.push(s);
            } else {
                sections.push(Section { name, optional, approvals, entries: Vec::new() });
            }
            continue;
        }
        let mut t = tokens(line);
        if t.is_empty() {
            continue;
        }
        let pattern = t.remove(0);
        let owners: Vec<String> =
            t.into_iter().take_while(|o| !o.starts_with('#')).filter(|o| o.starts_with('@') || o.contains('@')).collect();
        let owners = if owners.is_empty() { defaults.clone() } else { owners };
        let section = sections.last_mut().expect("there is always one");
        section.entries.push(Entry { matchers: matchers(&pattern), pattern, owners });
    }
    sections.retain(|s| !s.entries.is_empty());
    sections
}

/// Per section: the owners of `path` (the last matching line), when one matches.
pub fn owners_of<'a>(sections: &'a [Section], path: &str) -> Vec<(&'a Section, &'a Entry)> {
    sections.iter().filter_map(|s| s.entries.iter().rev().find(|e| e.matches(path)).map(|e| (s, e))).collect()
}

/// The `CODEOWNERS` file at `commit`: (path, parsed sections).
pub async fn load(state: &AppState, p: &Project, commit: &str) -> ApiResult<Option<(String, Vec<Section>)>> {
    for loc in LOCATIONS {
        match state.storage.blob(p.id, commit, loc, 1 << 20).await {
            Ok(b) if !b.truncated => return Ok(Some((loc.to_string(), parse(&String::from_utf8_lossy(&b.data))))),
            Ok(_) => continue,
            Err(forgust_storage::StorageError::PathNotFound(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}

/// Owner references → usernames of people with at least the Developer role
/// on the project.
async fn resolve(state: &AppState, p: &Project, refs: &BTreeSet<String>) -> ApiResult<BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    for r in refs {
        let mut ids: Vec<i64> = Vec::new();
        if let Some(name) = r.strip_prefix('@') {
            if let Some(u) = forgust_db::users::find_by_username(&state.db, name).await? {
                ids.push(u.id);
            } else {
                let mut conn = state.db.acquire().await?;
                if let Some(hit) = forgust_db::routes::lookup(&mut conn, name).await?
                    && hit.source_type == "namespace"
                {
                    ids = forgust_db::members::group_member_ids_with_ancestors(&state.db, hit.source_id).await?;
                }
            }
        } else if let Some(u) = forgust_db::users::find_by_email(&state.db, r).await?
            && u.email_verified()
        {
            ids.push(u.id);
        }
        let mut names = Vec::new();
        for u in forgust_db::users::find_many(&state.db, &ids).await? {
            let level = if u.is_admin {
                forgust_db::access::OWNER
            } else {
                forgust_db::authz::project_level(&state.db, u.id, p.id).await?.unwrap_or(0)
            };
            if u.is_active() && level >= forgust_db::access::DEVELOPER {
                names.push(u.username);
            }
        }
        names.sort();
        out.insert(r.clone(), names);
    }
    Ok(out)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SectionReport {
    /// Empty for the default section.
    pub name: String,
    pub optional: bool,
    pub approvals_required: u32,
    /// Usernames who own some changed file of this section.
    pub owners: Vec<String>,
    /// Changed files this section covers.
    pub files: Vec<String>,
    /// Code owners of this section who approved.
    pub approved_by: Vec<String>,
    pub satisfied: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CodeOwners {
    /// Where the rules come from (None: the target branch has no CODEOWNERS).
    pub file: Option<String>,
    pub sections: Vec<SectionReport>,
    /// Changed file → its owners' usernames (every section).
    pub file_owners: BTreeMap<String, Vec<String>>,
    /// Owners who are not reviewers yet (nor the author).
    pub suggested_reviewers: Vec<forgust_db::people::UserBrief>,
    /// The target branch requires their approval (a protected branch setting).
    pub approval_required: bool,
}

/// Code owners of a merge request's changes, and whether they approved.
pub async fn for_merge_request(state: &AppState, p: &Project, mr: &forgust_db::merge_requests::MergeRequest) -> ApiResult<CodeOwners> {
    let rules = forgust_db::protected_branches::list(&state.db, p.id).await?;
    let approval_required =
        forgust_db::protected_branches::matching(&rules, &mr.target_branch).iter().any(|r| r.code_owner_approval_required);
    let empty =
        CodeOwners { file: None, sections: Vec::new(), file_owners: BTreeMap::new(), suggested_reviewers: Vec::new(), approval_required };
    let target = state.storage.resolve(p.id, &format!("refs/heads/{}", mr.target_branch)).await?;
    let head = state.storage.resolve(p.id, &format!("refs/heads/{}", mr.source_branch)).await?;
    let (Some(target), Some(head)) = (target, head) else { return Ok(empty) };
    let Some((file, sections)) = load(state, p, &target).await? else { return Ok(empty) };
    let base = state.storage.merge_base(p.id, &target, &head).await?.unwrap_or(target.clone());
    let changed = state.storage.changed_paths(p.id, None, &base, &head).await?;

    // which entry covers each changed file, per section
    let mut per_section: Vec<(BTreeSet<String>, Vec<String>)> = vec![(BTreeSet::new(), Vec::new()); sections.len()];
    let mut refs = BTreeSet::new();
    let mut file_refs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for path in &changed {
        for (s, e) in owners_of(&sections, path) {
            let i = sections.iter().position(|x| std::ptr::eq(x, s)).expect("from the list");
            per_section[i].0.extend(e.owners.iter().cloned());
            per_section[i].1.push(path.clone());
            refs.extend(e.owners.iter().cloned());
            file_refs.entry(path.clone()).or_default().extend(e.owners.iter().cloned());
        }
    }
    let resolved = resolve(state, p, &refs).await?;
    let names = |set: &BTreeSet<String>| -> Vec<String> {
        let mut v: Vec<String> = set.iter().flat_map(|r| resolved.get(r).cloned().unwrap_or_default()).collect();
        v.sort();
        v.dedup();
        v
    };
    let approvals = forgust_db::merge_requests::approvals(&state.db, mr.id, p.id).await?;
    let approvers: BTreeSet<&str> = approvals.iter().map(|a| a.username.as_str()).collect();
    let mut reports = Vec::new();
    let mut all_owners = BTreeSet::new();
    for (s, (owner_refs, files)) in sections.iter().zip(per_section) {
        if files.is_empty() {
            continue;
        }
        let owners = names(&owner_refs);
        all_owners.extend(owners.iter().cloned());
        let approved_by: Vec<String> = owners.iter().filter(|o| approvers.contains(o.as_str())).cloned().collect();
        // no eligible owner at all: nobody could ever approve, so (as in GitLab)
        // the section does not block
        let satisfied = s.optional || owners.is_empty() || approved_by.len() as u32 >= s.approvals.min(owners.len() as u32);
        reports.push(SectionReport {
            name: s.name.clone(),
            optional: s.optional,
            approvals_required: if s.optional { 0 } else { s.approvals },
            owners,
            files,
            approved_by,
            satisfied,
        });
    }
    let author = match mr.author_id {
        Some(id) => forgust_db::users::find(&state.db, id).await?.map(|u| u.username),
        None => None,
    };
    let reviewers: BTreeSet<String> =
        forgust_db::people::mr_reviewers(&state.db, &[mr.id]).await?.into_iter().map(|(_, r)| r.user.username).collect();
    let mut suggested_reviewers = Vec::new();
    for name in all_owners.into_iter().filter(|o| Some(o) != author.as_ref() && !reviewers.contains(o)) {
        if let Some(u) = forgust_db::users::find_by_username(&state.db, &name).await? {
            suggested_reviewers.push(forgust_db::people::UserBrief { id: u.id, username: u.username, name: u.name });
        }
    }
    Ok(CodeOwners {
        file: Some(file),
        sections: reports,
        file_owners: file_refs.iter().map(|(f, r)| (f.clone(), names(r))).collect(),
        suggested_reviewers,
        approval_required,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owners(sections: &[Section], path: &str) -> Vec<(String, Vec<String>)> {
        owners_of(sections, path).into_iter().map(|(s, e)| (s.name.clone(), e.owners.clone())).collect()
    }

    #[test]
    fn gitlab_patterns_and_sections() {
        let s = parse(
            "# comment\n* @lead\n/docs/ @writers\n*.rs @rust ann@example.com\nREADME.md @readme # trailing\nsrc/lib.rs @lib\n\n\
             [Backend][2] @backend\n/api/\n/api/v4/ @v4\n^[Optional] @opt\n*.md\n[My\\ Docs]\nfile\\ with\\ space.txt @spaces\n",
        );
        assert_eq!(s.len(), 4);
        assert_eq!(owners(&s, "main.go"), [("".into(), vec!["@lead".into()])]);
        assert_eq!(owners(&s, "docs/guide/intro.txt"), [("".into(), vec!["@writers".into()])]);
        assert_eq!(owners(&s, "a/docs/x.txt"), [("".into(), vec!["@lead".into()])], "/docs/ is anchored");
        assert_eq!(owners(&s, "deep/x.rs"), [("".into(), vec!["@rust".into(), "ann@example.com".into()])]);
        assert_eq!(owners(&s, "src/lib.rs"), [("".into(), vec!["@lib".into()])], "the last match wins");
        assert_eq!(owners(&s, "x/src/lib.rs")[0].1, vec!["@rust", "ann@example.com"], "src/lib.rs is anchored");
        assert_eq!(owners(&s, "sub/README.md")[0].1, vec!["@readme"], "no slash: any depth");
        let api = owners(&s, "api/v4/users.rs");
        assert_eq!(api[1], ("Backend".into(), vec!["@v4".into()]));
        assert_eq!(owners(&s, "api/v3/x.rs")[1], ("Backend".into(), vec!["@backend".into()]), "default owners");
        let backend = s.iter().find(|x| x.name == "Backend").unwrap();
        assert_eq!((backend.approvals, backend.optional), (2, false));
        let opt = s.iter().find(|x| x.name == "Optional").unwrap();
        assert!(opt.optional);
        assert_eq!(owners(&s, "notes.md").last().unwrap().1, vec!["@opt"]);
        assert_eq!(owners(&s, "file with space.txt").last().unwrap().1, vec!["@spaces"]);
    }
}
