//! API response shapes.

use chrono::{DateTime, Utc};
use forgust_db::namespaces::Namespace;
use forgust_db::projects::Project;
use forgust_db::users::User;
use forgust_db::{access, visibility};
use serde::Serialize;
use utoipa::ToSchema;

use crate::config::Config;

#[derive(Serialize, ToSchema)]
pub struct Resolved {
    /// `project`, `group` or `user`
    pub kind: String,
    pub id: i64,
    pub full_path: String,
    pub redirected: bool,
}

#[derive(Serialize, ToSchema)]
pub struct UserDto {
    pub id: i64,
    pub username: String,
    pub name: String,
    pub email: Option<String>,
    pub user_type: String,
    /// Only to the user themselves and to admins (who to aim at).
    pub is_admin: Option<bool>,
    /// `active` or `blocked`; only to the user themselves and to admins.
    pub state: Option<String>,
    /// Only to the user themselves and to admins (it tells who is easiest to attack).
    pub two_factor_enabled: Option<bool>,
    pub namespace_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    /// Shown with the email: whether it is confirmed, and a new address
    /// waiting for its confirmation link.
    pub email_verified: Option<bool>,
    pub unconfirmed_email: Option<String>,
    /// The avatar's URL (versioned), when it has one.
    pub avatar_url: Option<String>,
}

