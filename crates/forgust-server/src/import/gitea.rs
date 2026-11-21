//! Gitea and Forgejo (gitea.com, codeberg.org or self-hosted) through their
//! shared REST API (`/api/v1`).
//!
//! Pull requests become merge requests. Review comments keep their line and
//! whether their conversation was resolved; review summaries become comments.
//! Issues and pull requests share one sequence of numbers, as on GitHub.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use url::Url;

use super::client::{ByNumber, Http, basic_auth, by_number, number_of, urlencode};
use super::model::*;
use forgust_db::access::{DEVELOPER, MAINTAINER, REPORTER};

pub struct Gitea {
    /// Gitea or Forgejo, as chosen; the server's version says which it is.
    kind: Kind,
    http: Http,
    token: String,
    /// Every conversation comment of a repository (issues and pull requests),
    /// by number: read once, page by page, instead of a request per item.
    comments: tokio::sync::Mutex<HashMap<String, ByNumber<TComment>>>,
}

impl Gitea {
    /// `api` is the checked `/api/v1/` root.
    pub fn new(kind: Kind, api: Url, addrs: Vec<std::net::SocketAddr>, token: &str) -> Result<Gitea> {
        // no token: public repositories only
        let auth =
            if token.is_empty() { reqwest::header::HeaderMap::new() } else { Http::header("authorization", &format!("token {token}"))? };
        let http = Http::new(api, addrs, auth)?.page_param("limit");
        Ok(Gitea { kind, http, token: token.to_string(), comments: Default::default() })
    }

    /// Every conversation comment of a repository (issues and pull requests;
    /// Gitea lists them oldest first), by number.
    async fn conversation(&self, p: &SourceProject) -> Result<ByNumber<TComment>> {
        by_number(&self.http, &self.comments, format!("{}/issues/comments", repo(p)), &[], |c| {
            number_of(c.issue_url.as_deref().filter(|u| !u.is_empty())).or(number_of(c.pull_request_url.as_deref()))
        })
        .await
    }
}

/// Which forge a version string is from: Forgejo says `9.0.1+gitea-1.22.0`.
pub fn flavor(raw: &str) -> Kind {
    if raw.contains("+gitea-") { Kind::Forgejo } else { Kind::Gitea }
}

// ------------------------------------------------------------------ API shapes

#[derive(Deserialize, Clone)]
struct TUser {
    id: i64,
    login: String,
    #[serde(default)]
    full_name: String,
    email: Option<String>,
}

impl From<TUser> for SourceUser {
    fn from(u: TUser) -> Self {
        SourceUser {
            id: u.id.to_string(),
            name: if u.full_name.is_empty() { u.login.clone() } else { u.full_name },
            username: u.login,
            // Gitea answers a placeholder when the user keeps their email private
            email: u.email.filter(|e| !e.is_empty() && !e.ends_with("@noreply.localhost")),
        }
    }
}

fn user(u: Option<TUser>) -> Option<SourceUser> {
    u.map(Into::into)
}

#[derive(Deserialize)]
struct TOrg {
    #[serde(alias = "username")]
    name: String,
    #[serde(default)]
    full_name: String,
}

#[derive(Deserialize)]
struct TVersion {
    version: String,
}

#[derive(Deserialize)]
struct TRepo {
    full_name: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    internal: bool,
    default_branch: Option<String>,
    clone_url: String,
    html_url: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    mirror: bool,
    #[serde(default)]
    empty: bool,
    /// KiB
    #[serde(default)]
    size: i64,
    #[serde(default = "yes")]
    has_issues: bool,
    #[serde(default = "yes")]
    has_pull_requests: bool,
    #[serde(default)]
    has_wiki: bool,
}

fn yes() -> bool {
    true
}

impl TRepo {
    fn source(&self) -> SourceProject {
        // `internal` is set on repositories that are not private but whose
        // owner is (a limited or private organization): signed-in users only
        let visibility = if self.internal {
            "internal"
        } else if self.private {
            "private"
        } else {
            "public"
        };
        SourceProject {
            id: self.full_name.clone(),
            full_path: self.full_name.clone(),
            name: self.name.clone(),
            path: self.name.clone(),
            description: self.description.clone(),
            visibility: visibility.into(),
            default_branch: self.default_branch.clone().filter(|b| !b.is_empty()),
            http_url: self.clone_url.clone(),
            web_url: self.html_url.clone(),
            archived: self.archived,
            empty: self.empty,
            issues_enabled: self.has_issues,
        }
    }
}

