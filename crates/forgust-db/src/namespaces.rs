use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Namespace {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub full_path: String,
    pub parent_id: Option<i64>,
    pub traversal_ids: Vec<i64>,
    pub visibility: i16,
    pub description: String,
    pub owner_id: Option<i64>,
    pub max_visibility: Option<i16>,
    pub require_two_factor: bool,
    pub project_creation_level: i16,
    pub subgroup_creation_level: i16,
    pub created_at: DateTime<Utc>,
    /// When its avatar was set (None: no avatar).
    pub avatar_at: Option<DateTime<Utc>>,
}

impl Namespace {
    pub fn root_id(&self) -> i64 {
        self.traversal_ids.first().copied().unwrap_or(self.id)
    }
    pub fn is_group(&self) -> bool {
        self.kind == "group"
    }
    pub fn depth(&self) -> usize {
        self.traversal_ids.len()
    }
}

pub struct NewNamespace<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub path: &'a str,
    pub parent_id: Option<i64>,
    pub visibility: i16,
    pub description: &'a str,
    pub owner_id: Option<i64>,
}

/// Insert a namespace and fill its `traversal_ids` (ancestors + itself).
pub async fn insert(conn: &mut PgConnection, n: NewNamespace<'_>) -> Result<i64> {
    let id = sqlx::query_scalar!(
        "INSERT INTO namespaces (kind, name, path, parent_id, visibility, description, owner_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        n.kind,
        n.name,
        n.path,
        n.parent_id,
        n.visibility,
        n.description,
        n.owner_id
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE namespaces SET traversal_ids =
            coalesce((SELECT traversal_ids FROM namespaces WHERE id = $2), '{}') || id
         WHERE id = $1",
        id,
        n.parent_id
    )
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

pub async fn find(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Namespace>> {
    sqlx::query_as!(
        Namespace,
        r#"SELECT n.id, n.kind, n.name, n.path, r.path AS "full_path!", n.parent_id, n.traversal_ids, n.visibility,
                  n.description, n.owner_id, n.max_visibility, n.require_two_factor, n.project_creation_level,
                  n.subgroup_creation_level, n.created_at, n.avatar_at
           FROM namespaces n JOIN routes r ON r.source_type = 'namespace' AND r.source_id = n.id
           WHERE n.id = $1"#,
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn find_many(e: impl PgExecutor<'_>, ids: &[i64]) -> Result<Vec<Namespace>> {
    sqlx::query_as!(
        Namespace,
        r#"SELECT n.id, n.kind, n.name, n.path, r.path AS "full_path!", n.parent_id, n.traversal_ids, n.visibility,
                  n.description, n.owner_id, n.max_visibility, n.require_two_factor, n.project_creation_level,
                  n.subgroup_creation_level, n.created_at, n.avatar_at
           FROM namespaces n JOIN routes r ON r.source_type = 'namespace' AND r.source_id = n.id
           WHERE n.id = ANY($1) ORDER BY array_length(n.traversal_ids, 1)"#,
        ids
    )
    .fetch_all(e)
    .await
}

/// Child groups of a namespace that the user may see (members, or by visibility).
pub async fn children_visible(
    e: impl PgExecutor<'_>,
    parent: i64,
    user_id: Option<i64>,
    is_admin: bool,
    signed_in: bool,
) -> Result<Vec<Namespace>> {
    sqlx::query_as!(
        Namespace,
        r#"SELECT n.id, n.kind, n.name, n.path, r.path AS "full_path!", n.parent_id, n.traversal_ids, n.visibility,
                  n.description, n.owner_id, n.max_visibility, n.require_two_factor, n.project_creation_level,
                  n.subgroup_creation_level, n.created_at, n.avatar_at
           FROM namespaces n JOIN routes r ON r.source_type = 'namespace' AND r.source_id = n.id
           WHERE n.parent_id = $1
             AND ($3 OR n.visibility = 20 OR (n.visibility = 10 AND $4)
                  OR EXISTS (SELECT 1 FROM namespace_authorizations a WHERE a.namespace_id = n.id AND a.user_id = $2)
                  OR EXISTS (SELECT 1 FROM namespaces d JOIN namespace_authorizations a ON a.namespace_id = d.id
                             WHERE d.traversal_ids @> ARRAY[n.id] AND a.user_id = $2)
                  OR EXISTS (SELECT 1 FROM projects p JOIN namespaces d ON d.id = p.namespace_id
                             JOIN project_authorizations pa ON pa.project_id = p.id
                             WHERE d.traversal_ids @> ARRAY[n.id] AND pa.user_id = $2))
           ORDER BY lower(n.name)"#,
        parent,
        user_id,
        is_admin,
        signed_in
    )
    .fetch_all(e)
    .await
}

/// Groups the user can see at the top level: their orgs, plus public/internal ones.
pub async fn top_level_visible(e: impl PgExecutor<'_>, user_id: Option<i64>, is_admin: bool, signed_in: bool) -> Result<Vec<Namespace>> {
    sqlx::query_as!(
        Namespace,
        r#"SELECT n.id, n.kind, n.name, n.path, r.path AS "full_path!", n.parent_id, n.traversal_ids, n.visibility,
                  n.description, n.owner_id, n.max_visibility, n.require_two_factor, n.project_creation_level,
                  n.subgroup_creation_level, n.created_at, n.avatar_at
           FROM namespaces n JOIN routes r ON r.source_type = 'namespace' AND r.source_id = n.id
           WHERE n.kind = 'group' AND n.parent_id IS NULL
             AND ($2 OR n.visibility = 20 OR (n.visibility = 10 AND $3)
                  OR EXISTS (SELECT 1 FROM namespaces d JOIN namespace_authorizations a ON a.namespace_id = d.id
                             WHERE d.traversal_ids @> ARRAY[n.id] AND a.user_id = $1)
                  OR EXISTS (SELECT 1 FROM projects p JOIN namespaces d ON d.id = p.namespace_id
                             JOIN project_authorizations pa ON pa.project_id = p.id
                             WHERE d.traversal_ids @> ARRAY[n.id] AND pa.user_id = $1))
           ORDER BY lower(n.name)"#,
        user_id,
        is_admin,
        signed_in
    )
    .fetch_all(e)
    .await
}

pub async fn update_details(e: impl PgExecutor<'_>, id: i64, name: &str, description: &str, visibility: i16) -> Result<()> {
    sqlx::query!(
        "UPDATE namespaces SET name = $2, description = $3, visibility = $4, updated_at = now() WHERE id = $1",
        id,
        name,
        description,
        visibility
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn update_policy(
    e: impl PgExecutor<'_>,
    id: i64,
    max_visibility: Option<i16>,
    require_two_factor: bool,
    project_creation_level: i16,
    subgroup_creation_level: i16,
) -> Result<()> {
    sqlx::query!(
        "UPDATE namespaces SET max_visibility = $2, require_two_factor = $3, project_creation_level = $4,
                subgroup_creation_level = $5, updated_at = now() WHERE id = $1",
        id,
        max_visibility,
        require_two_factor,
        project_creation_level,
        subgroup_creation_level
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn set_path(e: impl PgExecutor<'_>, id: i64, path: &str) -> Result<()> {
    sqlx::query!("UPDATE namespaces SET path = $2, updated_at = now() WHERE id = $1", id, path).execute(e).await?;
    Ok(())
}

/// Move a group (and its subtree) under a new parent, rewriting traversal ids.
pub async fn reparent(conn: &mut PgConnection, id: i64, new_parent: Option<i64>) -> Result<()> {
    sqlx::query!("UPDATE namespaces SET parent_id = $2, updated_at = now() WHERE id = $1", id, new_parent).execute(&mut *conn).await?;
    sqlx::query!(
        "UPDATE namespaces n SET traversal_ids =
             coalesce((SELECT traversal_ids FROM namespaces WHERE id = $2), '{}')
             || n.traversal_ids[array_position(n.traversal_ids, $1):]
         WHERE n.traversal_ids @> ARRAY[$1::bigint]",
        id,
        new_parent
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Height of the subtree under (and including) `id`.
pub async fn subtree_height(e: impl PgExecutor<'_>, id: i64) -> Result<i32> {
    sqlx::query_scalar!(
        r#"SELECT coalesce(max(array_length(traversal_ids, 1) - array_position(traversal_ids, $1) + 1), 1) AS "h!"
           FROM namespaces WHERE traversal_ids @> ARRAY[$1::bigint]"#,
        id
    )
    .fetch_one(e)
    .await
}

pub async fn has_children(e: impl PgExecutor<'_>, id: i64) -> Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT (EXISTS (SELECT 1 FROM namespaces WHERE parent_id = $1)
                OR EXISTS (SELECT 1 FROM projects WHERE namespace_id = $1)) AS "x!""#,
        id
    )
    .fetch_one(e)
    .await
}

pub async fn delete(e: impl PgExecutor<'_>, id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM namespaces WHERE id = $1", id).execute(e).await?;
    Ok(())
}

/// The strictest org policy for the namespace's root.
pub async fn root_policy(e: impl PgExecutor<'_>, id: i64) -> Result<Option<(Option<i16>, bool)>> {
    let r = sqlx::query!(
        "SELECT root.max_visibility, root.require_two_factor FROM namespaces n
         JOIN namespaces root ON root.id = n.traversal_ids[1] WHERE n.id = $1",
        id
    )
    .fetch_optional(e)
    .await?;
    Ok(r.map(|r| (r.max_visibility, r.require_two_factor)))
}

/// Widest visibility among descendants (groups and projects) of a namespace.
pub async fn max_descendant_visibility(e: impl PgExecutor<'_>, id: i64) -> Result<i16> {
    sqlx::query_scalar!(
        r#"SELECT greatest(
              (SELECT coalesce(max(visibility), 0) FROM namespaces WHERE traversal_ids @> ARRAY[$1::bigint] AND id <> $1),
              (SELECT coalesce(max(p.visibility), 0) FROM projects p JOIN namespaces n ON n.id = p.namespace_id
               WHERE n.traversal_ids @> ARRAY[$1::bigint])
           )::smallint AS "v!""#,
        id
    )
    .fetch_one(e)
    .await
}

/// Mark the avatar as set now, or removed.
pub async fn set_avatar(e: impl PgExecutor<'_>, id: i64, present: bool) -> Result<()> {
    sqlx::query!("UPDATE namespaces SET avatar_at = CASE WHEN $2 THEN now() END WHERE id = $1", id, present).execute(e).await?;
    Ok(())
}