impl UserDto {
    /// Emails and account security are only shown to the user themselves and to admins.
    pub fn from(u: &User, show_email: bool) -> Self {
        UserDto {
            id: u.id,
            username: u.username.clone(),
            name: u.name.clone(),
            email: show_email.then(|| u.email.clone()),
            user_type: u.user_type.clone(),
            is_admin: show_email.then_some(u.is_admin),
            state: show_email.then(|| u.state.clone()),
            two_factor_enabled: show_email.then_some(u.totp_enabled),
            namespace_id: u.namespace_id,
            created_at: u.created_at,
            email_verified: show_email.then(|| u.email_verified()),
            unconfirmed_email: if show_email { u.unconfirmed_email.clone() } else { None },
            avatar_url: u.avatar_at.map(|at| format!("/api/v1/users/{}/avatar?v={}", u.username, at.timestamp())),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct GroupDto {
    pub id: i64,
    /// `group` or `user`
    pub kind: String,
    pub name: String,
    pub path: String,
    pub full_path: String,
    pub parent_id: Option<i64>,
    pub visibility: String,
    pub description: String,
    pub depth: usize,
    pub is_org: bool,
    pub web_url: String,
    pub created_at: DateTime<Utc>,
    /// The avatar's URL (versioned), when it has one.
    pub avatar_url: Option<String>,
}

impl GroupDto {
    pub fn from(n: &Namespace, cfg: &Config) -> Self {
        GroupDto {
            id: n.id,
            kind: n.kind.clone(),
            name: n.name.clone(),
            path: n.path.clone(),
            full_path: n.full_path.clone(),
            parent_id: n.parent_id,
            visibility: visibility::name(n.visibility).into(),
            description: n.description.clone(),
            depth: n.depth(),
            is_org: n.is_group() && n.parent_id.is_none(),
            web_url: cfg.web_url(&n.full_path),
            created_at: n.created_at,
            avatar_url: n.avatar_at.map(|at| format!("/api/v1/groups/{}/avatar?v={}", n.id, at.timestamp())),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct OrgPolicyDto {
    pub max_visibility: Option<String>,
    pub require_two_factor: bool,
    pub project_creation_level: String,
    pub subgroup_creation_level: String,
}

#[derive(Serialize, ToSchema)]
pub struct Breadcrumb {
    pub id: i64,
    pub name: String,
    pub full_path: String,
}

#[derive(Serialize, ToSchema)]
pub struct GroupDetail {
    #[serde(flatten)]
    pub group: GroupDto,
    /// Present on orgs (top-level groups).
    pub policy: Option<OrgPolicyDto>,
    pub ancestors: Vec<Breadcrumb>,
    /// The viewer's effective role (`none` when not a member).
    pub access_level: String,
    pub can_create_project: bool,
    pub can_create_subgroup: bool,
    pub can_admin: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ProjectDto {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub full_path: String,
    pub namespace_id: i64,
    pub description: String,
    pub visibility: String,
    pub default_branch: Option<String>,
    pub ref_format: String,
    pub http_url: String,
    pub ssh_url: String,
    pub web_url: String,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    /// The avatar's URL (versioned), when it has one.
    pub avatar_url: Option<String>,
    /// Read-only since then (its Owner can unarchive it).
    pub archived: bool,
    pub archived_at: Option<DateTime<Utc>>,
}

impl ProjectDto {
    pub fn from(p: &Project, cfg: &Config) -> Self {
        ProjectDto {
            id: p.id,
            name: p.name.clone(),
            path: p.path.clone(),
            full_path: p.full_path.clone(),
            namespace_id: p.namespace_id,
            description: p.description.clone(),
            visibility: visibility::name(p.visibility).into(),
            default_branch: p.default_branch.clone(),
            ref_format: p.ref_format.clone(),
            http_url: cfg.http_clone_url(&p.full_path),
            ssh_url: cfg.ssh_clone_url(&p.full_path),
            web_url: cfg.web_url(&p.full_path),
            created_at: p.created_at,
            last_activity_at: p.last_activity_at,
            avatar_url: p.avatar_at.map(|at| format!("/api/v1/projects/{}/avatar?v={}", p.id, at.timestamp())),
            archived: p.archived(),
            archived_at: p.archived_at,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct ProjectDetail {
    #[serde(flatten)]
    pub project: ProjectDto,
    pub ancestors: Vec<Breadcrumb>,
    pub access_level: String,
    pub can_read_code: bool,
    pub can_push: bool,
    pub can_admin: bool,
    pub can_own: bool,
    pub empty_repo: bool,
    /// Older paths that still redirect here.
    pub redirects: Vec<String>,
    /// Its storage quota, or its namespace's, is 80 % or 100 % full (for those
    /// who can push).
    pub storage_warning: Option<forgust_db::quotas::Warning>,
}

/// A quota and what it holds.
#[derive(Serialize, ToSchema)]
pub struct QuotaUse {
    /// The project, or the top-level group or personal namespace (its path).
    pub name: String,
    pub used: i64,
    pub quota: i64,
}

/// What a project keeps, against its quotas.
#[derive(Serialize, ToSchema)]
pub struct ProjectStorageUse {
    #[serde(flatten)]
    pub kinds: forgust_db::releases::StorageUse,
    pub repository_bytes: i64,
    pub total_bytes: i64,
    /// The project's own quota.
    pub quota: Option<QuotaUse>,
    /// Its top-level namespace's (shared by all its projects).
    pub namespace: Option<QuotaUse>,
}

/// What a top-level group keeps, against its quota.
#[derive(Serialize, ToSchema)]
pub struct GroupStorageUse {
    /// The top-level group this counts for (a subgroup shows its organization's).
    pub namespace: String,
    pub used: i64,
    pub quota: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct MemberDto {
    pub user_id: i64,
    pub username: String,
    pub name: String,
    pub user_type: String,
    pub access_level: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<forgust_db::members::Member> for MemberDto {
    fn from(m: forgust_db::members::Member) -> Self {
        MemberDto {
            user_id: m.user_id,
            username: m.username,
            name: m.name,
            user_type: m.user_type,
            access_level: access::name(m.access_level).into(),
            expires_at: m.expires_at,
            created_at: m.created_at,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct ShareDto {
    pub group_id: i64,
    pub group_full_path: String,
    pub max_access_level: String,
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<forgust_db::links::ShareLink> for ShareDto {
    fn from(l: forgust_db::links::ShareLink) -> Self {
        ShareDto {
            group_id: l.group_id,
            group_full_path: l.group_full_path,
            max_access_level: access::name(l.max_access_level).into(),
            expires_at: l.expires_at,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct ProtectedBranchDto {
    pub id: i64,
    pub name: String,
    pub push_access_level: String,
    pub merge_access_level: String,
    pub allow_force_push: bool,
    pub allow_deploy_keys: bool,
    /// Merge requests need the approval of the changed files' code owners.
    pub code_owner_approval_required: bool,
}

pub fn push_level_name(l: i16) -> &'static str {
    if l == 0 { "no_one" } else { access::name(l) }
}

impl From<forgust_db::protected_branches::ProtectedBranch> for ProtectedBranchDto {
    fn from(b: forgust_db::protected_branches::ProtectedBranch) -> Self {
        ProtectedBranchDto {
            id: b.id,
            name: b.name,
            push_access_level: push_level_name(b.push_access_level).into(),
            merge_access_level: push_level_name(b.merge_access_level).into(),
            allow_force_push: b.allow_force_push,
            allow_deploy_keys: b.allow_deploy_keys,
            code_owner_approval_required: b.code_owner_approval_required,
        }
    }
}

/// A token's secret is shown once, at creation.
#[derive(Serialize, ToSchema)]
pub struct NewToken {
    pub id: i64,
    pub token: String,
    pub username: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AccessTokenDto {
    /// The bot user behind the token.
    pub bot_user_id: i64,
    pub bot_username: String,
    pub name: String,
    pub access_level: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
    pub last_used_at: Option<DateTime<Utc>>,
}
