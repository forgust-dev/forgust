//! What a source forge looks like to the importer: one model for GitLab,
//! GitHub, Gitea and Forgejo, so the import job does not care where things
//! come from.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The forges Forgust imports from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = ImportSource)]
pub enum Kind {
    Gitlab,
    Github,
    Gitea,
    Forgejo,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Gitlab, Kind::Github, Kind::Gitea, Kind::Forgejo];

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Gitlab => "gitlab",
            Kind::Github => "github",
            Kind::Gitea => "gitea",
            Kind::Forgejo => "forgejo",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Gitlab => "GitLab",
            Kind::Github => "GitHub",
            Kind::Gitea => "Gitea",
            Kind::Forgejo => "Forgejo",
        }
    }

    /// The public instance the form starts with.
    pub fn default_url(self) -> &'static str {
        match self {
            Kind::Gitlab => "https://gitlab.com",
            Kind::Github => "https://github.com",
            Kind::Gitea => "https://gitea.com",
            Kind::Forgejo => "https://codeberg.org",
        }
    }

    /// The oldest self-hosted version with every API the importer reads
    /// (GitHub: Enterprise Server). Older servers are refused when they say
    /// their version; github.com and gitlab.com always run the latest. Fields
    /// that came later have fallbacks (GitLab: `draft` 14.0, `merge_user`
    /// 14.7, `internal` 15.3, variables' `raw` 15.6 and `description` 16.2).
    pub fn minimum(self) -> semver::Version {
        match self {
            // `read_api` tokens
            Kind::Gitlab => semver::Version::new(12, 10, 0),
            // `X-GitHub-Api-Version: 2022-11-28`
            Kind::Github => semver::Version::new(3, 9, 0),
            // Actions variables and draft pull requests
            Kind::Gitea => semver::Version::new(1, 22, 0),
            // Actions variables
            Kind::Forgejo => semver::Version::new(8, 0, 0),
        }
    }

    /// Projects nest in groups and subgroups (GitLab); elsewhere a project is
    /// `owner/repo`.
    pub fn nested(self) -> bool {
        self == Kind::Gitlab
    }
}

/// A server's version, from the text it gives (`17.5.1-ee`, `3.14.2`,
/// `1.22.3+dev-12`, Forgejo's `9.0.1+gitea-1.22.0`): its leading numbers.
pub fn parse_version(raw: &str) -> Option<semver::Version> {
    let core = raw.trim().trim_start_matches('v');
    let core = core.split(['-', '+', ' ']).next()?;
    let mut nums = core.split('.').map(|n| n.parse::<u64>());
    let major = nums.next()?.ok()?;
    let minor = nums.next().unwrap_or(Ok(0)).ok()?;
    let patch = nums.next().unwrap_or(Ok(0)).unwrap_or(0);
    Some(semver::Version::new(major, minor, patch))
}

/// What a source says about itself: which forge it is and its version.
#[derive(Debug, Clone)]
pub struct ServerVersion {
    /// Gitea and Forgejo share an API: the version tells them apart.
    pub kind: Kind,
    pub raw: String,
}

/// A project (GitLab) or repository (GitHub, Gitea, Forgejo) on the source.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SourceProject {
    /// GitLab project id, or GitHub `owner/repo`.
    pub id: String,
    /// group/sub/project (GitLab) or owner/repo (GitHub).
    pub full_path: String,
    pub name: String,
    pub path: String,
    pub description: String,
    /// private / internal / public
    pub visibility: String,
    pub default_branch: Option<String>,
    /// Clone URL over https.
    pub http_url: String,
    pub web_url: String,
    pub archived: bool,
    pub empty: bool,
    /// The source says this project's issues are turned on.
    pub issues_enabled: bool,
}

