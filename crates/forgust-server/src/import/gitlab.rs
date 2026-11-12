//! GitLab (gitlab.com or self-managed) through `/api/v4`.

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use url::Url;

use super::client::Http;
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
    pub fn new(web: Url, addrs: Vec<std::net::SocketAddr>, token: &str) -> Result<GitLab> {
        let api = web.join("api/v4/").map_err(|e| SourceError::Other(e.to_string()))?;
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
}

#[derive(Deserialize)]
struct GNote {
    id: i64,
    body: String,
    author: Option<GUser>,
    created_at: DateTime<Utc>,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    resolved: bool,
    position: Option<GPosition>,
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

    async fn list_projects(&self, scope: &str, search: &str) -> Result<Vec<SourceProject>> {
        let scope = scope.trim().trim_matches('/');
        let list: Vec<GProject> = if scope.is_empty() {
            self.http.all("projects", &[("membership", "true"), ("search", search), ("order_by", "path"), ("sort", "asc")]).await?
        } else {
            // projects shared with the group live elsewhere: not part of it
            let path = format!("groups/{}/projects", urlencode(scope));
            let all: Vec<GProject> = self
                .http
                .all(
                    &path,
                    &[("include_subgroups", "true"), ("with_shared", "false"), ("search", search), ("order_by", "path"), ("sort", "asc")],
                )
                .await?;
            let prefix = format!("{}/", scope.to_lowercase());
            all.into_iter().filter(|p| p.source().full_path.to_lowercase().starts_with(&prefix)).collect()
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
        let mut cfg = Vec::new();
        if !self.token.is_empty() {
            let basic = STANDARD.encode(format!("oauth2:{}", self.token));
            cfg.push(("http.extraHeader".to_string(), format!("Authorization: Basic {basic}")));
        }
        cfg.extend(pin(&self.web, &self.http.addrs));
        cfg
    }

    fn mr_ref_glob(&self) -> &'static str {
        "refs/merge-requests/*/head"
    }

    async fn labels(&self, p: &SourceProject) -> Result<Vec<SourceLabel>> {
        let list: Vec<GLabel> = self.http.all(&format!("{}/labels", pid(p)), &[("include_ancestor_groups", "true")]).await?;
        Ok(list
            .into_iter()
            .map(|l| SourceLabel { name: l.name, color: normalize_color(&l.color), description: l.description.unwrap_or_default() })
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
            })
            .collect())
    }

    async fn issue_comments(&self, p: &SourceProject, issue: &SourceIssue) -> Result<Vec<SourceComment>> {
        let list: Vec<GNote> =
            self.http.all(&format!("{}/issues/{}/notes", pid(p), issue.iid), &[("order_by", "created_at"), ("sort", "asc")]).await?;
        Ok(list
            .into_iter()
            .filter(|n| !n.system)
            .map(|n| SourceComment { id: n.id.to_string(), author: user(n.author), body: n.body, created_at: n.created_at })
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

/// Path segments as GitLab wants them in URLs (`group%2Fproject`).
pub fn urlencode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// GitLab colors are #rrggbb, sometimes #rgb or a CSS name; keep what fits.
fn normalize_color(c: &str) -> String {
    let c = c.trim();
    if c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit()) {
        return c.to_ascii_lowercase();
    }
    if c.len() == 4 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit()) {
        let d: Vec<char> = c[1..].chars().collect();
        return format!("#{0}{0}{1}{1}{2}{2}", d[0], d[1], d[2]).to_ascii_lowercase();
    }
    "#6366f1".into()
}

/// The `http.curloptResolve` entry pinning git to the checked addresses: one
/// entry listing them all (curl keeps only the last entry per host and port),
/// IPv4 first.
pub fn pin(web: &Url, addrs: &[std::net::SocketAddr]) -> Vec<(String, String)> {
    let (Some(host), Some(port)) = (web.host_str(), web.port_or_known_default()) else { return Vec::new() };
    if addrs.is_empty() {
        return Vec::new();
    }
    let mut ips: Vec<std::net::IpAddr> = addrs.iter().map(|a| a.ip()).collect();
    ips.sort_by_key(|ip| ip.is_ipv6());
    ips.dedup();
    let list: Vec<String> = ips
        .iter()
        .map(|ip| match ip {
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
            ip => ip.to_string(),
        })
        .collect();
    vec![("http.curloptResolve".to_string(), format!("{host}:{port}:{}", list.join(",")))]
}

#[cfg(test)]
mod tests {
    #[test]
    fn colors() {
        assert_eq!(super::normalize_color("#FF0000"), "#ff0000");
        assert_eq!(super::normalize_color("#abc"), "#aabbcc");
        assert_eq!(super::normalize_color("red"), "#6366f1");
    }

    #[test]
    fn pins() {
        let u = url::Url::parse("https://gitlab.example.com/").unwrap();
        // one entry (curl keeps the last per host:port), IPv4 first
        let p = super::pin(&u, &["[2001:db8::1]:443".parse().unwrap(), "10.0.0.5:443".parse().unwrap()]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].1, "gitlab.example.com:443:10.0.0.5,[2001:db8::1]");
    }
}
