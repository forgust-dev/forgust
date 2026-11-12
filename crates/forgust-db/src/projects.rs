use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Project {
    pub id: i64,
    pub namespace_id: i64,
    pub name: String,
    pub path: String,
    pub full_path: String,
    pub description: String,
    pub visibility: i16,
    pub default_branch: Option<String>,
    pub ref_format: String,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    /// Ancestors of the project's namespace, root first.
    pub traversal_ids: Vec<i64>,
    /// When its avatar was set (None: no avatar).
    pub avatar_at: Option<DateTime<Utc>>,
    /// When it was archived (read-only); None: not archived.
    pub archived_at: Option<DateTime<Utc>>,
}

impl Project {
    pub fn archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

pub struct NewProject<'a> {
    pub namespace_id: i64,
    pub name: &'a str,
    pub path: &'a str,
    pub description: &'a str,
    pub visibility: i16,
    pub default_branch: &'a str,
    pub ref_format: &'a str,
}

pub async fn insert(e: impl PgExecutor<'_>, p: NewProject<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO projects (namespace_id, name, path, description, visibility, default_branch, ref_format)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        p.namespace_id,
        p.name,
        p.path,
        p.description,
        p.visibility,
        p.default_branch,
        p.ref_format
    )
    .fetch_one(e)
    .await
}

/// A project by its current path, in one query (a renamed path's redirect is
/// `routes::lookup`'s).
pub async fn find_by_path(e: impl PgExecutor<'_>, path: &str) -> Result<Option<Project>> {
    sqlx::query_as!(
        Project,
        r#"SELECT p.id, p.namespace_id, p.name, p.path, r.path AS "full_path!", p.description, p.visibility,
                  p.default_branch, p.ref_format, p.created_at, p.last_activity_at, n.traversal_ids, p.avatar_at, p.archived_at
           FROM routes r
           JOIN projects p ON p.id = r.source_id
           JOIN namespaces n ON n.id = p.namespace_id
           WHERE r.source_type = 'project' AND lower(r.path) = lower($1)"#,
        path
    )
    .fetch_optional(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Project>> {
    sqlx::query_as!(
        Project,
        r#"SELECT p.id, p.namespace_id, p.name, p.path, r.path AS "full_path!", p.description, p.visibility,
                  p.default_branch, p.ref_format, p.created_at, p.last_activity_at, n.traversal_ids, p.avatar_at, p.archived_at
           FROM projects p
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
           JOIN namespaces n ON n.id = p.namespace_id
           WHERE p.id = $1"#,
        id
    )
    .fetch_optional(e)
    .await
}

/// Projects directly in a namespace that the viewer may see.
pub async fn in_namespace_visible(
    e: impl PgExecutor<'_>,
    ns: i64,
    user_id: Option<i64>,
    is_admin: bool,
    signed_in: bool,
) -> Result<Vec<Project>> {
    sqlx::query_as!(
        Project,
        r#"SELECT p.id, p.namespace_id, p.name, p.path, r.path AS "full_path!", p.description, p.visibility,
                  p.default_branch, p.ref_format, p.created_at, p.last_activity_at, n.traversal_ids, p.avatar_at, p.archived_at
           FROM projects p
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
           JOIN namespaces n ON n.id = p.namespace_id
           WHERE p.namespace_id = $1
             AND ($3 OR p.visibility = 20 OR (p.visibility = 10 AND $4)
                  OR EXISTS (SELECT 1 FROM listed_project_authorizations a WHERE a.project_id = p.id AND a.user_id = $2))
           ORDER BY p.last_activity_at DESC"#,
        ns,
        user_id,
        is_admin,
        signed_in
    )
    .fetch_all(e)
    .await
}

/// Projects the user is a member of (directly or through groups), most active first.
pub async fn for_user(e: impl PgExecutor<'_>, user_id: i64, search: &str, limit: i64, offset: i64) -> Result<Vec<(Project, i16)>> {
    let rows = sqlx::query!(
        r#"SELECT p.id, p.namespace_id, p.name, p.path, r.path AS "full_path!", p.description, p.visibility,
                  p.default_branch, p.ref_format, p.created_at, p.last_activity_at, n.traversal_ids, p.avatar_at, p.archived_at, a.access_level AS "access_level!"
           FROM listed_project_authorizations a
           JOIN projects p ON p.id = a.project_id
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
           JOIN namespaces n ON n.id = p.namespace_id
           WHERE a.user_id = $1 AND ($2 = '' OR r.path ILIKE '%' || $2 || '%')
           ORDER BY p.last_activity_at DESC LIMIT $3 OFFSET $4"#,
        user_id,
        search,
        limit,
        offset
    )
    .fetch_all(e)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                Project {
                    id: r.id,
                    namespace_id: r.namespace_id,
                    name: r.name,
                    path: r.path,
                    full_path: r.full_path,
                    description: r.description,
                    visibility: r.visibility,
                    default_branch: r.default_branch,
                    ref_format: r.ref_format,
                    created_at: r.created_at,
                    last_activity_at: r.last_activity_at,
                    traversal_ids: r.traversal_ids,
                    avatar_at: r.avatar_at,
                    archived_at: r.archived_at,
                },
                r.access_level,
            )
        })
        .collect())
}

