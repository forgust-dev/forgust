//! Notification levels (M7, ADR-0021).

use sqlx::PgExecutor;

use crate::Result;

pub const LEVELS: &[&str] = &["watch", "participating", "mention", "disabled"];

pub async fn global_level(e: impl PgExecutor<'_>, user_id: i64) -> Result<String> {
    sqlx::query_scalar!("SELECT notification_level FROM users WHERE id = $1", user_id).fetch_one(e).await
}

pub async fn set_global_level(e: impl PgExecutor<'_>, user_id: i64, level: &str) -> Result<()> {
    sqlx::query!("UPDATE users SET notification_level = $2 WHERE id = $1", user_id, level).execute(e).await?;
    Ok(())
}

/// The person's choice for the project (`None`: their default applies).
pub async fn project_level(e: impl PgExecutor<'_>, user_id: i64, project_id: i64) -> Result<Option<String>> {
    sqlx::query_scalar!("SELECT level FROM notification_settings WHERE user_id = $1 AND project_id = $2", user_id, project_id)
        .fetch_optional(e)
        .await
}

/// `None` goes back to the default.
pub async fn set_project_level(e: impl PgExecutor<'_>, user_id: i64, project_id: i64, level: Option<&str>) -> Result<()> {
    match level {
        Some(l) => {
            sqlx::query!(
                "INSERT INTO notification_settings (user_id, project_id, level) VALUES ($1, $2, $3)
                 ON CONFLICT (user_id, project_id) DO UPDATE SET level = $3, updated_at = now()",
                user_id,
                project_id,
                l
            )
            .execute(e)
            .await?;
        }
        None => {
            sqlx::query!("DELETE FROM notification_settings WHERE user_id = $1 AND project_id = $2", user_id, project_id)
                .execute(e)
                .await?;
        }
    }
    Ok(())
}

/// Effective level of each of these people for the project.
pub async fn levels(e: impl PgExecutor<'_>, project_id: i64, user_ids: &[i64]) -> Result<Vec<(i64, String)>> {
    Ok(sqlx::query!(
        r#"SELECT u.id, coalesce(ns.level, u.notification_level) AS "level!"
             FROM users u LEFT JOIN notification_settings ns ON ns.user_id = u.id AND ns.project_id = $1
            WHERE u.id = ANY($2)"#,
        project_id,
        user_ids
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.id, r.level))
    .collect())
}

/// Who watches the project: chose `watch` for it, or watch by default and are
/// members (a default does not reach every project of the instance). Access is
/// checked again when sending.
pub async fn watchers(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        r#"SELECT u.id AS "id!" FROM users u
             LEFT JOIN notification_settings ns ON ns.user_id = u.id AND ns.project_id = $1
            WHERE ns.level = 'watch'
               OR (ns.level IS NULL AND u.notification_level = 'watch'
                   AND EXISTS (SELECT 1 FROM project_authorizations a WHERE a.user_id = u.id AND a.project_id = $1))"#,
        project_id
    )
    .fetch_all(e)
    .await
}
