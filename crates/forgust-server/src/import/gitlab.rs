//! GitLab (gitlab.com or self-managed) through `/api/v4`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use url::Url;

use super::client::{Http, basic_auth, urlencode};
use super::model::*;

pub struct GitLab {
    http: Http,
    web: Url,
    token: String,
    /// People's emails are looked up until the source rate-limits those
    /// lookups (gitlab.com limits `users/:id` per hour for anonymous clients).
    lookups_off: std::sync::atomic::AtomicBool,
}

impl GitLab {
    /// `web` is the instance's address, `api` its checked `/api/v4/` root.
    pub fn new(web: Url, api: Url, addrs: Vec<std::net::SocketAddr>, token: &str) -> Result<GitLab> {
        // no token: public projects only
        let auth = if token.is_empty() { reqwest::header::HeaderMap::new() } else { Http::header("private-token", token)? };
        let http = Http::new(api, addrs, auth)?;
        Ok(GitLab { http, web, token: token.to_string(), lookups_off: Default::default() })
    }
}

// ------------------------------------------------------------------ API shapes

#[derive(Deserialize)]
struct GUser {
    id: i64,
    username: String,
    #[serde(default)]
    name: String,
    email: Option<String>,
    public_email: Option<String>,
}

impl From<GUser> for SourceUser {
    fn from(u: GUser) -> Self {
        let email = u.email.or(u.public_email).filter(|e| !e.is_empty());
        SourceUser { id: u.id.to_string(), username: u.username, name: u.name, email }
    }
}

fn user(u: Option<GUser>) -> Option<SourceUser> {
    u.map(Into::into)
}

#[derive(Deserialize)]
struct GGroup {
    full_path: String,
    #[serde(default)]
    full_name: String,
}

#[derive(Deserialize)]
struct GVersion {
    version: String,
}

