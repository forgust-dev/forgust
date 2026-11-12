use sqlx::PgExecutor;

use crate::Result;

/// Count a push; returns pushes since the last maintenance.
pub async fn record_push(e: impl PgExecutor<'_>, project_id: i64) -> Result<i32> {
    sqlx::query_scalar!(
        "INSERT INTO repo_maintenance (project_id, pushes_since_maintenance, last_push_at) VALUES ($1, 1, now())
         ON CONFLICT (project_id) DO UPDATE SET pushes_since_maintenance = repo_maintenance.pushes_since_maintenance + 1,
             last_push_at = now()
         RETURNING pushes_since_maintenance",
        project_id
    )
    .fetch_one(e)
    .await
}

pub async fn record_maintenance(e: impl PgExecutor<'_>, project_id: i64) -> Result<()> {
    sqlx::query!(
        "INSERT INTO repo_maintenance (project_id, pushes_since_maintenance, last_maintenance_at) VALUES ($1, 0, now())
         ON CONFLICT (project_id) DO UPDATE SET pushes_since_maintenance = 0, last_maintenance_at = now()",
        project_id
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Repos pushed to since their last maintenance (or never maintained), oldest first.
pub async fn due(e: impl PgExecutor<'_>, limit: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT project_id FROM repo_maintenance
         WHERE pushes_since_maintenance > 0 AND (last_maintenance_at IS NULL OR last_maintenance_at < now() - interval '1 hour')
         ORDER BY last_maintenance_at NULLS FIRST LIMIT $1",
        limit
    )
    .fetch_all(e)
    .await
}
