//! GitHub (github.com or Enterprise Server) through the REST API.
//!
//! Pull requests become merge requests; review comments keep their line (the
//! side picks the old or new line); review summaries become comments. GitHub
//! does not tell resolved threads apart over REST, so threads import as open.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use url::Url;

use super::client::Http;
use super::model::*;
use forgust_db::access::{DEVELOPER, GUEST, MAINTAINER, REPORTER};

pub struct GitHub {
    http: Http,
    token: String,
    /// Every conversation comment of a repository (issues and pull requests),
    /// by number: read once, page by page, instead of a request per item.
    comments: tokio::sync::Mutex<HashMap<String, ByNumber<HComment>>>,
    /// Every review comment of a repository, by pull request number.
    review_comments: tokio::sync::Mutex<HashMap<String, ByNumber<HReviewComment>>>,
}

type ByNumber<T> = Arc<HashMap<i64, Vec<T>>>;

/// The number at the end of an issue or pull request API URL.
fn number_of(url: Option<&str>) -> Option<i64> {
    url?.rsplit('/').next()?.parse().ok()
}

/// The API root for a GitHub web URL: api.github.com, or `/api/v3/` on Enterprise.
pub fn api_url(web: &Url) -> Result<Url> {
    let host = web.host_str().unwrap_or_default();
    let api = if host == "github.com" || host == "www.github.com" { Url::parse("https://api.github.com/") } else { web.join("api/v3/") };
    api.map_err(|e| SourceError::Other(e.to_string()))
}

impl GitHub {
    pub fn new(api: Url, addrs: Vec<std::net::SocketAddr>, token: &str) -> Result<GitHub> {
        // no token: public repositories only (and GitHub's 60 requests an hour)
        let mut h =
            if token.is_empty() { reqwest::header::HeaderMap::new() } else { Http::header("authorization", &format!("Bearer {token}"))? };
        h.insert("x-github-api-version", reqwest::header::HeaderValue::from_static("2022-11-28"));
        let http = Http::new(api, addrs, h)?;
        Ok(GitHub { http, token: token.to_string(), comments: Default::default(), review_comments: Default::default() })
    }

    /// A repository-wide list, oldest first, grouped by the item it belongs to;
    /// fetched once (callers wait for the first one).
    async fn by_number<T: serde::de::DeserializeOwned>(
        &self,
        cache: &tokio::sync::Mutex<HashMap<String, ByNumber<T>>>,
        path: String,
        number: fn(&T) -> Option<i64>,
    ) -> Result<ByNumber<T>> {
        let mut cache = cache.lock().await;
        if let Some(hit) = cache.get(&path) {
            return Ok(hit.clone());
        }
        let list: Vec<T> = self.http.all(&path, &[("sort", "created"), ("direction", "asc")]).await?;
        let mut by: HashMap<i64, Vec<T>> = HashMap::new();
        for item in list {
            if let Some(n) = number(&item) {
                by.entry(n).or_default().push(item);
            }
        }
        let by = Arc::new(by);
        cache.insert(path, by.clone());
        Ok(by)
    }

    /// Requests left this hour (`/rate_limit` itself is free); None when the
    /// server does not limit (Enterprise can turn it off).
    async fn quota_left(&self) -> Option<i64> {
        let v: serde_json::Value = self.http.get("rate_limit", &[]).await.ok()?;
        v["resources"]["core"]["remaining"].as_i64()
    }

    async fn conversation(&self, p: &SourceProject) -> Result<ByNumber<HComment>> {
        self.by_number(&self.comments, format!("{}/issues/comments", repo(p)), |c| number_of(c.issue_url.as_deref())).await
    }
}

// ------------------------------------------------------------------ API shapes

#[derive(Deserialize, Clone)]
struct HUser {
    id: i64,
    login: String,
    name: Option<String>,
    email: Option<String>,
}