#[derive(Deserialize)]
struct GProject {
    id: i64,
    path_with_namespace: String,
    name: String,
    path: String,
    description: Option<String>,
    visibility: Option<String>,
    default_branch: Option<String>,
    http_url_to_repo: String,
    web_url: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    empty_repo: bool,
    issues_enabled: Option<bool>,
    issues_access_level: Option<String>,
    statistics: Option<GStats>,
    container_registry_enabled: Option<bool>,
    wiki_enabled: Option<bool>,
    #[serde(default)]
    forked_from_project: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GStats {
    repository_size: Option<i64>,
    lfs_objects_size: Option<i64>,
}

impl GProject {
    fn source(&self) -> SourceProject {
        SourceProject {
            id: self.id.to_string(),
            full_path: self.path_with_namespace.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            description: self.description.clone().unwrap_or_default(),
            visibility: self.visibility.clone().unwrap_or_else(|| "private".into()),
            default_branch: self.default_branch.clone(),
            http_url: self.http_url_to_repo.clone(),
            web_url: self.web_url.clone(),
            archived: self.archived,
            empty: self.empty_repo,
            issues_enabled: self.issues_enabled.unwrap_or(true) && self.issues_access_level.as_deref() != Some("disabled"),
        }
    }
}

#[derive(Deserialize)]
struct GLabel {
    name: String,
    color: String,
    description: Option<String>,
}

#[derive(Deserialize, Clone)]
struct GMilestone {
    title: String,
    description: Option<String>,
    state: String,
    start_date: Option<chrono::NaiveDate>,
    due_date: Option<chrono::NaiveDate>,
}

impl GMilestone {
    fn source(self) -> SourceMilestone {
        SourceMilestone {
            open: self.state == "active",
            title: self.title,
            description: self.description.unwrap_or_default(),
            start_date: self.start_date,
            due_date: self.due_date,
        }
    }
}

#[derive(Deserialize)]
struct GIssue {
    id: i64,
    iid: i64,
    title: String,
    description: Option<String>,
    author: Option<GUser>,
    state: String,
    #[serde(default)]
    labels: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    milestone: Option<GMilestone>,
    due_date: Option<chrono::NaiveDate>,
    #[serde(default)]
    confidential: bool,
}

#[derive(Deserialize)]
struct GNote {
    id: i64,
    body: String,
    author: Option<GUser>,
    created_at: DateTime<Utc>,
    #[serde(default)]
    system: bool,
    /// Shown on GitLab to the project's members only: imported internal
    #[serde(default)]
    internal: bool,
    /// The same, as GitLab 14 named it (later versions send both)
    #[serde(default)]
    confidential: bool,
    #[serde(default)]
    resolved: bool,
    position: Option<GPosition>,
}

impl GNote {
    fn members_only(&self) -> bool {
        self.internal || self.confidential
    }
}

#[derive(Deserialize)]
struct GPosition {
    base_sha: Option<String>,
    start_sha: Option<String>,
    head_sha: Option<String>,
    old_path: Option<String>,
    new_path: Option<String>,
    old_line: Option<i32>,
    new_line: Option<i32>,
    position_type: Option<String>,
}

#[derive(Deserialize)]
struct GDiscussion {
    notes: Vec<GNote>,
}

#[derive(Deserialize)]
struct GMergeRequest {
    id: i64,
    iid: i64,
    title: String,
    description: Option<String>,
    author: Option<GUser>,
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    work_in_progress: bool,
    source_branch: String,
    target_branch: String,
    #[serde(default)]
    labels: Vec<String>,
    milestone: Option<GMilestone>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    merged_at: Option<DateTime<Utc>>,
    closed_at: Option<DateTime<Utc>>,
    merged_by: Option<GUser>,
    merge_user: Option<GUser>,
    merge_commit_sha: Option<String>,
    squash_commit_sha: Option<String>,
    #[serde(default)]
    squash: bool,
    sha: Option<String>,
    source_project_id: Option<i64>,
    target_project_id: Option<i64>,
    diff_refs: Option<GDiffRefs>,
}

#[derive(Deserialize)]
struct GDiffRefs {
    base_sha: Option<String>,
    start_sha: Option<String>,
    head_sha: Option<String>,
}

impl GDiffRefs {
    fn complete(self) -> Option<DiffRefs> {
        Some(DiffRefs { base_sha: self.base_sha?, start_sha: self.start_sha?, head_sha: self.head_sha? })
    }
}

#[derive(Deserialize)]
struct GVariable {
    key: String,
    value: String,
    variable_type: Option<String>,
    #[serde(default)]
    protected: bool,
    #[serde(default)]
    masked: bool,
    #[serde(default)]
    raw: bool,
    environment_scope: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct GAccess {
    access_level: Option<i16>,
}

#[derive(Deserialize)]
struct GProtectedBranch {
    name: String,
    #[serde(default)]
    push_access_levels: Vec<GAccess>,
    #[serde(default)]
    merge_access_levels: Vec<GAccess>,
    #[serde(default)]
    allow_force_push: bool,
}

#[derive(Deserialize)]
struct GMember {
    id: i64,
    username: String,
    #[serde(default)]
    name: String,
    access_level: i16,
    email: Option<String>,
    public_email: Option<String>,
}

impl GMember {
    fn member(self) -> SourceMember {
        SourceMember {
            level: self.access_level.min(50),
            user: SourceUser {
                id: self.id.to_string(),
                username: self.username,
                name: self.name,
                email: self.email.or(self.public_email).filter(|e| !e.is_empty()),
            },
        }
    }
}

/// `shared_with_groups` of a project or group.
#[derive(Deserialize)]
struct GShares {
    #[serde(default)]
    shared_with_groups: Vec<GShare>,
}

#[derive(Deserialize)]
struct GShare {
    group_full_path: String,
    group_access_level: i16,
}

impl GShares {
    fn list(self) -> Vec<(String, i16)> {
        self.shared_with_groups.into_iter().map(|g| (g.group_full_path, g.group_access_level.min(50))).collect()
    }
}

/// Lowest role in a list of access levels (0 = no one); GitLab's 60 (admin) maps to Owner.
fn lowest(levels: &[GAccess]) -> i16 {
    levels.iter().filter_map(|a| a.access_level).filter(|l| *l > 0).min().map(|l| l.min(50)).unwrap_or(0)
}

fn pid(p: &SourceProject) -> String {
    format!("projects/{}", p.id)
}

#[async_trait]
impl Source for GitLab {
    async fn whoami(&self) -> Result<SourceUser> {
        Ok(self.http.get::<GUser>("user", &[]).await?.into())
    }

    async fn version(&self) -> Option<ServerVersion> {
        // gitlab.com runs the latest; elsewhere `/version` wants a token
        if self.web.host_str() == Some("gitlab.com") || self.token.is_empty() {
            return None;
        }
        let v: GVersion = self.http.get_no_wait("version", &[]).await.ok()?;
        Some(ServerVersion { kind: Kind::Gitlab, raw: v.version })
    }