#[derive(Deserialize)]
struct TLabel {
    name: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct TLabelRef {
    name: String,
}

#[derive(Deserialize, Clone)]
struct TMilestone {
    title: String,
    #[serde(default)]
    description: String,
    state: String,
    due_on: Option<DateTime<Utc>>,
}

impl TMilestone {
    fn source(self) -> SourceMilestone {
        SourceMilestone {
            open: self.state == "open",
            title: self.title,
            description: self.description,
            start_date: None,
            due_date: self.due_on.map(|d| d.date_naive()),
        }
    }
}

#[derive(Deserialize)]
struct TIssue {
    id: i64,
    number: i64,
    title: String,
    #[serde(default)]
    body: String,
    user: Option<TUser>,
    state: String,
    #[serde(default)]
    labels: Vec<TLabelRef>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    milestone: Option<TMilestone>,
    due_date: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct TComment {
    id: i64,
    /// Set on issues' comments (empty on pull requests')…
    issue_url: Option<String>,
    /// …and this on pull requests'.
    pull_request_url: Option<String>,
    #[serde(default)]
    body: String,
    user: Option<TUser>,
    created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct TBranchRef {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: Option<TRepoRef>,
}

#[derive(Deserialize)]
struct TRepoRef {
    full_name: String,
}

#[derive(Deserialize)]
struct TPull {
    id: i64,
    number: i64,
    title: String,
    #[serde(default)]
    body: String,
    user: Option<TUser>,
    state: String,
    /// Gitea 1.22 and Forgejo 8 on; a `WIP:` title says it too.
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    labels: Vec<TLabelRef>,
    milestone: Option<TMilestone>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    merged_at: Option<DateTime<Utc>>,
    closed_at: Option<DateTime<Utc>>,
    merge_commit_sha: Option<String>,
    merged_by: Option<TUser>,
    head: TBranchRef,
    base: TBranchRef,
}

#[derive(Deserialize)]
struct TReview {
    id: i64,
    #[serde(default)]
    body: String,
    user: Option<TUser>,
    state: String,
    submitted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    comments_count: i64,
}

#[derive(Deserialize)]
struct TReviewComment {
    id: i64,
    #[serde(default)]
    body: String,
    user: Option<TUser>,
    /// Who resolved the conversation, when it is.
    resolver: Option<TUser>,
    created_at: DateTime<Utc>,
    path: String,
    commit_id: Option<String>,
    original_commit_id: Option<String>,
    /// The line on the new side (0 when on the old side)…
    #[serde(default)]
    position: i64,
    /// …or on the old side.
    #[serde(default)]
    original_position: i64,
}

#[derive(Deserialize)]
struct TBranchProtection {
    #[serde(default)]
    rule_name: String,
    #[serde(default)]
    branch_name: String,
    #[serde(default)]
    enable_push: bool,
    #[serde(default)]
    enable_push_whitelist: bool,
    #[serde(default)]
    enable_merge_whitelist: bool,
    /// Gitea 1.23 and later (not on Forgejo).
    #[serde(default)]
    enable_force_push: bool,
}

#[derive(Deserialize)]
struct TPermission {
    permission: String,
}

#[derive(Deserialize)]
struct TVariable {
    name: String,
    #[serde(default)]
    data: String,
    #[serde(default)]
    description: String,
}

fn repo(p: &SourceProject) -> String {
    repo_path(&p.id)
}

/// `repos/owner/name`, each part encoded.
fn repo_path(id: &str) -> String {
    let (owner, name) = id.split_once('/').unwrap_or((id, ""));
    format!("repos/{}/{}", urlencode(owner), urlencode(name))
}

#[async_trait]
impl Source for Gitea {
    async fn whoami(&self) -> Result<SourceUser> {
        Ok(self.http.get::<TUser>("user", &[]).await?.into())
    }

    async fn version(&self) -> Option<ServerVersion> {
        // Forgejo answers its own endpoint (before 7.0, `/version` gave a
        // Gitea-like number there); Gitea does not have it
        if let Ok(v) = self.http.get_no_wait::<TVersion>("../forgejo/v1/version", &[]).await {
            return Some(ServerVersion { kind: Kind::Forgejo, raw: v.version });
        }
        let v: TVersion = self.http.get_no_wait("version", &[]).await.ok()?;
        Some(ServerVersion { kind: flavor(&v.version), raw: v.version })
    }

    async fn groups(&self) -> Result<Vec<SourceNamespace>> {
        let orgs: Vec<TOrg> = self.http.all("user/orgs", &[]).await?;
        Ok(orgs
            .into_iter()
            .map(|o| SourceNamespace {
                kind: "group".into(),
                name: if o.full_name.is_empty() { o.name.clone() } else { o.full_name },
                path: o.name,
            })
            .collect())
    }

    async fn list_projects(&self, owner: &str, search: &str) -> Result<Vec<SourceProject>> {
        let owner = owner.trim().trim_matches('/');
        let mut list: Vec<TRepo> = if owner.is_empty() {
            self.http.all("user/repos", &[]).await?
        } else {
            match self.http.all(&format!("orgs/{}/repos", urlencode(owner)), &[]).await {
                // not an organization: a user's own (their private ones too, for them)
                Err(SourceError::NotFound(_)) => self.http.all(&format!("users/{}/repos", urlencode(owner)), &[]).await?,
                r => r?,
            }
        };
        list.sort_by_key(|r| r.full_name.to_lowercase());
        let search = search.to_lowercase();
        Ok(list.iter().filter(|r| search.is_empty() || r.full_name.to_lowercase().contains(&search)).map(TRepo::source).collect())
    }

    async fn project(&self, id: &str) -> Result<SourceProject> {
        if !id.contains('/') {
            return Err(SourceError::NotFound(id.to_string()));
        }
        Ok(self.http.get::<TRepo>(&repo_path(id), &[]).await?.source())
    }

    fn git_auth(&self) -> Vec<(String, String)> {
        if self.token.is_empty() {
            return Vec::new();
        }
        vec![basic_auth("forgust", &self.token)]
    }

    fn mr_ref_glob(&self) -> &'static str {
        "refs/pull/*/head"
    }

    fn anonymous(&self) -> bool {
        self.token.is_empty()
    }

    fn waiting(&self) -> Option<tokio::sync::watch::Receiver<i64>> {
        Some(self.http.waiting.subscribe())
    }

    /// Community instances (Codeberg) ask to go easy on them.
    fn concurrency(&self) -> usize {
        4
    }

    fn shared_numbers(&self) -> bool {
        true
    }

    async fn labels(&self, p: &SourceProject) -> Result<Vec<SourceLabel>> {
        let list: Vec<TLabel> = self.http.all(&format!("{}/labels", repo(p)), &[]).await?;
        Ok(list.into_iter().map(|l| SourceLabel { color: label_color(&l.color), name: l.name, description: l.description }).collect())
    }

    async fn milestones(&self, p: &SourceProject) -> Result<Vec<SourceMilestone>> {
        let list: Vec<TMilestone> = self.http.all(&format!("{}/milestones", repo(p)), &[("state", "all")]).await?;
        Ok(list.into_iter().map(TMilestone::source).collect())
    }

    async fn issues(&self, p: &SourceProject) -> Result<Vec<SourceIssue>> {
        let mut list: Vec<TIssue> = self.http.all(&format!("{}/issues", repo(p)), &[("state", "all"), ("type", "issues")]).await?;
        list.sort_by_key(|i| i.number);
        Ok(list
            .into_iter()
            .map(|i| SourceIssue {
                id: i.id.to_string(),
                iid: i.number,
                title: i.title,
                description: i.body,
                author: user(i.user),
                open: i.state == "open",
                labels: i.labels.into_iter().map(|l| l.name).collect(),
                created_at: i.created_at,
                updated_at: i.updated_at,
                closed_at: i.closed_at,
                milestone: i.milestone.map(TMilestone::source),
                due_date: i.due_date.map(|d| d.date_naive()),
                confidential: false,
            })
            .collect())
    }

    async fn issue_comments(&self, p: &SourceProject, issue: &SourceIssue) -> Result<Vec<SourceComment>> {
        let all = self.conversation(p).await?;
        Ok(all
            .get(&issue.iid)
            .into_iter()
            .flatten()
            .map(|c| SourceComment {
                id: c.id.to_string(),
                author: user(c.user.clone()),
                body: c.body.clone(),
                created_at: c.created_at,
                internal: false,
            })
            .collect())
    }

    async fn merge_requests(&self, p: &SourceProject) -> Result<Vec<SourceMergeRequest>> {
        let mut list: Vec<TPull> = self.http.all(&format!("{}/pulls", repo(p)), &[("state", "all"), ("sort", "oldest")]).await?;
        list.sort_by_key(|m| m.number);
        Ok(list
            .into_iter()
            .map(|m| {
                let merged = m.merged || m.merged_at.is_some();
                let state = if merged {
                    MrState::Merged
                } else if m.state == "open" {
                    MrState::Opened
                } else {
                    MrState::Closed
                };
                let from_fork = m.head.repo.as_ref().map(|r| !r.full_name.eq_ignore_ascii_case(&p.full_path)).unwrap_or(true);
                SourceMergeRequest {
                    id: m.id.to_string(),
                    iid: m.number,
                    draft: m.draft || crate::services::mr::is_draft_title(&m.title),
                    title: m.title,
                    description: m.body,
                    author: user(m.user),
                    state,
                    source_branch: if from_fork { format!("pull/{}", m.number) } else { m.head.name },
                    target_branch: m.base.name,
                    labels: m.labels.into_iter().map(|l| l.name).collect(),
                    milestone: m.milestone.map(TMilestone::source),
                    created_at: m.created_at,
                    updated_at: m.updated_at,
                    merged_at: m.merged_at,
                    closed_at: if merged { None } else { m.closed_at },
                    merged_by: if merged { user(m.merged_by) } else { None },
                    merge_commit_sha: if merged { m.merge_commit_sha } else { None },
                    squash: false,
                    // the importer computes the merge base itself
                    diff_refs: None,
                    head_sha: m.head.sha,
                    from_fork,
                }
            })
            .collect())
    }

    async fn mr_threads(&self, p: &SourceProject, mr: &SourceMergeRequest) -> Result<Vec<SourceThread>> {
        let mut threads = Vec::new();
        let reviews_path = format!("{}/pulls/{}/reviews", repo(p), mr.iid);
        let (comments, reviews) = tokio::try_join!(self.conversation(p), self.http.all::<TReview>(&reviews_path, &[]))?;
        for c in comments.get(&mr.iid).into_iter().flatten() {
            threads.push(SourceThread::single(format!("c{}", c.id), user(c.user.clone()), c.body.clone(), c.created_at));
        }
        // conversations on a line: every comment on the same line and side,
        // across reviews, in the order written
        let mut lines: Vec<((String, i64, i64), SourceThread)> = Vec::new();
        for r in reviews {
            // a review still being written is only its author's
            if r.state == "PENDING" {
                continue;
            }
            if let (Some(at), Some(text)) = (r.submitted_at, review_summary(&r.state, &r.body)) {
                threads.push(SourceThread::single(format!("r{}", r.id), user(r.user.clone()), text, at));
            }
            if r.comments_count == 0 {
                continue;
            }
            let list: Vec<TReviewComment> = self.http.all(&format!("{}/pulls/{}/reviews/{}/comments", repo(p), mr.iid, r.id), &[]).await?;
            for c in list {
                let key = (c.path.clone(), c.position, c.original_position);
                let note = |position| SourceNote {
                    id: format!("d{}", c.id),
                    author: user(c.user.clone()),
                    body: c.body.clone(),
                    created_at: c.created_at,
                    position,
                    internal: false,
                };
                if let Some((_, t)) = lines.iter_mut().find(|(k, _)| *k == key) {
                    t.notes.push(note(None));
                    t.resolved |= c.resolver.is_some();
                    continue;
                }
                let sha = c.commit_id.clone().or(c.original_commit_id.clone()).unwrap_or_default();
                let (old_line, new_line) = match (c.position, c.original_position) {
                    (n, _) if n > 0 => (None, Some(n as i32)),
                    (_, o) if o > 0 => (Some(o as i32), None),
                    _ => (None, None),
                };
                let position = (!sha.is_empty() && (old_line.is_some() || new_line.is_some())).then(|| SourcePosition {
                    base_sha: None,
                    start_sha: None,
                    head_sha: sha,
                    old_path: Some(c.path.clone()),
                    new_path: Some(c.path.clone()),
                    old_line,
                    new_line,
                });
                lines.push((key, SourceThread { notes: vec![note(position)], resolved: c.resolver.is_some() }));
            }
        }
        for (_, mut t) in lines {
            t.notes.sort_by_key(|n| n.created_at);
            threads.push(t);
        }
        threads.sort_by_key(|t| t.notes.first().map(|n| n.created_at));
        Ok(threads)
    }

    async fn variables(&self, p: &SourceProject) -> Result<Vec<SourceVariable>> {
        // Actions variables (secrets are write-only over the API)
        // they need admin access to the repository: the dry run says when missing
        let list: Vec<TVariable> = match self.http.all(&format!("{}/actions/variables", repo(p)), &[]).await {
            Err(SourceError::NotFound(_) | SourceError::Refused(_)) => return Ok(Vec::new()),
            r => r?,
        };
        Ok(list
            .into_iter()
            .map(|v| SourceVariable {
                key: v.name,
                value: v.data,
                variable_type: "env_var".into(),
                protected: false,
                masked: false,
                raw: true,
                environment_scope: "*".into(),
                description: v.description,
            })
            .collect())
    }

    async fn protected_branches(&self, p: &SourceProject) -> Result<Vec<SourceProtectedBranch>> {
        let list: Vec<TBranchProtection> = match self.http.all(&format!("{}/branch_protections", repo(p)), &[]).await {
            Err(SourceError::Refused(_)) => return Ok(Vec::new()),
            r => r?,
        };
        Ok(list
            .into_iter()
            .filter_map(|b| {
                let name = if b.rule_name.is_empty() { b.branch_name } else { b.rule_name };
                (!name.is_empty()).then_some(SourceProtectedBranch {
                    name,
                    // no pushes, or only the allowed people's (taken as Maintainers)
                    push_level: match (b.enable_push, b.enable_push_whitelist) {
                        (false, _) => 0,
                        (true, true) => MAINTAINER,
                        (true, false) => DEVELOPER,
                    },
                    merge_level: if b.enable_merge_whitelist { MAINTAINER } else { DEVELOPER },
                    allow_force_push: b.enable_force_push,
                })
            })
            .collect())
    }

    async fn members(&self, p: &SourceProject) -> Result<Vec<SourceMember>> {
        let list: Vec<TUser> = self.http.all(&format!("{}/collaborators", repo(p)), &[]).await?;
        let mut out = Vec::new();
        for u in list {
            let perm: TPermission = self.http.get(&format!("{}/collaborators/{}/permission", repo(p), urlencode(&u.login)), &[]).await?;
            let level = match perm.permission.as_str() {
                "owner" | "admin" => MAINTAINER,
                "write" => DEVELOPER,
                _ => REPORTER,
            };
            out.push(SourceMember { user: u.into(), level });
        }
        Ok(out)
    }

    async fn user_email(&self, u: &SourceUser) -> Result<Option<String>> {
        if u.email.is_some() || self.anonymous() {
            return Ok(u.email.clone());
        }
        match self.http.get::<TUser>(&format!("users/{}", urlencode(&u.username)), &[]).await {
            Ok(full) => Ok(SourceUser::from(full).email),
            Err(SourceError::NotFound(_) | SourceError::Refused(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn plan(&self, p: &SourceProject) -> Result<Report> {
        let full: TRepo = self.http.get(&repo(p), &[]).await?;
        let name = self.kind.name();
        let count = |path: String, query: &'static [(&'static str, &'static str)]| async move { self.http.count(&path, query).await };
        let mut r = Report {
            issues: if p.issues_enabled {
                count(format!("{}/issues", repo(p)), &[("state", "all"), ("type", "issues")]).await?
            } else {
                Some(0)
            },
            merge_requests: if full.has_pull_requests { count(format!("{}/pulls", repo(p)), &[("state", "all")]).await? } else { Some(0) },
            labels: count(format!("{}/labels", repo(p)), &[]).await?,
            variables: count(format!("{}/actions/variables", repo(p)), &[]).await?,
            members: count(format!("{}/collaborators", repo(p)), &[]).await?,
            protected_branches: count(format!("{}/branch_protections", repo(p)), &[]).await?,
            repository_bytes: Some(full.size * 1024),
            lfs_bytes: None,
            warnings: Vec::new(),
        };
        let w = &mut r.warnings;
        if p.archived {
            w.push(format!("The repository is archived on {name}; it is imported as a regular project."));
        }
        if full.fork {
            w.push("The repository is a fork; it is imported on its own, without the link to its upstream.".into());
        }
        if full.mirror {
            w.push(format!("The repository is a mirror on {name}; it is imported once, as a regular project."));
        }
        let mut workflows = 0;
        for dir in [".gitea/workflows", ".forgejo/workflows", ".github/workflows"] {
            workflows += count(format!("{}/contents/{dir}", repo(p)), &[]).await?.unwrap_or(0);
        }
        if workflows > 0 {
            w.push(format!("{workflows} Actions workflow file(s) do not run here: pipelines come from a .gitlab-ci.yml."));
        }
        w.push("Actions secrets cannot be read through the API; add them as CI/CD variables after the import.".into());
        if full.has_wiki && count(format!("{}/wiki/pages", repo(p)), &[]).await?.unwrap_or(0) > 0 {
            w.push("The wiki has pages; wikis are not supported yet.".into());
        }
        if count(format!("{}/releases", repo(p)), &[]).await?.unwrap_or(0) > 0 {
            w.push("Releases are not imported (their tags are).".into());
        }
        if let Some(branch) = &p.default_branch {
            if let Some(attrs) = self.http.get_text(&format!("{}/raw/.gitattributes", repo(p)), &[("ref", branch)]).await?
                && attrs.contains("filter=lfs")
            {
                w.push("The repository uses Git LFS.".into());
            }
            if let Some(yaml) = self.http.get_text(&format!("{}/raw/.gitlab-ci.yml", repo(p)), &[("ref", branch)]).await? {
                w.extend(super::ci_warnings(&yaml));
            }
        }
        if r.members.is_none() {
            w.push("Collaborators cannot be listed with this token; members will not be imported.".into());
        }
        if !self.anonymous() && (r.variables.is_none() || r.protected_branches.is_none()) {
            w.push(
                "Actions variables and branch protections need admin access to the repository; with this token they will not be imported."
                    .into(),
            );
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flavors() {
        assert_eq!(flavor("1.22.3"), Kind::Gitea);
        assert_eq!(flavor("1.23.0+dev-12-gabc"), Kind::Gitea);
        assert_eq!(flavor("9.0.1+gitea-1.22.0"), Kind::Forgejo);
    }

    #[test]
    fn visibilities() {
        let v = |private: bool, internal: bool| {
            let r: TRepo = serde_json::from_value(serde_json::json!({
                "full_name": "o/r", "name": "r", "clone_url": "x", "html_url": "x", "private": private, "internal": internal
            }))
            .unwrap();
            r.source().visibility
        };
        assert_eq!(v(false, false), "public");
        assert_eq!(v(true, false), "private");
        // a public repository of a limited or private organization
        assert_eq!(v(false, true), "internal");
    }

    #[test]
    fn numbers() {
        assert_eq!(number_of(Some("https://codeberg.org/a/b/issues/12")), Some(12));
        assert_eq!(number_of(Some("https://codeberg.org/a/b/pulls/3/")), Some(3));
        assert_eq!(number_of(Some("")), None);
    }
}