impl From<HUser> for SourceUser {
    fn from(u: HUser) -> Self {
        SourceUser {
            id: u.id.to_string(),
            name: u.name.unwrap_or_else(|| u.login.clone()),
            username: u.login,
            email: u.email.filter(|e| !e.is_empty()),
        }
    }
}

fn user(u: Option<HUser>) -> Option<SourceUser> {
    u.map(Into::into)
}

#[derive(Deserialize)]
struct HRepo {
    full_name: String,
    name: String,
    description: Option<String>,
    #[serde(default)]
    private: bool,
    visibility: Option<String>,
    default_branch: Option<String>,
    clone_url: String,
    html_url: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    size: i64,
    #[serde(default = "yes")]
    has_issues: bool,
}

fn yes() -> bool {
    true
}

impl HRepo {
    fn source(&self) -> SourceProject {
        let visibility = match self.visibility.as_deref() {
            Some("public") => "public",
            Some("internal") => "internal",
            _ if !self.private => "public",
            _ => "private",
        };
        SourceProject {
            id: self.full_name.clone(),
            full_path: self.full_name.clone(),
            name: self.name.clone(),
            path: self.name.clone(),
            description: self.description.clone().unwrap_or_default(),
            visibility: visibility.into(),
            default_branch: self.default_branch.clone(),
            http_url: self.clone_url.clone(),
            web_url: self.html_url.clone(),
            archived: self.archived,
            empty: self.size == 0,
            issues_enabled: self.has_issues,
        }
    }
}

#[derive(Deserialize)]
struct HLabel {
    name: String,
    color: String,
    description: Option<String>,
}

#[derive(Deserialize)]
struct HLabelRef {
    name: String,
}

#[derive(Deserialize, Clone)]
struct HMilestone {
    title: String,
    description: Option<String>,
    state: String,
    due_on: Option<DateTime<Utc>>,
}

impl HMilestone {
    fn source(self) -> SourceMilestone {
        SourceMilestone {
            open: self.state == "open",
            title: self.title,
            description: self.description.unwrap_or_default(),
            start_date: None,
            due_date: self.due_on.map(|d| d.date_naive()),
        }
    }
}