    async fn groups(&self) -> Result<Vec<SourceNamespace>> {
        let groups: Vec<GGroup> = self.http.all("groups", &[("min_access_level", "10"), ("order_by", "path"), ("sort", "asc")]).await?;
        Ok(groups
            .into_iter()
            .map(|g| SourceNamespace {
                kind: "group".into(),
                name: if g.full_name.is_empty() { g.full_path.clone() } else { g.full_name },
                path: g.full_path,
            })
            .collect())
    }

    async fn list_projects(&self, owner: &str, search: &str) -> Result<Vec<SourceProject>> {
        let owner = owner.trim().trim_matches('/');
        let order = [("search", search), ("order_by", "path"), ("sort", "asc")];
        let list: Vec<GProject> = if owner.is_empty() {
            self.http.all("projects", &[[("membership", "true")].as_slice(), &order].concat()).await?
        } else {
            // projects shared with the group live elsewhere: not part of it
            let path = format!("groups/{}/projects", urlencode(owner));
            match self
                .http
                .all::<GProject>(&path, &[[("include_subgroups", "true"), ("with_shared", "false")].as_slice(), &order].concat())
                .await
            {
                Ok(all) => {
                    let prefix = format!("{}/", owner.to_lowercase());
                    all.into_iter().filter(|p| p.source().full_path.to_lowercase().starts_with(&prefix)).collect()
                }
                // not a group: a user's personal projects
                Err(SourceError::NotFound(_)) => self.http.all(&format!("users/{}/projects", urlencode(owner)), &order).await?,
                Err(e) => return Err(e),
            }
        };
        // forks are imported as plain projects; their upstream link is lost
        Ok(list.iter().map(GProject::source).collect())
    }

    async fn project(&self, id: &str) -> Result<SourceProject> {
        Ok(self.http.get::<GProject>(&format!("projects/{}", urlencode(id)), &[]).await?.source())
    }

