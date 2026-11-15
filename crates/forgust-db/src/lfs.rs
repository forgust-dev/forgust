use sqlx::{PgConnection, PgExecutor};

use crate::Result;

/// Register an object (idempotent) and link it to the project; an object the
/// sweep had marked unused is in use again.
pub async fn link(e: impl PgExecutor<'_>, project_id: i64, oid: &str, size: i64) -> Result<()> {
    sqlx::query!(
        "WITH o AS (INSERT INTO lfs_objects (oid, size) VALUES ($2, $3)
                    ON CONFLICT (oid) DO UPDATE SET unused_since = NULL WHERE lfs_objects.unused_since IS NOT NULL)
         INSERT INTO lfs_objects_projects (project_id, oid)
         SELECT $1, $2 ON CONFLICT DO NOTHING",
        project_id,
        oid,
        size
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Size of an object linked to the project.
/// [`linked_size`] for several objects in one query: `(oid, size)` of those
/// the project has.
pub async fn linked_sizes(e: impl PgExecutor<'_>, project_id: i64, oids: &[String]) -> Result<Vec<(String, i64)>> {
    Ok(sqlx::query!(
        "SELECT o.oid, o.size FROM lfs_objects o JOIN lfs_objects_projects l ON l.oid = o.oid
         WHERE l.project_id = $1 AND o.oid = ANY($2)",
        project_id,
        oids
    )
    .fetch_all(e)
    .await?
    .into_iter()
    .map(|r| (r.oid, r.size))
    .collect())
}

pub async fn linked_size(e: impl PgExecutor<'_>, project_id: i64, oid: &str) -> Result<Option<i64>> {
    sqlx::query_scalar!(
        "SELECT o.size FROM lfs_objects o JOIN lfs_objects_projects l ON l.oid = o.oid
         WHERE l.project_id = $1 AND o.oid = $2",
        project_id,
        oid
    )
    .fetch_optional(e)
    .await
}

pub async fn project_usage(e: impl PgExecutor<'_>, project_id: i64) -> Result<(i64, i64)> {
    let r = sqlx::query!(
        r#"SELECT count(*) AS "n!", coalesce(sum(o.size), 0)::bigint AS "bytes!" FROM lfs_objects o
           JOIN lfs_objects_projects l ON l.oid = o.oid WHERE l.project_id = $1"#,
        project_id
    )
    .fetch_one(e)
    .await?;
    Ok((r.n, r.bytes))
}

/// One object at a time for what stores its file and what deletes it (until the
/// transaction ends): an upload and the sweep of unused objects must not cross.
pub async fn lock(c: &mut PgConnection, oid: &str) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))").bind(format!("lfs-object:{oid}")).execute(&mut *c).await?;
    Ok(())
}

/// Mark the objects no project uses (any more), and unmark those used again.
pub async fn mark_unused(c: &mut PgConnection) -> Result<()> {
    sqlx::query!(
        "UPDATE lfs_objects o SET unused_since = now()
          WHERE o.unused_since IS NULL AND NOT EXISTS (SELECT 1 FROM lfs_objects_projects l WHERE l.oid = o.oid)"
    )
    .execute(&mut *c)
    .await?;
    sqlx::query!(
        "UPDATE lfs_objects o SET unused_since = NULL
          WHERE o.unused_since IS NOT NULL AND EXISTS (SELECT 1 FROM lfs_objects_projects l WHERE l.oid = o.oid)"
    )
    .execute(&mut *c)
    .await?;
    Ok(())
}

/// Objects marked unused for at least `seconds`.
pub async fn unused_for(e: impl PgExecutor<'_>, seconds: i64, limit: i64) -> Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT oid FROM lfs_objects WHERE unused_since <= now() - make_interval(secs => $1::bigint::double precision)
          ORDER BY unused_since LIMIT $2",
        seconds,
        limit
    )
    .fetch_all(e)
    .await
}

/// Delete an object's record if no project uses it; call with [`lock`] held.
pub async fn delete_unused(e: impl PgExecutor<'_>, oid: &str) -> Result<bool> {
    Ok(sqlx::query!(
        "DELETE FROM lfs_objects o WHERE o.oid = $1 AND o.unused_since IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM lfs_objects_projects l WHERE l.oid = o.oid)",
        oid
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}
