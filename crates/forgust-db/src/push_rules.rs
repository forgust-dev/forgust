//! Per-project push rules, checked by the pre-receive hook.

use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PushRules {
    /// Every new commit's message must match (empty: any).
    #[serde(default)]
    pub commit_message_regex: String,
    /// No new commit's message may match (empty: none refused).
    #[serde(default)]
    pub commit_message_negative_regex: String,
    /// Every new commit's author email must match (empty: any).
    #[serde(default)]
    pub author_email_regex: String,
    /// New branches' names must match (empty: any).
    #[serde(default)]
    pub branch_name_regex: String,
    /// Largest file a push may add, in MB (0: no limit).
    #[serde(default)]
    pub max_file_size_mb: i32,
    /// Refuse files that look like secrets (private keys, credentials).
    #[serde(default)]
    pub prevent_secrets: bool,
    /// Refuse commits without a verified signature.
    #[serde(default)]
    pub reject_unsigned_commits: bool,
}

impl PushRules {
    pub fn is_empty(&self) -> bool {
        self.commit_message_regex.is_empty()
            && self.commit_message_negative_regex.is_empty()
            && self.author_email_regex.is_empty()
            && self.branch_name_regex.is_empty()
            && self.max_file_size_mb == 0
            && !self.prevent_secrets
            && !self.reject_unsigned_commits
    }
}

pub async fn get(e: impl PgExecutor<'_>, project_id: i64) -> Result<PushRules> {
    Ok(sqlx::query_as!(
        PushRules,
        "SELECT commit_message_regex, commit_message_negative_regex, author_email_regex, branch_name_regex, max_file_size_mb,
                prevent_secrets, reject_unsigned_commits
           FROM push_rules WHERE project_id = $1",
        project_id
    )
    .fetch_optional(e)
    .await?
    .unwrap_or_default())
}

pub async fn set(e: impl PgExecutor<'_>, project_id: i64, r: &PushRules) -> Result<()> {
    sqlx::query!(
        "INSERT INTO push_rules (project_id, commit_message_regex, commit_message_negative_regex, author_email_regex, branch_name_regex,
                                 max_file_size_mb, prevent_secrets, reject_unsigned_commits)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (project_id) DO UPDATE SET commit_message_regex = $2, commit_message_negative_regex = $3,
             author_email_regex = $4, branch_name_regex = $5, max_file_size_mb = $6, prevent_secrets = $7,
             reject_unsigned_commits = $8, updated_at = now()",
        project_id,
        r.commit_message_regex,
        r.commit_message_negative_regex,
        r.author_email_regex,
        r.branch_name_regex,
        r.max_file_size_mb,
        r.prevent_secrets,
        r.reject_unsigned_commits
    )
    .execute(e)
    .await?;
    Ok(())
}
