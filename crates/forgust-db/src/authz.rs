//! Reads of the precomputed effective-access tables (ADR-0006).

use sqlx::PgExecutor;

use crate::Result;

pub async fn project_level(e: impl PgExecutor<'_>, user_id: i64, project_id: i64) -> Result<Option<i16>> {
    sqlx::query_scalar!("SELECT access_level FROM project_authorizations WHERE user_id = $1 AND project_id = $2", user_id, project_id)
        .fetch_optional(e)
        .await
}

/// A user's level on a project, and whether its organisation (`root_id`)
/// requires two-factor authentication: one round trip for both.
pub async fn project_level_and_2fa(e: impl PgExecutor<'_>, user_id: i64, project_id: i64, root_id: i64) -> Result<(Option<i16>, bool)> {
    let r = sqlx::query!(
        r#"SELECT (SELECT access_level FROM project_authorizations WHERE user_id = $1 AND project_id = $2) AS level,
                  coalesce((SELECT require_two_factor FROM namespaces WHERE id = $3), false) AS "require_2fa!""#,
        user_id,
        project_id,
        root_id
    )
    .fetch_one(e)
    .await?;
    Ok((r.level, r.require_2fa))
}

/// [`project_level_and_2fa`] for a group or user namespace.
pub async fn namespace_level_and_2fa(e: impl PgExecutor<'_>, user_id: i64, namespace_id: i64, root_id: i64) -> Result<(Option<i16>, bool)> {
    let r = sqlx::query!(
        r#"SELECT (SELECT access_level FROM namespace_authorizations WHERE user_id = $1 AND namespace_id = $2) AS level,
                  coalesce((SELECT require_two_factor FROM namespaces WHERE id = $3), false) AS "require_2fa!""#,
        user_id,
        namespace_id,
        root_id
    )
    .fetch_one(e)
    .await?;
    Ok((r.level, r.require_2fa))
}

pub async fn namespace_level(e: impl PgExecutor<'_>, user_id: i64, namespace_id: i64) -> Result<Option<i16>> {
    sqlx::query_scalar!("SELECT access_level FROM namespace_authorizations WHERE user_id = $1 AND namespace_id = $2", user_id, namespace_id)
        .fetch_optional(e)
        .await
}

/// True when the user can see anything inside the namespace subtree (a member of
/// a descendant group or project). Lets members of a subgroup see its parents.
pub async fn has_descendant_access(e: impl PgExecutor<'_>, user_id: i64, namespace_id: i64) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT (EXISTS (SELECT 1 FROM namespaces d JOIN namespace_authorizations a ON a.namespace_id = d.id
                           WHERE d.traversal_ids @> ARRAY[$2::bigint] AND a.user_id = $1)
                OR EXISTS (SELECT 1 FROM projects p JOIN namespaces d ON d.id = p.namespace_id
                           JOIN project_authorizations a ON a.project_id = p.id
                           WHERE d.traversal_ids @> ARRAY[$2::bigint] AND a.user_id = $1)) AS "x!""#,
        user_id,
        namespace_id
    )
    .fetch_one(e)
    .await
}