#[derive(Deserialize)]
struct HIssue {
    id: i64,
    number: i64,
    title: String,
    body: Option<String>,
    user: Option<HUser>,
    state: String,
    #[serde(default)]
    labels: Vec<HLabelRef>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    milestone: Option<HMilestone>,
    /// Present on pull requests, which the issues list includes.
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct HComment {
    id: i64,
    /// The issue (or pull request) it is on, in the repository-wide list.
    issue_url: Option<String>,
    body: Option<String>,
    user: Option<HUser>,
    created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct HBranchRef {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: Option<HRepoRef>,
}

#[derive(Deserialize)]
struct HRepoRef {
    full_name: String,
}

#[derive(Deserialize)]
struct HPull {
    id: i64,
    number: i64,
    title: String,
    body: Option<String>,
    user: Option<HUser>,
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    labels: Vec<HLabelRef>,
    milestone: Option<HMilestone>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    merged_at: Option<DateTime<Utc>>,
    closed_at: Option<DateTime<Utc>>,
    merge_commit_sha: Option<String>,
    head: HBranchRef,
    base: HBranchRef,
}

#[derive(Deserialize)]
struct HReviewComment {
    id: i64,
    pull_request_url: Option<String>,
    body: String,
    user: Option<HUser>,
    created_at: DateTime<Utc>,
    path: String,
    line: Option<i32>,
    original_line: Option<i32>,
    side: Option<String>,
    commit_id: Option<String>,
    original_commit_id: Option<String>,
    in_reply_to_id: Option<i64>,
}

#[derive(Deserialize)]
struct HReview {
    id: i64,
    body: Option<String>,
    user: Option<HUser>,
    state: String,
    submitted_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct HCollaborator {
    id: i64,
    login: String,
    permissions: Option<HPermissions>,
}

#[derive(Deserialize, Default)]
struct HPermissions {
    #[serde(default)]
    admin: bool,
    #[serde(default)]
    maintain: bool,
    #[serde(default)]
    push: bool,
    #[serde(default)]
    triage: bool,
}

#[derive(Deserialize)]
struct HBranch {
    name: String,
}

#[derive(Deserialize)]
struct HSearch {
    total_count: i64,
}

fn repo(p: &SourceProject) -> String {
    format!("repos/{}", p.id)
}

#[async_trait]
impl Source for GitHub {
    async fn whoami(&self) -> Result<SourceUser> {
        Ok(self.http.get::<HUser>("user", &[]).await?.into())
    }

    async fn list_projects(&self, scope: &str, search: &str) -> Result<Vec<SourceProject>> {
        let scope = scope.trim().trim_matches('/');
        let list: Vec<HRepo> = if scope.is_empty() {
            self.http.all("user/repos", &[("affiliation", "owner,collaborator,organization_member"), ("sort", "full_name")]).await?
        } else {
            match self.http.all(&format!("orgs/{scope}/repos"), &[("type", "all"), ("sort", "full_name")]).await {
                Err(SourceError::NotFound(_)) => {
                    self.http.all(&format!("users/{scope}/repos"), &[("type", "all"), ("sort", "full_name")]).await?
                }
                r => r?,
            }
        };
        let search = search.to_lowercase();
        Ok(list.iter().filter(|r| search.is_empty() || r.full_name.to_lowercase().contains(&search)).map(HRepo::source).collect())
    }

    async fn project(&self, id: &str) -> Result<SourceProject> {
        Ok(self.http.get::<HRepo>(&format!("repos/{id}"), &[]).await?.source())
    }

    fn git_auth(&self) -> Vec<(String, String)> {
        if self.token.is_empty() {
            return Vec::new();
        }
        let basic = STANDARD.encode(format!("x-access-token:{}", self.token));
        vec![("http.extraHeader".to_string(), format!("Authorization: Basic {basic}"))]
    }

    fn mr_ref_glob(&self) -> &'static str {
        "refs/pull/*/head"
    }

    /// GitHub asks integrations to go easy on concurrent requests.
    fn anonymous(&self) -> bool {
        self.token.is_empty()
    }

    fn waiting(&self) -> Option<tokio::sync::watch::Receiver<i64>> {
        Some(self.http.waiting.subscribe())
    }

    fn concurrency(&self) -> usize {
        4
    }

    async fn labels(&self, p: &SourceProject) -> Result<Vec<SourceLabel>> {
        let list: Vec<HLabel> = self.http.all(&format!("{}/labels", repo(p)), &[]).await?;
        Ok(list
            .into_iter()
            .map(|l| {
                let c = l.color.trim_start_matches('#');
                let color = if c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit()) {
                    format!("#{}", c.to_ascii_lowercase())
                } else {
                    "#6366f1".into()
                };
                SourceLabel { name: l.name, color, description: l.description.unwrap_or_default() }
            })
            .collect())
    }

    async fn milestones(&self, p: &SourceProject) -> Result<Vec<SourceMilestone>> {
        let list: Vec<HMilestone> = self.http.all(&format!("{}/milestones", repo(p)), &[("state", "all")]).await?;
        Ok(list.into_iter().map(HMilestone::source).collect())
    }

    async fn issues(&self, p: &SourceProject) -> Result<Vec<SourceIssue>> {
        let list: Vec<HIssue> =
            self.http.all(&format!("{}/issues", repo(p)), &[("state", "all"), ("sort", "created"), ("direction", "asc")]).await?;
        Ok(list
            .into_iter()
            .filter(|i| i.pull_request.is_none())
            .map(|i| SourceIssue {
                id: i.id.to_string(),
                iid: i.number,
                title: i.title,
                description: i.body.unwrap_or_default(),
                author: user(i.user),
                open: i.state == "open",
                labels: i.labels.into_iter().map(|l| l.name).collect(),
                created_at: i.created_at,
                updated_at: i.updated_at,
                closed_at: i.closed_at,
                milestone: i.milestone.map(HMilestone::source),
                due_date: None,
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
                body: c.body.clone().unwrap_or_default(),
                created_at: c.created_at,
            })
            .collect())
    }

    async fn merge_requests(&self, p: &SourceProject) -> Result<Vec<SourceMergeRequest>> {
        let list: Vec<HPull> =
            self.http.all(&format!("{}/pulls", repo(p)), &[("state", "all"), ("sort", "created"), ("direction", "asc")]).await?;
        Ok(list
            .into_iter()
            .map(|m| {
                let state = if m.merged_at.is_some() {
                    MrState::Merged
                } else if m.state == "open" {
                    MrState::Opened
                } else {
                    MrState::Closed
                };
                let from_fork = m.head.repo.as_ref().map(|r| r.full_name != p.full_path).unwrap_or(true);
                SourceMergeRequest {
                    id: m.id.to_string(),
                    iid: m.number,
                    title: m.title,
                    description: m.body.unwrap_or_default(),
                    author: user(m.user),
                    state,
                    draft: m.draft,
                    source_branch: if from_fork { format!("pull/{}", m.number) } else { m.head.name },
                    target_branch: m.base.name,
                    labels: m.labels.into_iter().map(|l| l.name).collect(),
                    milestone: m.milestone.map(HMilestone::source),
                    created_at: m.created_at,
                    updated_at: m.updated_at,
                    merged_at: m.merged_at,
                    closed_at: if m.merged_at.is_some() { None } else { m.closed_at },
                    merged_by: None,
                    merge_commit_sha: if m.merged_at.is_some() { m.merge_commit_sha } else { None },
                    squash: false,
                    // GitHub's base.sha moves with the branch; the importer
                    // computes the merge base itself
                    diff_refs: None,
                    head_sha: m.head.sha,
                    from_fork,
                }
            })
            .collect())
    }

    async fn mr_threads(&self, p: &SourceProject, mr: &SourceMergeRequest) -> Result<Vec<SourceThread>> {
        let mut threads = Vec::new();
        // conversation comments, review summaries and review comments, together
        // review summaries have no repository-wide list: one request per pull
        // request, too many for the 60 an hour GitHub allows without a token
        let reviews = async {
            if self.anonymous() {
                return Ok(Vec::new());
            }
            self.http.all::<HReview>(&format!("{}/pulls/{}/reviews", repo(p), mr.iid), &[]).await
        };
        let review_comments =
            self.by_number(&self.review_comments, format!("{}/pulls/comments", repo(p)), |c| number_of(c.pull_request_url.as_deref()));
        let (comments, reviews, review_comments) = tokio::try_join!(self.conversation(p), reviews, review_comments)?;
        for c in comments.get(&mr.iid).into_iter().flatten() {
            threads.push(SourceThread {
                notes: vec![SourceNote {
                    id: format!("c{}", c.id),
                    author: user(c.user.clone()),
                    body: c.body.clone().unwrap_or_default(),
                    created_at: c.created_at,
                    position: None,
                }],
                resolved: false,
            });
        }
        // review summaries with text
        for r in reviews {
            let body = r.body.unwrap_or_default();
            let Some(at) = r.submitted_at else { continue };
            let lead = match r.state.as_str() {
                "APPROVED" => "Approved",
                "CHANGES_REQUESTED" => "Requested changes",
                _ => "",
            };
            if body.trim().is_empty() && lead.is_empty() {
                continue;
            }
            let text = match (lead.is_empty(), body.trim().is_empty()) {
                (false, true) => format!("**{lead}**"),
                (false, false) => format!("**{lead}:** {body}"),
                _ => body,
            };
            threads.push(SourceThread {
                notes: vec![SourceNote { id: format!("r{}", r.id), author: user(r.user), body: text, created_at: at, position: None }],
                resolved: false,
            });
        }
        // review comments, grouped into threads by reply
        let mut roots: Vec<(i64, SourceThread)> = Vec::new();
        let mut index: HashMap<i64, usize> = HashMap::new();
        for c in review_comments.get(&mr.iid).into_iter().flatten() {
            let note = |position| SourceNote {
                id: format!("d{}", c.id),
                author: user(c.user.clone()),
                body: c.body.clone(),
                created_at: c.created_at,
                position,
            };
            match c.in_reply_to_id.and_then(|r| index.get(&r).copied()) {
                Some(i) => {
                    roots[i].1.notes.push(note(None));
                    index.insert(c.id, i);
                }
                None => {
                    // the line on the commit it was written on (still there when outdated)
                    let (sha, line) = match (&c.commit_id, c.line) {
                        (Some(sha), Some(line)) => (sha.clone(), Some(line)),
                        _ => (c.original_commit_id.clone().unwrap_or_default(), c.original_line),
                    };
                    let left = c.side.as_deref() == Some("LEFT");
                    let position = (!sha.is_empty() && line.is_some()).then(|| SourcePosition {
                        base_sha: None,
                        start_sha: None,
                        head_sha: sha,
                        old_path: Some(c.path.clone()),
                        new_path: Some(c.path.clone()),
                        old_line: if left { line } else { None },
                        new_line: if left { None } else { line },
                    });
                    index.insert(c.id, roots.len());
                    roots.push((c.id, SourceThread { notes: vec![note(position)], resolved: false }));
                }
            }
        }
        threads.extend(roots.into_iter().map(|(_, t)| t));
        threads.sort_by_key(|t| t.notes.first().map(|n| n.created_at));
        Ok(threads)
    }

    async fn variables(&self, _p: &SourceProject) -> Result<Vec<SourceVariable>> {
        // Actions secrets are write-only over the API
        Ok(Vec::new())
    }

    async fn protected_branches(&self, p: &SourceProject) -> Result<Vec<SourceProtectedBranch>> {
        let list: Vec<HBranch> = self.http.all(&format!("{}/branches", repo(p)), &[("protected", "true")]).await?;
        Ok(list
            .into_iter()
            .map(|b| SourceProtectedBranch { name: b.name, push_level: MAINTAINER, merge_level: DEVELOPER, allow_force_push: false })
            .collect())
    }

    async fn members(&self, p: &SourceProject) -> Result<Vec<SourceMember>> {
        let list: Vec<HCollaborator> = self.http.all(&format!("{}/collaborators", repo(p)), &[("affiliation", "all")]).await?;
        Ok(list
            .into_iter()
            .map(|c| {
                let perm = c.permissions.unwrap_or_default();
                let level = if perm.admin || perm.maintain {
                    MAINTAINER
                } else if perm.push {
                    DEVELOPER
                } else if perm.triage {
                    REPORTER
                } else {
                    GUEST.max(REPORTER)
                };
                SourceMember { user: SourceUser { id: c.id.to_string(), username: c.login.clone(), name: c.login, email: None }, level }
            })
            .collect())
    }

    async fn user_email(&self, u: &SourceUser) -> Result<Option<String>> {
        // without a token, a request per person would spend GitHub's 60 an hour
        if u.email.is_some() || self.anonymous() {
            return Ok(u.email.clone());
        }
        match self.http.get::<HUser>(&format!("users/{}", u.username), &[]).await {
            Ok(full) => Ok(full.email.filter(|e| !e.is_empty())),
            Err(SourceError::NotFound(_) | SourceError::Refused(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn plan(&self, p: &SourceProject) -> Result<Report> {
        let full: HRepo = self.http.get(&repo(p), &[]).await?;
        let search = |kind: &'static str| {
            let q = format!("repo:{} is:{kind}", p.full_path);
            async move {
                match self.http.get::<HSearch>("search/issues", &[("q", &q), ("per_page", "1")]).await {
                    Ok(s) => Ok(Some(s.total_count)),
                    Err(SourceError::Refused(_) | SourceError::NotFound(_)) => Ok(None),
                    Err(e) => Err(e),
                }
            }
        };
        let mut r = Report {
            issues: if p.issues_enabled { search("issue").await? } else { Some(0) },
            merge_requests: search("pr").await?,
            labels: self.http.count(&format!("{}/labels", repo(p)), &[]).await?,
            variables: None,
            members: self.http.count(&format!("{}/collaborators", repo(p)), &[("affiliation", "all")]).await?,
            protected_branches: self.http.count(&format!("{}/branches", repo(p)), &[("protected", "true")]).await?,
            repository_bytes: Some(full.size * 1024),
            lfs_bytes: None,
            warnings: Vec::new(),
        };
        let w = &mut r.warnings;
        if p.archived {
            w.push("The repository is archived on GitHub; it is imported as a regular project.".into());
        }
        if full.fork {
            w.push("The repository is a fork; it is imported on its own, without the link to its upstream.".into());
        }
        let workflows = self.http.count(&format!("{}/contents/.github/workflows", repo(p)), &[]).await?.unwrap_or(0);
        if workflows > 0 {
            w.push(format!("{workflows} GitHub Actions workflow file(s) do not run here: pipelines come from a .gitlab-ci.yml."));
        }
        w.push("Actions secrets cannot be read through GitHub's API; add them as CI/CD variables after the import.".into());
        w.push("Review threads import as open: GitHub's REST API does not say which ones were resolved.".into());
        if self.http.count(&format!("{}/releases", repo(p)), &[]).await?.unwrap_or(0) > 0 {
            w.push("Releases are not imported (their tags are).".into());
        }
        if let Some(attrs) = self.http.get_text(&format!("{}/contents/.gitattributes", repo(p)), &[]).await?
            && attrs.contains("lfs")
        {
            w.push("The repository uses Git LFS.".into());
        }
        if r.members.is_none() {
            w.push(if self.anonymous() {
                "Collaborators cannot be listed without a token; members will not be imported.".into()
            } else {
                "Collaborators cannot be listed with this token (push access needed); members will not be imported.".into()
            });
        }
        if self.anonymous() {
            w.push(
                "Without a token, review summaries (approvals) are not imported and people are not matched to accounts here by email: each would take a request."
                    .into(),
            );
            // what the import asks for (lists of 100) against what GitHub allows
            let pages = |n: Option<i64>| (n.unwrap_or(0) + 99) / 100;
            let comments = self.http.count(&format!("{}/issues/comments", repo(p)), &[]).await?;
            let review_comments = self.http.count(&format!("{}/pulls/comments", repo(p)), &[]).await?;
            let items = r.issues.unwrap_or(0) + r.merge_requests.unwrap_or(0);
            let need = 3 + pages(Some(items)) + pages(r.merge_requests) + pages(comments) + pages(review_comments) + pages(r.labels);
            if let Some(left) = self.quota_left().await
                && need > left
            {
                let hours = (need - left + 59) / 60;
                w.push(format!(
                    "Without a token GitHub allows 60 requests an hour ({left} left now). This import needs about {need}, so it will wait for GitHub about {hours} hour{}. A token makes it minutes; or leave out issues and merge requests to bring the repository now.",
                    if hours == 1 { "" } else { "s" }
                ));
            }
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn api_roots() {
        let gh = super::api_url(&url::Url::parse("https://github.com/").unwrap()).unwrap();
        assert_eq!(gh.as_str(), "https://api.github.com/");
        let ghe = super::api_url(&url::Url::parse("https://git.corp.example/").unwrap()).unwrap();
        assert_eq!(ghe.as_str(), "https://git.corp.example/api/v3/");
    }
}
