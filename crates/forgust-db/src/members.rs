use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Member {
    pub id: i64,
    pub source_type: String,
    pub source_id: i64,
    pub user_id: i64,
    pub username: String,
    pub name: String,
    pub user_type: String,
    pub access_level: i16,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Insert or update a membership.
pub async fn upsert(
    e: impl PgExecutor<'_>,
    source_type: &str,
    source_id: i64,
    user_id: i64,
    level: i16,
    expires_at: Option<DateTime<Utc>>,
    created_by: Option<i64>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO members (source_type, source_id, user_id, access_level, expires_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (source_type, source_id, user_id)
         DO UPDATE SET access_level = EXCLUDED.access_level, expires_at = EXCLUDED.expires_at",
        source_type,
        source_id,
        user_id,
        level,
        expires_at,
        created_by
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn remove(e: impl PgExecutor<'_>, source_type: &str, source_id: i64, user_id: i64) -> Result<bool> {
    let r = sqlx::query!("DELETE FROM members WHERE source_type = $1 AND source_id = $2 AND user_id = $3", source_type, source_id, user_id)
        .execute(e)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn list(e: impl PgExecutor<'_>, source_type: &str, source_id: i64) -> Result<Vec<Member>> {
    sqlx::query_as!(
        Member,
        "SELECT m.id, m.source_type, m.source_id, m.user_id, u.username, u.name, u.user_type, m.access_level,
                m.expires_at, m.created_at
         FROM members m JOIN users u ON u.id = m.user_id
         WHERE m.source_type = $1 AND m.source_id = $2
         ORDER BY m.access_level DESC, u.username",
        source_type,
        source_id
    )
    .fetch_all(e)
    .await
}

pub async fn count_owners(e: impl PgExecutor<'_>, source_type: &str, source_id: i64) -> Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM members WHERE source_type = $1 AND source_id = $2 AND access_level = 50"#,
        source_type,
        source_id
    )
    .fetch_one(e)
    .await
}

pub async fn level_of(e: impl PgExecutor<'_>, source_type: &str, source_id: i64, user_id: i64) -> Result<Option<i16>> {
    sqlx::query_scalar!(
        "SELECT access_level FROM members WHERE source_type = $1 AND source_id = $2 AND user_id = $3",
        source_type,
        source_id,
        user_id
    )
    .fetch_optional(e)
    .await
}

/// Memberships and shares past their `expires_at` go, and the access of
/// whoever they gave it to is recomputed (effective access is materialised, so
/// an expiry does nothing by itself). Returns the users whose access changed.
pub async fn expire(conn: &mut PgConnection) -> Result<Vec<i64>> {
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    let mut users: Vec<i64> =
        sqlx::query_scalar!("DELETE FROM members WHERE expires_at <= now() RETURNING user_id").fetch_all(&mut *tx).await?;
    // a share's invited group: its members and those of its ancestors
    users.extend(
        sqlx::query_scalar!(
            r#"WITH a AS (DELETE FROM group_group_links WHERE expires_at <= now() RETURNING shared_with_group_id AS id),
                    b AS (DELETE FROM project_group_links WHERE expires_at <= now() RETURNING group_id AS id),
                    g AS (SELECT id FROM a UNION SELECT id FROM b)
               SELECT DISTINCT m.user_id AS "user_id!" FROM g JOIN namespaces n ON n.id = g.id
               JOIN members m ON m.source_type = 'namespace' AND m.source_id = ANY (n.traversal_ids)"#
        )
        .fetch_all(&mut *tx)
        .await?,
    );
    users.sort_unstable();
    users.dedup();
    refresh_authorizations(&mut tx, &users).await?;
    tx.commit().await?;
    Ok(users)
}

/// Recompute effective access for these users (ADR-0006).
pub async fn refresh_authorizations(conn: &mut PgConnection, user_ids: &[i64]) -> Result<()> {
    if user_ids.is_empty() {
        return Ok(());
    }
    sqlx::query!("SELECT refresh_authorizations($1)", user_ids).execute(conn).await?;
    Ok(())
}

/// Users whose access may change when the tree under `namespace_id` changes.
pub async fn affected_by_namespace(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(r#"SELECT users_affected_by_namespace($1) AS "ids!""#, namespace_id).fetch_one(e).await
}

/// Users who are members of a project directly.
pub async fn project_member_ids(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!("SELECT user_id FROM members WHERE source_type = 'project' AND source_id = $1", project_id).fetch_all(e).await
}

/// Members of a group (and its ancestors), used when a share link changes.
pub async fn group_member_ids_with_ancestors(e: impl PgExecutor<'_>, group_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        "SELECT DISTINCT m.user_id FROM members m JOIN namespaces g ON g.id = $1
         WHERE m.source_type = 'namespace' AND m.source_id = ANY(g.traversal_ids)",
        group_id
    )
    .fetch_all(e)
    .await
}

pub async fn remove_all_for_source(e: impl PgExecutor<'_>, source_type: &str, source_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!("DELETE FROM members WHERE source_type = $1 AND source_id = $2 RETURNING user_id", source_type, source_id)
        .fetch_all(e)
        .await
}
