//! The Forgust bot (ADR-0026): which projects turned it on, the groups'
//! defaults, and the merge requests it holds.

use sqlx::PgExecutor;

use crate::Result;

/// The project's choice (None: it never chose).
pub async fn enabled(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<bool>> {
    sqlx::query_scalar!("SELECT enabled FROM bot_settings WHERE project_id = $1", project_id).fetch_optional(e).await
}

pub async fn set_enabled(e: impl PgExecutor<'_>, project_id: i64, enabled: bool, user_id: Option<i64>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO bot_settings (project_id, enabled, user_id) VALUES ($1, $2, $3)
         ON CONFLICT (project_id) DO UPDATE SET enabled = $2, user_id = $3, updated_at = now()",
        project_id,
        enabled,
        user_id
    )
    .execute(e)
    .await?;
    Ok(())
}

// ------------------------------------------------------------ group defaults

pub async fn group_default(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<bool> {
    Ok(sqlx::query_scalar!("SELECT enabled FROM bot_group_defaults WHERE namespace_id = $1", namespace_id)
        .fetch_optional(e)
        .await?
        .unwrap_or(false))
}

pub async fn set_group_default(e: impl PgExecutor<'_>, namespace_id: i64, enabled: bool) -> Result<()> {
    sqlx::query!(
        "INSERT INTO bot_group_defaults (namespace_id, enabled) VALUES ($1, $2)
         ON CONFLICT (namespace_id) DO UPDATE SET enabled = $2, updated_at = now()",
        namespace_id,
        enabled
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Whether a group above `traversal_ids` (the project's namespace path) turned
/// the bot on for its projects.
pub async fn inherited_default(e: impl PgExecutor<'_>, traversal_ids: &[i64]) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM bot_group_defaults WHERE enabled AND namespace_id = ANY ($1)) AS "x!""#,
        traversal_ids
    )
    .fetch_one(e)
    .await
}

/// Projects under a group that never chose (no settings yet).
pub async fn undecided_projects_under(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT p.id FROM projects p JOIN namespaces n ON n.id = p.namespace_id
          WHERE $1 = ANY (n.traversal_ids) AND NOT EXISTS (SELECT 1 FROM bot_settings s WHERE s.project_id = p.id)
          ORDER BY p.id",
        namespace_id
    )
    .fetch_all(e)
    .await
}

// --------------------------------------------------------------------- holds

/// Who put the merge request on hold (their username), when it is; a hold
/// whose author is gone reads `Some(None)`.
pub async fn held_by(e: impl PgExecutor<'_>, merge_request_id: i64) -> Result<Option<Option<String>>> {
    sqlx::query_scalar!(
        r#"SELECT u.username AS "username?" FROM merge_request_holds h LEFT JOIN users u ON u.id = h.user_id
            WHERE h.merge_request_id = $1"#,
        merge_request_id
    )
    .fetch_optional(e)
    .await
}

/// Put on hold; false when it already was.
pub async fn hold(e: impl PgExecutor<'_>, merge_request_id: i64, user_id: i64) -> Result<bool> {
    let n = sqlx::query!(
        "INSERT INTO merge_request_holds (merge_request_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        merge_request_id,
        user_id
    )
    .execute(e)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Lift the hold; false when there was none.
pub async fn unhold(e: impl PgExecutor<'_>, merge_request_id: i64) -> Result<bool> {
    let n = sqlx::query!("DELETE FROM merge_request_holds WHERE merge_request_id = $1", merge_request_id).execute(e).await?.rows_affected();
    Ok(n > 0)
}

// --------------------------------------------------------------- auto-labels

/// The labels the bot's rules asked for when it last looked at the merge request.
pub async fn auto_labels(e: impl PgExecutor<'_>, merge_request_id: i64) -> Result<Vec<String>> {
    sqlx::query_scalar!("SELECT name FROM merge_request_auto_labels WHERE merge_request_id = $1 ORDER BY name", merge_request_id)
        .fetch_all(e)
        .await
}

pub async fn set_auto_labels(conn: &mut sqlx::PgConnection, merge_request_id: i64, names: &[String]) -> Result<()> {
    sqlx::query!("DELETE FROM merge_request_auto_labels WHERE merge_request_id = $1 AND name <> ALL ($2)", merge_request_id, names)
        .execute(&mut *conn)
        .await?;
    sqlx::query!(
        "INSERT INTO merge_request_auto_labels (merge_request_id, name) SELECT $1, n FROM unnest($2::text[]) AS n ON CONFLICT DO NOTHING",
        merge_request_id,
        names
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ----------------------------------------------------------------- reviewers

/// Mark the merge request as looked at for reviewers; false when it already was.
pub async fn claim_review(e: impl PgExecutor<'_>, merge_request_id: i64) -> Result<bool> {
    let n = sqlx::query!("INSERT INTO merge_request_bot_reviews (merge_request_id) VALUES ($1) ON CONFLICT DO NOTHING", merge_request_id)
        .execute(e)
        .await?
        .rows_affected();
    Ok(n > 0)
}

/// When the bot last chose each of `user_ids` in the project (those never chosen are left out).
pub async fn review_turns(e: impl PgExecutor<'_>, project_id: i64, user_ids: &[i64]) -> Result<Vec<(i64, chrono::DateTime<chrono::Utc>)>> {
    Ok(sqlx::query!("SELECT user_id, picked_at FROM bot_review_turns WHERE project_id = $1 AND user_id = ANY ($2)", project_id, user_ids)
        .fetch_all(e)
        .await?
        .into_iter()
        .map(|r| (r.user_id, r.picked_at))
        .collect())
}

pub async fn took_review_turn(e: impl PgExecutor<'_>, project_id: i64, user_ids: &[i64]) -> Result<()> {
    sqlx::query!(
        "INSERT INTO bot_review_turns (project_id, user_id, picked_at) SELECT $1, u, clock_timestamp() FROM unnest($2::bigint[]) AS u
         ON CONFLICT (project_id, user_id) DO UPDATE SET picked_at = clock_timestamp()",
        project_id,
        user_ids
    )
    .execute(e)
    .await?;
    Ok(())
}

/// How many open merge requests of the project each of `user_ids` is asked to
/// review (those with none are left out).
pub async fn review_loads(e: impl PgExecutor<'_>, project_id: i64, user_ids: &[i64]) -> Result<Vec<(i64, i64)>> {
    Ok(sqlx::query!(
        r#"SELECT r.user_id, count(*) AS "n!" FROM merge_request_reviewers r JOIN merge_requests m ON m.id = r.merge_request_id
            WHERE m.project_id = $1 AND m.state = 'opened' AND r.user_id = ANY ($2) GROUP BY r.user_id"#,
        project_id,
        user_ids
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.user_id, r.n))
    .collect())
}
