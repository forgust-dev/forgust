//! CI/CD variables of projects and groups (values encrypted by the caller).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize)]
pub struct Variable {
    pub id: i64,
    pub project_id: Option<i64>,
    pub group_id: Option<i64>,
    pub key: String,
    #[serde(skip)]
    pub value_enc: String,
    /// `env_var` or `file`.
    pub variable_type: String,
    pub protected: bool,
    pub masked: bool,
    pub raw: bool,
    pub environment_scope: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// Variables a project's jobs see: groups from the outermost to the closest,
/// then the project (later ones win).
pub async fn for_project(e: impl PgExecutor<'_>, project_id: i64, traversal_ids: &[i64]) -> Result<Vec<Variable>> {
    sqlx::query_as!(
        Variable,
        "SELECT v.id, v.project_id, v.group_id, v.key, v.value_enc, v.variable_type, v.protected, v.masked, v.raw,
                v.environment_scope, v.description, v.created_at
         FROM ci_variables v
         WHERE (v.project_id = $1 OR v.group_id = ANY ($2)) AND v.environment_scope = '*'
         ORDER BY v.project_id IS NOT NULL, array_position($2, v.group_id), v.key",
        project_id,
        traversal_ids
    )
    .fetch_all(e)
    .await
}

pub async fn list(e: impl PgExecutor<'_>, project_id: Option<i64>, group_id: Option<i64>) -> Result<Vec<Variable>> {
    sqlx::query_as!(
        Variable,
        "SELECT id, project_id, group_id, key, value_enc, variable_type, protected, masked, raw, environment_scope, description,
                created_at
         FROM ci_variables
         WHERE ($1::bigint IS NOT NULL AND project_id = $1) OR ($2::bigint IS NOT NULL AND group_id = $2)
         ORDER BY key, environment_scope",
        project_id,
        group_id
    )
    .fetch_all(e)
    .await
}

pub struct VariableInput<'a> {
    pub key: &'a str,
    pub value_enc: &'a str,
    pub variable_type: &'a str,
    pub protected: bool,
    pub masked: bool,
    pub raw: bool,
    pub description: &'a str,
}

/// Create or replace by key.
pub async fn upsert(e: impl PgExecutor<'_>, project_id: Option<i64>, group_id: Option<i64>, v: VariableInput<'_>) -> Result<i64> {
    sqlx::query_scalar!(
        "WITH upd AS (
             UPDATE ci_variables SET value_enc = $4, variable_type = $5, protected = $6, masked = $7, raw = $8, description = $9
             WHERE key = $3 AND environment_scope = '*'
               AND (($1::bigint IS NOT NULL AND project_id = $1) OR ($2::bigint IS NOT NULL AND group_id = $2))
             RETURNING id)
         INSERT INTO ci_variables (project_id, group_id, key, value_enc, variable_type, protected, masked, raw, description)
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9 WHERE NOT EXISTS (SELECT 1 FROM upd)
         RETURNING id",
        project_id,
        group_id,
        v.key,
        v.value_enc,
        v.variable_type,
        v.protected,
        v.masked,
        v.raw,
        v.description
    )
    .fetch_optional(e)
    .await
    .map(|id| id.unwrap_or(0))
}

pub async fn delete(e: impl PgExecutor<'_>, project_id: Option<i64>, group_id: Option<i64>, key: &str) -> Result<bool> {
    let n = sqlx::query!(
        "DELETE FROM ci_variables WHERE key = $3
           AND (($1::bigint IS NOT NULL AND project_id = $1) OR ($2::bigint IS NOT NULL AND group_id = $2))",
        project_id,
        group_id,
        key
    )
    .execute(e)
    .await?
    .rows_affected();
    Ok(n > 0)
}
