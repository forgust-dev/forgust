//! Storage quotas per top-level namespace (group or personal) and per
//! project. What counts is what `project_storage_bytes` adds up (the same as
//! Admin > Storage per project): repository, LFS objects and image layers it
//! uses, CI artifacts and logs, uploads.

use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

/// The quotas that apply to a project, and where they come from.
#[derive(Debug, Clone)]
pub struct Limits {
    pub project_quota: Option<i64>,
    /// Its top-level namespace (group or personal).
    pub root_id: i64,
    pub root_path: String,
    pub root_kind: String,
    /// In force for the namespace: its own, or the instance's default.
    pub namespace_quota: Option<i64>,
}

pub async fn limits(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<Limits>> {
    let r = sqlx::query!(
        r#"SELECT nullif(p.quota_bytes, 0) AS project_quota, root.id AS "root_id!", root.path AS "root_path!", root.kind AS "root_kind!",
                  nullif(coalesce(root.quota_bytes, s.namespace_quota_bytes), 0) AS namespace_quota
             FROM projects p
             JOIN namespaces n ON n.id = p.namespace_id
             JOIN namespaces root ON root.id = n.traversal_ids[1]
            CROSS JOIN instance_settings s
            WHERE p.id = $1"#,
        project_id
    )
    .fetch_optional(e)
    .await?;
    Ok(r.map(|r| Limits {
        project_quota: r.project_quota,
        root_id: r.root_id,
        root_path: r.root_path,
        root_kind: r.root_kind,
        namespace_quota: r.namespace_quota,
    }))
}

/// What a project keeps, in bytes.
pub async fn project_used(e: impl PgExecutor<'_>, project_id: i64) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT project_storage_bytes($1) AS "b!""#, project_id).fetch_one(e).await
}

/// What every project of a top-level namespace (subgroups included) keeps.
pub async fn namespace_used(e: impl PgExecutor<'_>, root_id: i64) -> Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT coalesce(sum(project_storage_bytes(p.id)), 0)::bigint AS "b!"
             FROM projects p JOIN namespaces n ON n.id = p.namespace_id
            WHERE n.traversal_ids @> ARRAY[$1::bigint]"#,
        root_id
    )
    .fetch_one(e)
    .await
}

/// The instance's default for top-level namespaces (None: no quota).
pub async fn default_quota(e: impl PgExecutor<'_>) -> Result<Option<i64>> {
    sqlx::query_scalar!("SELECT nullif(namespace_quota_bytes, 0) FROM instance_settings").fetch_one(e).await
}

pub async fn set_default_quota(e: impl PgExecutor<'_>, bytes: Option<i64>) -> Result<()> {
    sqlx::query!("UPDATE instance_settings SET namespace_quota_bytes = $1, updated_at = now()", bytes.unwrap_or(0)).execute(e).await?;
    Ok(())
}

/// A top-level namespace's own quota: None for the instance's default, Some(0)
/// for none at all.
pub async fn set_namespace_quota(e: impl PgExecutor<'_>, namespace_id: i64, bytes: Option<i64>) -> Result<()> {
    sqlx::query!("UPDATE namespaces SET quota_bytes = $2, updated_at = now() WHERE id = $1 AND parent_id IS NULL", namespace_id, bytes)
        .execute(e)
        .await?;
    Ok(())
}

pub async fn namespace_own_quota(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!("SELECT quota_bytes FROM namespaces WHERE id = $1", namespace_id).fetch_optional(e).await?.flatten())
}

/// None or Some(0): no quota of its own.
pub async fn set_project_quota(e: impl PgExecutor<'_>, project_id: i64, bytes: Option<i64>) -> Result<()> {
    sqlx::query!("UPDATE projects SET quota_bytes = $2, updated_at = now() WHERE id = $1", project_id, bytes).execute(e).await?;
    Ok(())
}

/// A namespace or project that got fuller since it was last measured.
#[derive(Debug, Clone)]
pub struct Crossed {
    /// `namespace` or `project`
    pub scope: &'static str,
    pub id: i64,
    pub level: i16,
    pub used: i64,
    pub quota: i64,
}

