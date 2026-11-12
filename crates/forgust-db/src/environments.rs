//! Environments, deployments and protected environments.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Environment {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    pub slug: String,
    pub external_url: Option<String>,
    pub tier: String,
    /// available, stopping or stopped.
    pub state: String,
    pub stop_build_id: Option<i64>,
    pub auto_stop_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The environment, created on its first deployment.
pub async fn ensure(e: impl PgExecutor<'_>, project_id: i64, name: &str, slug: &str, tier: &str) -> Result<Environment> {
    sqlx::query_as!(
        Environment,
        "INSERT INTO environments (project_id, name, slug, tier) VALUES ($1, $2, $3, $4)
         ON CONFLICT (project_id, name) DO UPDATE SET name = EXCLUDED.name
         RETURNING id, project_id, name, slug, external_url, tier, state, stop_build_id, auto_stop_at, created_at, updated_at",
        project_id,
        name,
        slug,
        tier
    )
    .fetch_one(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<Option<Environment>> {
    sqlx::query_as!(
        Environment,
        "SELECT id, project_id, name, slug, external_url, tier, state, stop_build_id, auto_stop_at, created_at, updated_at
           FROM environments WHERE project_id = $1 AND id = $2",
        project_id,
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn by_name(e: impl PgExecutor<'_>, project_id: i64, name: &str) -> Result<Option<Environment>> {
    sqlx::query_as!(
        Environment,
        "SELECT id, project_id, name, slug, external_url, tier, state, stop_build_id, auto_stop_at, created_at, updated_at
           FROM environments WHERE project_id = $1 AND name = $2",
        project_id,
        name
    )
    .fetch_optional(e)
    .await
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct EnvironmentSummary {
    #[serde(flatten)]
    pub environment: Environment,
    pub last_deployment: Option<Deployment>,
}

pub async fn list(e: &mut PgConnection, project_id: i64, state: Option<&str>) -> Result<Vec<EnvironmentSummary>> {
    let envs = sqlx::query_as!(
        Environment,
        "SELECT id, project_id, name, slug, external_url, tier, state, stop_build_id, auto_stop_at, created_at, updated_at
           FROM environments WHERE project_id = $1 AND ($2::text IS NULL OR state = $2) ORDER BY name",
        project_id,
        state
    )
    .fetch_all(&mut *e)
    .await?;
    // every environment's last deployment in one query (not one each)
    let ids: Vec<i64> = envs.iter().map(|x| x.id).collect();
    let mut last: std::collections::HashMap<i64, Deployment> = sqlx::query_as!(
        Deployment,
        r#"SELECT DISTINCT ON (d.environment_id) d.id, d.iid, d.environment_id, d.build_id, d.ref AS git_ref, d.tag, d.sha, d.status, d.user_id,
                  u.username AS "username?", d.created_at, d.finished_at
             FROM deployments d LEFT JOIN users u ON u.id = d.user_id
            WHERE d.environment_id = ANY($1) ORDER BY d.environment_id, d.id DESC"#,
        &ids
    )
    .fetch_all(&mut *e)
    .await?
    .into_iter()
    .map(|d| (d.environment_id, d))
    .collect();
    Ok(envs.into_iter().map(|env| EnvironmentSummary { last_deployment: last.remove(&env.id), environment: env }).collect())
}

/// After a successful deployment: where it is, how to stop it, when.
pub async fn deployed(
    e: impl PgExecutor<'_>,
    id: i64,
    external_url: Option<&str>,
    stop_build_id: Option<i64>,
    auto_stop_at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE environments SET state = 'available', external_url = COALESCE($2, external_url), stop_build_id = $3,
                auto_stop_at = $4, updated_at = now() WHERE id = $1",
        id,
        external_url,
        stop_build_id,
        auto_stop_at
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_state(e: impl PgExecutor<'_>, id: i64, state: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE environments SET state = $2, auto_stop_at = CASE WHEN $2 = 'stopped' THEN NULL ELSE auto_stop_at END, updated_at = now()
          WHERE id = $1",
        id,
        state
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn delete(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM environments WHERE project_id = $1 AND id = $2 AND state = 'stopped'", project_id, id)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}

/// Available environments whose `auto_stop_in` has passed: (project, id).
pub async fn due_auto_stop(e: impl PgExecutor<'_>) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query!("SELECT project_id, id FROM environments WHERE state = 'available' AND auto_stop_at <= now() LIMIT 100")
        .fetch_all(e)
        .await?
        .into_iter()
        .map(|r| (r.project_id, r.id))
        .collect())
}

// ------------------------------------------------------------------ deployments

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Deployment {
    pub id: i64,
    pub iid: i64,
    pub environment_id: i64,
    pub build_id: Option<i64>,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub tag: bool,
    pub sha: String,
    /// created, running, success, failed, canceled, skipped or blocked.
    pub status: String,
    pub user_id: Option<i64>,
    pub username: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub struct NewDeployment<'a> {
    pub project_id: i64,
    pub iid: i64,
    pub environment_id: i64,
    pub build_id: i64,
    pub git_ref: &'a str,
    pub tag: bool,
    pub sha: &'a str,
    pub user_id: Option<i64>,
}

pub async fn create_deployment(e: impl PgExecutor<'_>, d: NewDeployment<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO deployments (project_id, iid, environment_id, build_id, ref, tag, sha, user_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (build_id) WHERE build_id IS NOT NULL DO UPDATE SET status = 'created' RETURNING id",
        d.project_id,
        d.iid,
        d.environment_id,
        d.build_id,
        d.git_ref,
        d.tag,
        d.sha,
        d.user_id
    )
    .fetch_one(e)
    .await
}

pub async fn deployments(e: impl PgExecutor<'_>, environment_id: i64, limit: i64) -> Result<Vec<Deployment>> {
    sqlx::query_as!(
        Deployment,
        r#"SELECT d.id, d.iid, d.environment_id, d.build_id, d.ref AS git_ref, d.tag, d.sha, d.status, d.user_id,
                  u.username AS "username?", d.created_at, d.finished_at
             FROM deployments d LEFT JOIN users u ON u.id = d.user_id
            WHERE d.environment_id = $1 ORDER BY d.id DESC LIMIT $2"#,
        environment_id,
        limit
    )
    .fetch_all(e)
    .await
}

/// The deployment a job makes, with its environment's id.
pub async fn of_build(e: impl PgExecutor<'_>, build_id: i64) -> Result<Option<Deployment>> {
    sqlx::query_as!(
        Deployment,
        r#"SELECT d.id, d.iid, d.environment_id, d.build_id, d.ref AS git_ref, d.tag, d.sha, d.status, d.user_id,
                  u.username AS "username?", d.created_at, d.finished_at
             FROM deployments d LEFT JOIN users u ON u.id = d.user_id WHERE d.build_id = $1"#,
        build_id
    )
    .fetch_optional(e)
    .await
}

pub async fn set_deployment_status(e: impl PgExecutor<'_>, id: i64, status: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE deployments SET status = $2,
                finished_at = CASE WHEN $2 IN ('success', 'failed', 'canceled', 'skipped') THEN now() ELSE NULL END
          WHERE id = $1",
        id,
        status
    )
    .execute(e)
    .await?;
    Ok(())
}

// ------------------------------------------------------------------ protected environments

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProtectedEnvironment {
    pub id: i64,
    pub name: String,
    pub deploy_access_level: i16,
}

impl ProtectedEnvironment {
    pub fn matches(&self, env: &str) -> bool {
        self.name == env || globset::Glob::new(&self.name).map(|g| g.compile_matcher().is_match(env)).unwrap_or(false)
    }
}

pub async fn protected(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<ProtectedEnvironment>> {
    sqlx::query_as!(
        ProtectedEnvironment,
        "SELECT id, name, deploy_access_level FROM protected_environments WHERE project_id = $1 ORDER BY name",
        project_id
    )
    .fetch_all(e)
    .await
}

pub async fn protect(e: impl PgExecutor<'_>, project_id: i64, name: &str, level: i16) -> Result<()> {
    sqlx::query!(
        "INSERT INTO protected_environments (project_id, name, deploy_access_level) VALUES ($1, $2, $3)
         ON CONFLICT (project_id, name) DO UPDATE SET deploy_access_level = EXCLUDED.deploy_access_level",
        project_id,
        name,
        level
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn unprotect(e: impl PgExecutor<'_>, project_id: i64, id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM protected_environments WHERE project_id = $1 AND id = $2", project_id, id)
        .execute(e)
        .await?
        .rows_affected()
        > 0)
}
