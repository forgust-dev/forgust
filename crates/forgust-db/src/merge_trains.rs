//! Merge train cars.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Car {
    pub id: i64,
    pub project_id: i64,
    pub target_branch: String,
    pub merge_request_id: i64,
    pub iid: i64,
    pub title: String,
    pub user_id: Option<i64>,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub train_sha: Option<String>,
    pub pipeline_id: Option<i64>,
    pub pipeline_status: Option<String>,
    /// Merges as one squashed commit on the target, not a merge commit.
    pub squash: bool,
    /// Lands as its own commits rebased onto what is ahead (rebase /
    /// fast-forward), not a merge commit.
    pub rebase: bool,
    /// Deletes the source branch once merged.
    pub remove_source_branch: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A train in order (the first car merges next); every train of the project
/// when `target` is None.
pub async fn cars(e: impl PgExecutor<'_>, project_id: i64, target: Option<&str>) -> Result<Vec<Car>> {
    sqlx::query_as!(
        Car,
        r#"SELECT c.id, c.project_id, c.target_branch, c.merge_request_id, m.iid, m.title, c.user_id, c.base_sha, c.head_sha,
                  c.train_sha, c.pipeline_id, p.status AS "pipeline_status?", c.squash, c.rebase, c.remove_source_branch, c.created_at, c.updated_at
             FROM merge_train_cars c JOIN merge_requests m ON m.id = c.merge_request_id
             LEFT JOIN ci_pipelines p ON p.id = c.pipeline_id
            WHERE c.project_id = $1 AND ($2::text IS NULL OR c.target_branch = $2) ORDER BY c.target_branch, c.id"#,
        project_id,
        target
    )
    .fetch_all(e)
    .await
}

pub async fn of_mr(e: impl PgExecutor<'_>, merge_request_id: i64) -> Result<Option<Car>> {
    sqlx::query_as!(
        Car,
        r#"SELECT c.id, c.project_id, c.target_branch, c.merge_request_id, m.iid, m.title, c.user_id, c.base_sha, c.head_sha,
                  c.train_sha, c.pipeline_id, p.status AS "pipeline_status?", c.squash, c.rebase, c.remove_source_branch, c.created_at, c.updated_at
             FROM merge_train_cars c JOIN merge_requests m ON m.id = c.merge_request_id
             LEFT JOIN ci_pipelines p ON p.id = c.pipeline_id
            WHERE c.merge_request_id = $1"#,
        merge_request_id
    )
    .fetch_optional(e)
    .await
}

/// The car whose pipeline this is.
pub async fn of_pipeline(e: impl PgExecutor<'_>, pipeline_id: i64) -> Result<Option<(i64, String)>> {
    Ok(sqlx::query!("SELECT project_id, target_branch FROM merge_train_cars WHERE pipeline_id = $1", pipeline_id)
        .fetch_optional(e)
        .await?
        .map(|r| (r.project_id, r.target_branch)))
}

/// Join a train (at the end); false when the MR already is on one.
/// A new car, pinned to the source head that was vetted when it was added.
/// How a car merges, chosen when it is added.
#[derive(Debug, Default, Clone, Copy)]
pub struct CarOptions {
    pub squash: bool,
    pub rebase: bool,
    pub remove_source_branch: bool,
}

pub async fn add(
    e: impl PgExecutor<'_>,
    project_id: i64,
    target: &str,
    merge_request_id: i64,
    user_id: i64,
    head_sha: &str,
    opts: CarOptions,
) -> Result<bool> {
    Ok(sqlx::query!(
        "INSERT INTO merge_train_cars (project_id, target_branch, merge_request_id, user_id, head_sha, squash, rebase, remove_source_branch)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (merge_request_id) DO NOTHING",
        project_id,
        target,
        merge_request_id,
        user_id,
        head_sha,
        opts.squash,
        opts.rebase,
        opts.remove_source_branch
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}

pub async fn remove(e: impl PgExecutor<'_>, merge_request_id: i64) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM merge_train_cars WHERE merge_request_id = $1", merge_request_id).execute(e).await?.rows_affected() > 0)
}

pub async fn rebuilt(e: impl PgExecutor<'_>, id: i64, base: &str, head: &str, train: &str, pipeline_id: Option<i64>) -> Result<()> {
    sqlx::query!(
        "UPDATE merge_train_cars SET base_sha = $2, head_sha = $3, train_sha = $4, pipeline_id = $5, updated_at = now() WHERE id = $1",
        id,
        base,
        head,
        train,
        pipeline_id
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn any_in(e: impl PgExecutor<'_>, project_id: i64) -> Result<bool> {
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM merge_train_cars WHERE project_id = $1) AS "x!""#, project_id).fetch_one(e).await
}
