use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AuditEvent {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub principal_kind: String,
    pub principal_id: Option<i64>,
    pub principal_name: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<i64>,
    #[schema(value_type = Object)]
    pub details: serde_json::Value,
    pub ip: Option<String>,
}

pub struct NewEvent<'a> {
    pub principal_kind: &'a str,
    pub principal_id: Option<i64>,
    pub action: &'a str,
    pub target_type: Option<&'a str>,
    pub target_id: Option<i64>,
    pub details: serde_json::Value,
    pub ip: Option<&'a str>,
}

pub async fn insert(e: impl PgExecutor<'_>, ev: NewEvent<'_>) -> Result<()> {
    sqlx::query!(
        "INSERT INTO audit_events (principal_kind, principal_id, action, target_type, target_id, details, ip)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        ev.principal_kind,
        ev.principal_id,
        ev.action,
        ev.target_type,
        ev.target_id,
        ev.details,
        ev.ip
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn list(
    e: impl PgExecutor<'_>,
    target_type: Option<&str>,
    target_id: Option<i64>,
    limit: i64,
    before_id: Option<i64>,
) -> Result<Vec<AuditEvent>> {
    sqlx::query_as!(
        AuditEvent,
        r#"SELECT a.id, a.created_at, a.principal_kind, a.principal_id,
                  CASE WHEN a.principal_kind IN ('user', 'admin') THEN u.username END AS principal_name,
                  a.action, a.target_type, a.target_id, a.details, a.ip
           FROM audit_events a LEFT JOIN users u ON u.id = a.principal_id
           WHERE ($1::text IS NULL OR a.target_type = $1) AND ($2::bigint IS NULL OR a.target_id = $2)
             AND ($4::bigint IS NULL OR a.id < $4)
           ORDER BY a.id DESC LIMIT $3"#,
        target_type,
        target_id,
        limit,
        before_id
    )
    .fetch_all(e)
    .await
}