/// Where projects live on the source: a user's own, or a group (GitLab) or
/// organization's.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SourceNamespace {
    /// `user` or `group`.
    pub kind: String,
    /// The path to list it by: a username, `group/sub`, or an organization.
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceUser {
    pub id: String,
    pub username: String,
    pub name: String,
    /// Known only when the source shares it (GitLab admin tokens, public emails).
    pub email: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SourceLabel {
    pub name: String,
    /// #rrggbb
    pub color: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct SourceMilestone {
    pub title: String,
    pub description: String,
    pub open: bool,
    pub start_date: Option<chrono::NaiveDate>,
    pub due_date: Option<chrono::NaiveDate>,
}

#[derive(Debug, Clone)]
pub struct SourceIssue {
    /// Unique on the source (for `imported_objects`).
    pub id: String,
    pub iid: i64,
    pub title: String,
    pub description: String,
    pub author: Option<SourceUser>,
    pub open: bool,
    pub labels: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    /// Its milestone (a group one on GitLab becomes the project's).
    pub milestone: Option<SourceMilestone>,
    pub due_date: Option<chrono::NaiveDate>,
    /// Hidden on the source from all but the project's members, the author and
    /// the assignees (GitLab): imported confidential.
    pub confidential: bool,
}

#[derive(Debug, Clone)]
pub struct SourceComment {
    pub id: String,
    pub author: Option<SourceUser>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// Seen on the source by the project's members only (GitLab's internal
    /// notes): imported internal.
    pub internal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MrState {
    Opened,
    Closed,
    Merged,
}

impl MrState {
    pub fn as_str(self) -> &'static str {
        match self {
            MrState::Opened => "opened",
            MrState::Closed => "closed",
            MrState::Merged => "merged",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceMergeRequest {
    pub id: String,
    pub iid: i64,
    pub title: String,
    pub description: String,
    pub author: Option<SourceUser>,
    pub state: MrState,
    pub draft: bool,
    pub source_branch: String,
    pub target_branch: String,
    pub labels: Vec<String>,
    pub milestone: Option<SourceMilestone>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub merged_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub merged_by: Option<SourceUser>,
    pub merge_commit_sha: Option<String>,
    pub squash: bool,
    /// base, start (target head when last updated) and head of the latest diff,
    /// when the source knows them (GitLab `diff_refs`).
    pub diff_refs: Option<DiffRefs>,
    /// Head commit (always known).
    pub head_sha: String,
    /// The source branch lives in a fork (forks are not imported).
    pub from_fork: bool,
}

#[derive(Debug, Clone)]
pub struct DiffRefs {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
}

/// A diff line a comment is attached to.
#[derive(Debug, Clone)]
pub struct SourcePosition {
    pub base_sha: Option<String>,
    pub start_sha: Option<String>,
    pub head_sha: String,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub old_line: Option<i32>,
    pub new_line: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct SourceNote {
    pub id: String,
    pub author: Option<SourceUser>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub position: Option<SourcePosition>,
    /// As [`SourceComment::internal`].
    pub internal: bool,
}

/// A comment and its replies.
#[derive(Debug, Clone)]
pub struct SourceThread {
    pub notes: Vec<SourceNote>,
    pub resolved: bool,
}

impl SourceThread {
    /// One comment not on a line, with no replies.
    pub fn single(id: String, author: Option<SourceUser>, body: String, created_at: DateTime<Utc>) -> SourceThread {
        SourceThread { notes: vec![SourceNote { id, author, body, created_at, position: None, internal: false }], resolved: false }
    }
}

/// A review's summary as a comment: its verdict in bold, then its text; None
/// for a plain review without text.
pub fn review_summary(state: &str, body: &str) -> Option<String> {
    let lead = match state {
        "APPROVED" => "Approved",
        // GitHub, then Gitea and Forgejo
        "CHANGES_REQUESTED" | "REQUEST_CHANGES" => "Requested changes",
        _ => "",
    };
    match (lead.is_empty(), body.trim().is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("**{lead}**")),
        (false, false) => Some(format!("**{lead}:** {body}")),
        (true, false) => Some(body.to_string()),
    }
}

#[derive(Debug, Clone)]
pub struct SourceVariable {
    pub key: String,
    pub value: String,
    /// env_var or file
    pub variable_type: String,
    pub protected: bool,
    pub masked: bool,
    pub raw: bool,
    pub environment_scope: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct SourceProtectedBranch {
    pub name: String,
    /// Lowest role allowed to push / merge (access levels, 0 = nobody).
    pub push_level: i16,
    pub merge_level: i16,
    pub allow_force_push: bool,
}

#[derive(Debug, Clone)]
pub struct SourceMember {
    pub user: SourceUser,
    /// Access level (guest 10 … owner 50).
    pub level: i16,
}

/// What a dry run found for one project.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct Report {
    pub issues: Option<i64>,
    pub merge_requests: Option<i64>,
    pub labels: Option<i64>,
    pub variables: Option<i64>,
    pub members: Option<i64>,
    pub protected_branches: Option<i64>,
    /// Repository size on the source, when it says.
    pub repository_bytes: Option<i64>,
    pub lfs_bytes: Option<i64>,
    /// Things that will not come over or will not work the same.
    pub warnings: Vec<String>,
}

/// A label color as `#rrggbb`: sources give `#rrggbb`, `rrggbb` (GitHub,
/// Gitea), `#rgb` or a CSS name; what does not fit gets the default.
pub fn label_color(c: &str) -> String {
    let hex = c.trim().trim_start_matches('#');
    if !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return "#6366f1".into();
    }
    match hex.len() {
        6 => format!("#{}", hex.to_ascii_lowercase()),
        3 => hex.chars().flat_map(|d| [d, d]).fold(String::from("#"), |mut s, d| {
            s.push(d.to_ascii_lowercase());
            s
        }),
        _ => "#6366f1".into(),
    }
}

pub type Result<T> = std::result::Result<T, SourceError>;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("{0}")]
    Refused(String),
    #[error("not found: {0}")]
    NotFound(String),
    /// Worth retrying later (rate limit, network, 5xx).
    #[error("{0}")]
    Transient(String),
    #[error("{0}")]
    Other(String),
}

impl SourceError {
    pub fn retryable(&self) -> bool {
        matches!(self, SourceError::Transient(_))
    }
}

/// A forge to import from. Lists are fetched whole (paginated by the client).
#[async_trait]
pub trait Source: Send + Sync {
    /// The signed-in user on the source (checks the token).
    async fn whoami(&self) -> Result<SourceUser>;
    /// What the server says it is, when it says (best effort: none on
    /// github.com, or when the version needs a token that was not given).
    async fn version(&self) -> Option<ServerVersion> {
        None
    }
    /// The groups or organizations the token user belongs to.
    async fn groups(&self) -> Result<Vec<SourceNamespace>>;
    /// Projects under `owner` (a user, group or organization), or every one
    /// the token can read when it is empty; optionally matching a search.
    async fn list_projects(&self, owner: &str, search: &str) -> Result<Vec<SourceProject>>;
    async fn project(&self, id: &str) -> Result<SourceProject>;
    /// git configuration (as `GIT_CONFIG_*` pairs) that authenticates a fetch.
    fn git_auth(&self) -> Vec<(String, String)>;
    /// Where the source keeps merge request heads (`refs/merge-requests/*/head`).
    fn mr_ref_glob(&self) -> &'static str;
    async fn labels(&self, p: &SourceProject) -> Result<Vec<SourceLabel>>;
    /// The project's milestones (the ones its issues use come with them too).
    async fn milestones(&self, p: &SourceProject) -> Result<Vec<SourceMilestone>>;
    async fn issues(&self, p: &SourceProject) -> Result<Vec<SourceIssue>>;
    async fn issue_comments(&self, p: &SourceProject, issue: &SourceIssue) -> Result<Vec<SourceComment>>;
    async fn merge_requests(&self, p: &SourceProject) -> Result<Vec<SourceMergeRequest>>;
    async fn mr_threads(&self, p: &SourceProject, mr: &SourceMergeRequest) -> Result<Vec<SourceThread>>;
    /// An MR's diff refs when its list entry did not have them and git cannot
    /// tell (GitLab, fast-forward merges): one request, so asked only then.
    async fn mr_diff_refs(&self, _p: &SourceProject, _mr: &SourceMergeRequest) -> Result<Option<DiffRefs>> {
        Ok(None)
    }
    async fn variables(&self, p: &SourceProject) -> Result<Vec<SourceVariable>>;
    async fn protected_branches(&self, p: &SourceProject) -> Result<Vec<SourceProtectedBranch>>;
    async fn members(&self, p: &SourceProject) -> Result<Vec<SourceMember>>;
    /// Direct members of a source group, by its full path.
    async fn group_members(&self, _group: &str) -> Result<Vec<SourceMember>> {
        Ok(Vec::new())
    }
    /// Groups a project is shared with: (group full path, highest role).
    async fn project_shares(&self, _p: &SourceProject) -> Result<Vec<(String, i16)>> {
        Ok(Vec::new())
    }
    /// Groups a group is shared with.
    async fn group_shares(&self, _group: &str) -> Result<Vec<(String, i16)>> {
        Ok(Vec::new())
    }
    /// Fill in a user's email when the source shares it (cached by the caller).
    async fn user_email(&self, user: &SourceUser) -> Result<Option<String>>;
    /// Counts and warnings for a dry run.
    async fn plan(&self, p: &SourceProject) -> Result<Report>;
    /// No token: public data only, and what the source refuses once it refuses
    /// for every item.
    fn anonymous(&self) -> bool {
        false
    }
    /// When the client's current rate-limit wait ends (epoch seconds, 0 when not
    /// waiting), for the page to say why nothing moves.
    fn waiting(&self) -> Option<tokio::sync::watch::Receiver<i64>> {
        None
    }
    /// How many requests to have in flight at once.
    fn concurrency(&self) -> usize {
        8
    }
    /// Issues and merge requests share one sequence of numbers (GitHub): a new
    /// issue here then never takes a number a pull request had, which `#N`
    /// written on the source still names.
    fn shared_numbers(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn label_colors() {
        assert_eq!(super::label_color("#FF0000"), "#ff0000");
        assert_eq!(super::label_color("00aabb"), "#00aabb");
        assert_eq!(super::label_color("#abc"), "#aabbcc");
        assert_eq!(super::label_color("red"), "#6366f1");
        assert_eq!(super::label_color(""), "#6366f1");
    }

    #[test]
    fn review_summaries() {
        assert_eq!(super::review_summary("APPROVED", "").as_deref(), Some("**Approved**"));
        assert_eq!(super::review_summary("REQUEST_CHANGES", "fix it").as_deref(), Some("**Requested changes:** fix it"));
        assert_eq!(super::review_summary("COMMENTED", "note").as_deref(), Some("note"));
        assert_eq!(super::review_summary("COMMENTED", " "), None);
    }

    #[test]
    fn versions() {
        let v = |s: &str| super::parse_version(s).map(|v| (v.major, v.minor, v.patch));
        assert_eq!(v("17.5.1-ee"), Some((17, 5, 1)));
        assert_eq!(v("3.14"), Some((3, 14, 0)));
        assert_eq!(v("v1.22.3+dev-12-gabc"), Some((1, 22, 3)));
        assert_eq!(v("9.0.1+gitea-1.22.0"), Some((9, 0, 1)));
        assert_eq!(v("28.4.0"), Some((28, 4, 0)));
        assert_eq!(v("development"), None);
        assert!(super::parse_version("1.21.11").unwrap() < super::Kind::Gitea.minimum());
        assert!(super::parse_version("28.0.0").unwrap() >= super::Kind::Gitea.minimum(), "Gitea dropped its `1.` prefix");
    }
}