/// Measure every namespace and project with a quota and store how full each is
/// (0, 80 or 100 %); returns those whose level went up.
pub async fn measure_levels(c: &mut sqlx::PgConnection) -> Result<Vec<Crossed>> {
    let ns = sqlx::query!(
        r#"WITH q AS (SELECT n.id, n.quota_level AS old, nullif(coalesce(n.quota_bytes, s.namespace_quota_bytes), 0) AS quota
                        FROM namespaces n CROSS JOIN instance_settings s WHERE n.parent_id IS NULL),
                used AS (SELECT q.id, (SELECT coalesce(sum(project_storage_bytes(p.id)), 0) FROM projects p JOIN namespaces n ON n.id = p.namespace_id
                                        WHERE n.traversal_ids @> ARRAY[q.id])::bigint AS b
                           FROM q WHERE q.quota IS NOT NULL),
                lv AS (SELECT q.id, q.old, q.quota, coalesce(u.b, 0) AS used,
                              CASE WHEN q.quota IS NULL THEN 0 WHEN coalesce(u.b, 0) >= q.quota THEN 100
                                   WHEN coalesce(u.b, 0) * 5 >= q.quota * 4 THEN 80 ELSE 0 END::smallint AS level
                         FROM q LEFT JOIN used u ON u.id = q.id)
           UPDATE namespaces n SET quota_level = lv.level FROM lv WHERE n.id = lv.id AND n.quota_level <> lv.level
           RETURNING n.id, lv.level AS "level!", lv.old AS "old!", lv.used AS "used!", lv.quota"#
    )
    .fetch_all(&mut *c)
    .await?;
    let ps = sqlx::query!(
        r#"WITH lv AS (SELECT p.id, p.quota_level AS old, nullif(p.quota_bytes, 0) AS quota,
                              CASE WHEN nullif(p.quota_bytes, 0) IS NULL THEN 0 ELSE project_storage_bytes(p.id) END AS used
                         FROM projects p WHERE coalesce(p.quota_bytes, 0) > 0 OR p.quota_level <> 0),
                l2 AS (SELECT lv.*, CASE WHEN quota IS NULL THEN 0 WHEN used >= quota THEN 100
                                         WHEN used * 5 >= quota * 4 THEN 80 ELSE 0 END::smallint AS level FROM lv)
           UPDATE projects p SET quota_level = l2.level FROM l2 WHERE p.id = l2.id AND p.quota_level <> l2.level
           RETURNING p.id, l2.level AS "level!", l2.old AS "old!", l2.used AS "used!", l2.quota"#
    )
    .fetch_all(&mut *c)
    .await?;
    let mut up: Vec<Crossed> = ns
        .into_iter()
        .filter(|r| r.level > r.old)
        .map(|r| Crossed { scope: "namespace", id: r.id, level: r.level, used: r.used, quota: r.quota.unwrap_or(0) })
        .collect();
    up.extend(ps.into_iter().filter(|r| r.level > r.old).map(|r| Crossed {
        scope: "project",
        id: r.id,
        level: r.level,
        used: r.used,
        quota: r.quota.unwrap_or(0),
    }));
    Ok(up)
}

/// How full a project or its namespace was when last measured (the fuller one).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Warning {
    /// `project` or `namespace`
    pub scope: String,
    /// The namespace's path, when it is the namespace.
    pub namespace: Option<String>,
    /// 80 or 100 (%).
    pub level: i16,
}

pub async fn warning(e: impl PgExecutor<'_>, project_id: i64) -> Result<Option<Warning>> {
    let r = sqlx::query!(
        r#"SELECT p.quota_level AS project_level, root.quota_level AS namespace_level, root.path AS "root_path!"
             FROM projects p JOIN namespaces n ON n.id = p.namespace_id JOIN namespaces root ON root.id = n.traversal_ids[1]
            WHERE p.id = $1"#,
        project_id
    )
    .fetch_optional(e)
    .await?;
    Ok(r.and_then(|r| {
        if r.namespace_level >= r.project_level && r.namespace_level > 0 {
            Some(Warning { scope: "namespace".into(), namespace: Some(r.root_path), level: r.namespace_level })
        } else if r.project_level > 0 {
            Some(Warning { scope: "project".into(), namespace: None, level: r.project_level })
        } else {
            None
        }
    }))
}

/// The level of a top-level namespace (0, 80, 100).
pub async fn namespace_level(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<i16> {
    Ok(sqlx::query_scalar!("SELECT quota_level FROM namespaces WHERE id = $1", namespace_id).fetch_optional(e).await?.unwrap_or(0))
}

/// The Owners of a project (its own and inherited), to tell about its quota.
pub async fn project_owner_ids(e: impl PgExecutor<'_>, project_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        r#"SELECT DISTINCT m.user_id AS "id!" FROM members m
             JOIN users u ON u.id = m.user_id AND u.state = 'active' AND u.user_type = 'human'
            WHERE m.access_level >= 50 AND (m.expires_at IS NULL OR m.expires_at > now())
              AND ((m.source_type = 'project' AND m.source_id = $1)
                OR (m.source_type = 'namespace' AND m.source_id = ANY((SELECT n.traversal_ids FROM projects p JOIN namespaces n ON n.id = p.namespace_id WHERE p.id = $1)::bigint[])))"#,
        project_id
    )
    .fetch_all(e)
    .await
}

/// The Owners of a top-level namespace: a group's Owners, or the person.
pub async fn namespace_owner_ids(e: impl PgExecutor<'_>, namespace_id: i64) -> Result<Vec<i64>> {
    sqlx::query_scalar!(
        r#"SELECT DISTINCT u.id AS "id!" FROM users u
            WHERE u.state = 'active' AND u.user_type = 'human'
              AND (u.id IN (SELECT m.user_id FROM members m WHERE m.source_type = 'namespace' AND m.source_id = $1 AND m.access_level >= 50
                                    AND (m.expires_at IS NULL OR m.expires_at > now()))
                OR u.id = (SELECT owner_id FROM namespaces WHERE id = $1))"#,
        namespace_id
    )
    .fetch_all(e)
    .await
}
