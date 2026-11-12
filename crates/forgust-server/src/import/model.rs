//! What a source forge looks like to the importer: one model for GitLab and
//! GitHub, so the import job does not care where things come from.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A project (GitLab) or repository (GitHub) on the source.
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
}

#[derive(Debug, Clone)]
pub struct SourceComment {
    pub id: String,
    pub author: Option<SourceUser>,
    pub body: String,
    pub created_at: DateTime<Utc>,
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
}

/// A comment and its replies.
#[derive(Debug, Clone)]
pub struct SourceThread {
    pub notes: Vec<SourceNote>,
    pub resolved: bool,
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
    /// Projects the token can import from, optionally under a group/owner or
    /// matching a search.
    async fn list_projects(&self, scope: &str, search: &str) -> Result<Vec<SourceProject>>;
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
}
