//! Pages deployments: which artifacts archive is a project's site.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PagesDeployment {
    pub project_id: i64,
    pub build_id: i64,
    pub pipeline_iid: i64,
    pub publish: String,
    #[serde(skip)]
    pub blob_key: String,
    pub created_at: DateTime<Utc>,
}

pub async fn deployment(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<PagesDeployment>> {
    sqlx::query_as!(
        PagesDeployment,
        r#"SELECT d.project_id, d.build_id, p.iid AS pipeline_iid, d.publish, a.blob_key, d.created_at
             FROM pages_deployments d
             JOIN ci_job_artifacts a ON a.id = d.artifact_id
             JOIN ci_builds b ON b.id = d.build_id
             JOIN ci_pipelines p ON p.id = b.pipeline_id
            WHERE d.project_id = $1"#,
        project_id
    )
    .fetch_optional(e)
    .await
}

/// A project by its current path (no redirects: a site's address is its path).
/// Of these paths, the longest that is a project: `(id, path)` (one query
/// for every candidate a Pages URL could name).
pub async fn longest_project_of(e: impl PgExecutor<'_>, paths: &[String]) -> Result<Option<(i64, String)>> {
    let lower: Vec<String> = paths.iter().map(|p| p.to_lowercase()).collect();
    Ok(sqlx::query!(
        "SELECT source_id, path FROM routes WHERE source_type = 'project' AND lower(path) = ANY($1) ORDER BY length(path) DESC LIMIT 1",
        &lower
    )
    .fetch_optional(e)
    .await?
    .map(|r| (r.source_id, r.path)))
}

pub async fn project_by_path(e: impl PgExecutor<'_>, path: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!("SELECT source_id FROM routes WHERE source_type = 'project' AND lower(path) = lower($1)", path)
        .fetch_optional(e)
        .await
}

/// Make this job's archive the site, unless a newer job's already is. The
/// archive stops expiring; the one it replaces gets its expiry back.
pub async fn deploy(c: &mut PgConnection, project_id: i64, build_id: i64, artifact_id: i64, publish: &str) -> Result<bool> {
    let current = sqlx::query!(
        "SELECT build_id, artifact_id, artifact_expire_at FROM pages_deployments WHERE project_id = $1 FOR UPDATE",
        project_id
    )
    .fetch_optional(&mut *c)
    .await?;
    if let Some(cur) = &current {
        if cur.build_id > build_id {
            return Ok(false);
        }
        sqlx::query!("UPDATE ci_job_artifacts SET expire_at = $2 WHERE id = $1", cur.artifact_id, cur.artifact_expire_at)
            .execute(&mut *c)
            .await?;
    }
    let expire: Option<DateTime<Utc>> =
        sqlx::query_scalar!("SELECT expire_at FROM ci_job_artifacts WHERE id = $1", artifact_id).fetch_one(&mut *c).await?;
    sqlx::query!("UPDATE ci_job_artifacts SET expire_at = NULL WHERE id = $1", artifact_id).execute(&mut *c).await?;
    sqlx::query!(
        "INSERT INTO pages_deployments (project_id, build_id, artifact_id, publish, artifact_expire_at) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (project_id) DO UPDATE SET build_id = $2, artifact_id = $3, publish = $4, artifact_expire_at = $5, created_at = now()",
        project_id,
        build_id,
        artifact_id,
        publish,
        expire
    )
    .execute(&mut *c)
    .await?;
    Ok(true)
}

/// Take the site down; its archive gets its expiry back.
pub async fn remove(c: &mut PgConnection, project_id: i64) -> Result<bool> {
    let Some(cur) =
        sqlx::query!("DELETE FROM pages_deployments WHERE project_id = $1 RETURNING artifact_id, artifact_expire_at", project_id)
            .fetch_optional(&mut *c)
            .await?
    else {
        return Ok(false);
    };
    sqlx::query!("UPDATE ci_job_artifacts SET expire_at = $2 WHERE id = $1", cur.artifact_id, cur.artifact_expire_at)
        .execute(&mut *c)
        .await?;
    Ok(true)
}