/// Public and internal projects (explore page).
pub async fn explore(e: impl PgExecutor<'_>, signed_in: bool, search: &str, limit: i64, offset: i64) -> Result<Vec<Project>> {
    sqlx::query_as!(
        Project,
        r#"SELECT p.id, p.namespace_id, p.name, p.path, r.path AS "full_path!", p.description, p.visibility,
                  p.default_branch, p.ref_format, p.created_at, p.last_activity_at, n.traversal_ids, p.avatar_at, p.archived_at
           FROM projects p
           JOIN routes r ON r.source_type = 'project' AND r.source_id = p.id
           JOIN namespaces n ON n.id = p.namespace_id
           WHERE (p.visibility = 20 OR (p.visibility = 10 AND $1)) AND ($2 = '' OR r.path ILIKE '%' || $2 || '%')
           ORDER BY p.last_activity_at DESC LIMIT $3 OFFSET $4"#,
        signed_in,
        search,
        limit,
        offset
    )
    .fetch_all(e)
    .await
}

pub async fn all_ids(e: impl PgExecutor<'_>) -> Result<Vec<(i64, Option<String>, String)>> {
    let r = sqlx::query!("SELECT id, default_branch, ref_format FROM projects ORDER BY id").fetch_all(e).await?;
    Ok(r.into_iter().map(|r| (r.id, r.default_branch, r.ref_format)).collect())
}

pub async fn update_details(e: impl PgExecutor<'_>, id: i64, name: &str, description: &str, visibility: i16) -> Result<()> {
    sqlx::query!(
        "UPDATE projects SET name = $2, description = $3, visibility = $4, updated_at = now() WHERE id = $1",
        id,
        name,
        description,
        visibility
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_default_branch(e: impl PgExecutor<'_>, id: i64, branch: &str) -> Result<()> {
    sqlx::query!("UPDATE projects SET default_branch = $2, updated_at = now() WHERE id = $1", id, branch).execute(e).await?;
    Ok(())
}

pub async fn set_path(e: impl PgExecutor<'_>, id: i64, path: &str) -> Result<()> {
    sqlx::query!("UPDATE projects SET path = $2, updated_at = now() WHERE id = $1", id, path).execute(e).await?;
    Ok(())
}

pub async fn set_namespace(e: impl PgExecutor<'_>, id: i64, namespace_id: i64) -> Result<()> {
    sqlx::query!("UPDATE projects SET namespace_id = $2, updated_at = now() WHERE id = $1", id, namespace_id).execute(e).await?;
    Ok(())
}

pub async fn touch_activity(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("UPDATE projects SET last_activity_at = now() WHERE id = $1", id).execute(e).await?;
    Ok(())
}

pub async fn delete(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM projects WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// Mark the avatar as set now, or removed.
pub async fn set_avatar(e: impl PgExecutor<'_>, id: i64, present: bool) -> Result<()> {
    sqlx::query!("UPDATE projects SET avatar_at = CASE WHEN $2 THEN now() END WHERE id = $1", id, present).execute(e).await?;
    Ok(())
}

/// Archive (`true`) or unarchive a project; when it was archived.
pub async fn set_archived(e: impl PgExecutor<'_>, id: i64, archived: bool) -> Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar!(
        "UPDATE projects SET archived_at = CASE WHEN $2 THEN coalesce(archived_at, now()) END, updated_at = now() WHERE id = $1 RETURNING archived_at",
        id,
        archived
    )
    .fetch_one(e)
    .await
}
