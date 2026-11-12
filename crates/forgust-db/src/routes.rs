use sqlx::{PgConnection, PgExecutor};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteHit {
    pub source_type: String,
    pub source_id: i64,
    /// The current path (differs from the request when it came from a redirect).
    pub path: String,
    pub redirected: bool,
}

pub async fn insert(e: impl PgExecutor<'_>, source_type: &str, source_id: i64, path: &str) -> Result<()> {
    sqlx::query!("INSERT INTO routes (source_type, source_id, path) VALUES ($1, $2, $3)", source_type, source_id, path).execute(e).await?;
    Ok(())
}

pub async fn path_taken(e: impl PgExecutor<'_>, path: &str) -> Result<bool> {
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM routes WHERE lower(path) = lower($1)) AS "x!""#, path).fetch_one(e).await
}

/// Resolve a full path: current routes first, then redirects from renames/transfers.
pub async fn lookup(conn: &mut PgConnection, path: &str) -> Result<Option<RouteHit>> {
    if let Some(r) = sqlx::query!("SELECT source_type, source_id, path FROM routes WHERE lower(path) = lower($1)", path)
        .fetch_optional(&mut *conn)
        .await?
    {
        return Ok(Some(RouteHit { source_type: r.source_type, source_id: r.source_id, path: r.path, redirected: false }));
    }
    let r = sqlx::query!(
        "SELECT r.source_type, r.source_id, cur.path AS current
         FROM redirect_routes r
         JOIN routes cur ON cur.source_type = r.source_type AND cur.source_id = r.source_id
         WHERE lower(r.path) = lower($1)",
        path
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(r.map(|r| RouteHit { source_type: r.source_type, source_id: r.source_id, path: r.current, redirected: true }))
}

/// Move every route at or under `old` to `new`, keeping the old paths as redirects.
pub async fn move_prefix(conn: &mut PgConnection, old: &str, new: &str) -> Result<()> {
    sqlx::query!(
        "INSERT INTO redirect_routes (source_type, source_id, path)
         SELECT source_type, source_id, path FROM routes
         WHERE lower(path) = lower($1) OR starts_with(lower(path), lower($1) || '/')
         ON CONFLICT (lower(path)) DO UPDATE SET source_type = EXCLUDED.source_type, source_id = EXCLUDED.source_id",
        old
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE routes SET path = $2 || substr(path, length($1) + 1)
         WHERE lower(path) = lower($1) OR starts_with(lower(path), lower($1) || '/')",
        old,
        new
    )
    .execute(&mut *conn)
    .await?;
    // a path that is live again must not also redirect elsewhere
    sqlx::query!("DELETE FROM redirect_routes rr USING routes r WHERE lower(rr.path) = lower(r.path)").execute(&mut *conn).await?;
    Ok(())
}

pub async fn delete_for(conn: &mut PgConnection, source_type: &str, source_id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM routes WHERE source_type = $1 AND source_id = $2", source_type, source_id).execute(&mut *conn).await?;
    sqlx::query!("DELETE FROM redirect_routes WHERE source_type = $1 AND source_id = $2", source_type, source_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn redirects_for(e: impl PgExecutor<'_>, source_type: &str, source_id: i64) -> Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT path FROM redirect_routes WHERE source_type = $1 AND source_id = $2 ORDER BY created_at DESC",
        source_type,
        source_id
    )
    .fetch_all(e)
    .await
}
