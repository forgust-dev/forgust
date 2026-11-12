use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProtectedBranch {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub push_access_level: i16,
    pub merge_access_level: i16,
    pub allow_force_push: bool,
    pub allow_deploy_keys: bool,
    /// Merge requests into the branch need an approval from the code owners
    /// of the files they change (`CODEOWNERS`).
    pub code_owner_approval_required: bool,
    pub created_at: DateTime<Utc>,
}

impl ProtectedBranch {
    /// The rule applies to `branch` (exact name or glob, e.g. `release/*`).
    pub fn matches(&self, branch: &str) -> bool {
        self.name == branch || globset::Glob::new(&self.name).map(|g| g.compile_matcher().is_match(branch)).unwrap_or(false)
    }
}

/// The rules that apply to `branch`.
pub fn matching<'a>(rules: &'a [ProtectedBranch], branch: &str) -> Vec<&'a ProtectedBranch> {
    rules.iter().filter(|r| r.matches(branch)).collect()
}

pub async fn list(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<ProtectedBranch>> {
    sqlx::query_as!(
        ProtectedBranch,
        "SELECT id, project_id, name, push_access_level, merge_access_level, allow_force_push, allow_deploy_keys,
                code_owner_approval_required, created_at
         FROM protected_branches WHERE project_id = $1 ORDER BY name",
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn upsert(
    e: impl PgExecutor<'_>,
    project_id: i64,
    name: &str,
    push: i16,
    merge: i16,
    allow_force_push: bool,
    allow_deploy_keys: bool,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO protected_branches (project_id, name, push_access_level, merge_access_level, allow_force_push, allow_deploy_keys)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (project_id, name) DO UPDATE SET push_access_level = EXCLUDED.push_access_level,
             merge_access_level = EXCLUDED.merge_access_level, allow_force_push = EXCLUDED.allow_force_push,
             allow_deploy_keys = EXCLUDED.allow_deploy_keys
         RETURNING id",
        project_id,
        name,
        push,
        merge,
        allow_force_push,
        allow_deploy_keys
    )
    .fetch_one(e)
    .await
}

pub async fn set_code_owner_approval(e: impl PgExecutor<'_>, id: i64, required: bool) -> Result<()> {
    sqlx::query!("UPDATE protected_branches SET code_owner_approval_required = $2 WHERE id = $1", id, required).execute(e).await?;
    Ok(())
}

pub async fn delete(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM protected_branches WHERE id = $1 AND project_id = $2", id, project_id).execute(e).await?.rows_affected()
        > 0)
}

// ------------------------------------------------------------------ protected tags

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProtectedTag {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub create_access_level: i16,
    pub created_at: DateTime<Utc>,
}

impl ProtectedTag {
    pub fn matches(&self, tag: &str) -> bool {
        self.name == tag || globset::Glob::new(&self.name).map(|g| g.compile_matcher().is_match(tag)).unwrap_or(false)
    }
}

pub async fn tags(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<ProtectedTag>> {
    sqlx::query_as!(
        ProtectedTag,
        "SELECT id, project_id, name, create_access_level, created_at FROM protected_tags WHERE project_id = $1 ORDER BY name",
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn upsert_tag(e: impl PgExecutor<'_>, project_id: i64, name: &str, create: i16) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO protected_tags (project_id, name, create_access_level) VALUES ($1, $2, $3)
         ON CONFLICT (project_id, name) DO UPDATE SET create_access_level = EXCLUDED.create_access_level RETURNING id",
        project_id,
        name,
        create
    )
    .fetch_one(e)
    .await
}

pub async fn delete_tag(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM protected_tags WHERE id = $1 AND project_id = $2", id, project_id).execute(e).await?.rows_affected() > 0)
}