    async fn mr_diff_refs(&self, p: &SourceProject, mr: &SourceMergeRequest) -> Result<Option<DiffRefs>> {
        match self.http.get::<GMergeRequest>(&format!("{}/merge_requests/{}", pid(p), mr.iid), &[]).await {
            Ok(full) => Ok(full.diff_refs.and_then(GDiffRefs::complete)),
            Err(SourceError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn anonymous(&self) -> bool {
        self.token.is_empty()
    }

    fn waiting(&self) -> Option<tokio::sync::watch::Receiver<i64>> {
        Some(self.http.waiting.subscribe())
    }

    fn git_auth(&self) -> Vec<(String, String)> {
        if self.token.is_empty() { Vec::new() } else { vec![basic_auth("oauth2", &self.token)] }
    }

    fn mr_ref_glob(&self) -> &'static str {
        "refs/merge-requests/*/head"
    }

    async fn labels(&self, p: &SourceProject) -> Result<Vec<SourceLabel>> {
        let list: Vec<GLabel> = self.http.all(&format!("{}/labels", pid(p)), &[("include_ancestor_groups", "true")]).await?;
        Ok(list
            .into_iter()
            .map(|l| SourceLabel { name: l.name, color: label_color(&l.color), description: l.description.unwrap_or_default() })
            .collect())
    }

    async fn milestones(&self, p: &SourceProject) -> Result<Vec<SourceMilestone>> {
        let list: Vec<GMilestone> = self.http.all(&format!("{}/milestones", pid(p)), &[]).await?;
        Ok(list.into_iter().map(GMilestone::source).collect())
    }

    async fn issues(&self, p: &SourceProject) -> Result<Vec<SourceIssue>> {
        let list: Vec<GIssue> =
            self.http.all(&format!("{}/issues", pid(p)), &[("scope", "all"), ("order_by", "created_at"), ("sort", "asc")]).await?;
        Ok(list
            .into_iter()
            .map(|i| SourceIssue {
                id: i.id.to_string(),
                iid: i.iid,
                title: i.title,
                description: i.description.unwrap_or_default(),
                author: user(i.author),
                open: i.state == "opened",
                labels: i.labels,
                created_at: i.created_at,
                updated_at: i.updated_at,
                closed_at: i.closed_at,
                milestone: i.milestone.map(GMilestone::source),
                due_date: i.due_date,
                confidential: i.confidential,
            })
            .collect())
    }

    async fn issue_comments(&self, p: &SourceProject, issue: &SourceIssue) -> Result<Vec<SourceComment>> {
        let list: Vec<GNote> =
            self.http.all(&format!("{}/issues/{}/notes", pid(p), issue.iid), &[("order_by", "created_at"), ("sort", "asc")]).await?;
        Ok(list
            .into_iter()
            .filter(|n| !n.system)
            .map(|n| SourceComment {
                internal: n.members_only(),
                id: n.id.to_string(),
                author: user(n.author),
                body: n.body,
                created_at: n.created_at,
            })
            .collect())
    }

    async fn merge_requests(&self, p: &SourceProject) -> Result<Vec<SourceMergeRequest>> {
        let list: Vec<GMergeRequest> = self
            .http
            .all(&format!("{}/merge_requests", pid(p)), &[("scope", "all"), ("state", "all"), ("order_by", "created_at"), ("sort", "asc")])
            .await?;
        // the list has no diff_refs; git works them out, else `mr_diff_refs` asks
        let mut out = Vec::with_capacity(list.len());
        for mut m in list {
            let diff_refs = m.diff_refs.take().and_then(GDiffRefs::complete);
            let head_sha = m.sha.clone().or_else(|| diff_refs.as_ref().map(|d| d.head_sha.clone())).unwrap_or_default();
            let state = match m.state.as_str() {
                "merged" => MrState::Merged,
                "opened" | "locked" => MrState::Opened,
                _ => MrState::Closed,
            };
            out.push(SourceMergeRequest {
                id: m.id.to_string(),
                iid: m.iid,
                title: m.title,
                description: m.description.unwrap_or_default(),
                author: user(m.author),
                state,
                draft: m.draft || m.work_in_progress,
                source_branch: m.source_branch,
                target_branch: m.target_branch,
                labels: m.labels,
                milestone: m.milestone.map(GMilestone::source),
                created_at: m.created_at,
                updated_at: m.updated_at,
                merged_at: m.merged_at,
                closed_at: m.closed_at,
                merged_by: user(m.merged_by.or(m.merge_user)),
                merge_commit_sha: m.merge_commit_sha.or(m.squash_commit_sha),
                squash: m.squash,
                diff_refs,
                head_sha,
                from_fork: m.source_project_id.is_some() && m.source_project_id != m.target_project_id,
            });
        }
        Ok(out)
    }

    async fn mr_threads(&self, p: &SourceProject, mr: &SourceMergeRequest) -> Result<Vec<SourceThread>> {
        let list: Vec<GDiscussion> = self.http.all(&format!("{}/merge_requests/{}/discussions", pid(p), mr.iid), &[]).await?;
        Ok(list
            .into_iter()
            .filter_map(|d| {
                let resolved = d.notes.first().is_some_and(|n| n.resolved);
                let notes: Vec<SourceNote> = d
                    .notes
                    .into_iter()
                    .filter(|n| !n.system)
                    .map(|n| SourceNote {
                        internal: n.members_only(),
                        id: n.id.to_string(),
                        author: user(n.author),
                        body: n.body,
                        created_at: n.created_at,
                        position: n.position.filter(|p| p.position_type.as_deref().unwrap_or("text") == "text").and_then(|p| {
                            Some(SourcePosition {
                                base_sha: p.base_sha,
                                start_sha: p.start_sha,
                                head_sha: p.head_sha?,
                                old_path: p.old_path,
                                new_path: p.new_path,
                                old_line: p.old_line,
                                new_line: p.new_line,
                            })
                        }),
                    })
                    .collect();
                (!notes.is_empty()).then_some(SourceThread { notes, resolved })
            })
            .collect())
    }

    async fn variables(&self, p: &SourceProject) -> Result<Vec<SourceVariable>> {
        let list: Vec<GVariable> = self.http.all(&format!("{}/variables", pid(p)), &[]).await?;
        Ok(list
            .into_iter()
            .map(|v| SourceVariable {
                key: v.key,
                value: v.value,
                variable_type: v.variable_type.unwrap_or_else(|| "env_var".into()),
                protected: v.protected,
                masked: v.masked,
                raw: v.raw,
                environment_scope: v.environment_scope.unwrap_or_else(|| "*".into()),
                description: v.description.unwrap_or_default(),
            })
            .collect())
    }

    async fn protected_branches(&self, p: &SourceProject) -> Result<Vec<SourceProtectedBranch>> {
        let list: Vec<GProtectedBranch> = self.http.all(&format!("{}/protected_branches", pid(p)), &[]).await?;
        Ok(list
            .into_iter()
            .map(|b| SourceProtectedBranch {
                push_level: lowest(&b.push_access_levels),
                merge_level: lowest(&b.merge_access_levels),
                allow_force_push: b.allow_force_push,
                name: b.name,
            })
            .collect())
    }

    async fn members(&self, p: &SourceProject) -> Result<Vec<SourceMember>> {
        // effective members (direct and inherited)
        let list: Vec<GMember> = self.http.all(&format!("{}/members/all", pid(p)), &[]).await?;
        Ok(list.into_iter().map(GMember::member).collect())
    }

    async fn group_members(&self, group: &str) -> Result<Vec<SourceMember>> {
        let list: Vec<GMember> = self.http.all(&format!("groups/{}/members", urlencode(group)), &[]).await?;
        Ok(list.into_iter().map(GMember::member).collect())
    }

    async fn project_shares(&self, p: &SourceProject) -> Result<Vec<(String, i16)>> {
        let s: GShares = self.http.get(&pid(p), &[]).await?;
        Ok(s.list())
    }

    async fn group_shares(&self, group: &str) -> Result<Vec<(String, i16)>> {
        let s: GShares = self.http.get(&format!("groups/{}", urlencode(group)), &[("with_projects", "false")]).await?;
        Ok(s.list())
    }

    async fn user_email(&self, u: &SourceUser) -> Result<Option<String>> {
        if u.email.is_some() {
            return Ok(u.email.clone());
        }
        use std::sync::atomic::Ordering::Relaxed;
        if self.lookups_off.load(Relaxed) {
            return Ok(None);
        }
        // an email is a nice-to-have: a rate limit stops the lookups instead
        // of holding the import up for an hour
        match self.http.get_no_wait::<GUser>(&format!("users/{}", u.id), &[]).await {
            Ok(full) => Ok(SourceUser::from(full).email),
            Err(SourceError::NotFound(_) | SourceError::Refused(_)) => Ok(None),
            Err(SourceError::Transient(m)) => {
                tracing::info!(reason = %m, "import: no more email lookups on this source");
                self.lookups_off.store(true, Relaxed);
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn plan(&self, p: &SourceProject) -> Result<Report> {
        let base = pid(p);
        let full: GProject = self.http.get(&base, &[("statistics", "true")]).await?;
        let mut r = Report {
            issues: self.http.count(&format!("{base}/issues"), &[("scope", "all")]).await?,
            merge_requests: self.http.count(&format!("{base}/merge_requests"), &[("scope", "all"), ("state", "all")]).await?,
            labels: self.http.count(&format!("{base}/labels"), &[("include_ancestor_groups", "true")]).await?,
            variables: self.http.count(&format!("{base}/variables"), &[]).await?,
            members: self.http.count(&format!("{base}/members/all"), &[]).await?,
            protected_branches: self.http.count(&format!("{base}/protected_branches"), &[]).await?,
            repository_bytes: full.statistics.as_ref().and_then(|s| s.repository_size),
            lfs_bytes: full.statistics.as_ref().and_then(|s| s.lfs_objects_size).filter(|n| *n > 0),
            warnings: Vec::new(),
        };
        let w = &mut r.warnings;
        if p.archived {
            w.push("The project is archived on GitLab; it is imported as a regular project.".into());
        }
        if full.forked_from_project.is_some() {
            w.push("The project is a fork; it is imported on its own, without the link to its upstream.".into());
        }
        if r.variables.is_none() {
            w.push("CI/CD variables cannot be read with this token (Maintainer role needed); they will not be imported.".into());
        }
        if full.container_registry_enabled == Some(true)
            && self.http.count(&format!("{base}/registry/repositories"), &[]).await?.unwrap_or(0) > 0
        {
            w.push("The container registry has images; they are not copied (push them again, or copy them with skopeo copy --all).".into());
        }
        if full.wiki_enabled == Some(true) && self.http.count(&format!("{base}/wikis"), &[]).await?.unwrap_or(0) > 0 {
            w.push("The wiki has pages; wikis are not supported yet.".into());
        }
        if self.http.count(&format!("{base}/packages"), &[]).await?.unwrap_or(0) > 0 {
            w.push("The package registry has packages; they are not imported.".into());
        }
        if self.http.count(&format!("{base}/releases"), &[]).await?.unwrap_or(0) > 0 {
            w.push("Releases are not imported (their tags are).".into());
        }
        if let Some(branch) = &p.default_branch {
            let file = format!("{base}/repository/files/{}/raw", urlencode(".gitlab-ci.yml"));
            if let Some(yaml) = self.http.get_text(&file, &[("ref", branch)]).await? {
                w.extend(super::ci_warnings(&yaml));
            }
        }
        Ok(r)
    }
}
