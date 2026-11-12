use sqlx::PgExecutor;

use crate::Result;

/// Register an object (idempotent) and link it to the project.
pub async fn link(e: impl PgExecutor<'_>, project_id: i64, oid: &str, size: i64) -> Result<()> {
    sqlx::query!(
        "WITH o AS (INSERT INTO lfs_objects (oid, size) VALUES ($2, $3) ON CONFLICT (oid) DO NOTHING)
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
