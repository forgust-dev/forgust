//! Webhooks and their delivery log.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Webhook {
    pub id: i64,
    pub project_id: Option<i64>,
    pub namespace_id: Option<i64>,
    pub url: String,
    #[serde(skip)]
    pub secret_enc: Option<String>,
    pub events: Vec<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    /// `generic` (the JSON payload), or a chat app: `slack`, `mattermost`,
    /// `discord`, `matrix`, `teams`.
    pub format: String,
}

pub async fn create(
    e: impl PgExecutor<'_>,
    project_id: Option<i64>,
    namespace_id: Option<i64>,
    url: &str,
    secret_enc: Option<&str>,
    events: &[String],
    format: &str,
) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO webhooks (project_id, namespace_id, url, secret_enc, events, format) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        project_id,
        namespace_id,
        url,
        secret_enc,
        events,
        format
    )
    .fetch_one(e)
    .await
}

pub async fn list(e: impl PgExecutor<'_>, project_id: Option<i64>, namespace_id: Option<i64>) -> Result<Vec<Webhook>> {
    sqlx::query_as!(
        Webhook,
        "SELECT id, project_id, namespace_id, url, secret_enc, events, enabled, created_at, format FROM webhooks
         WHERE project_id IS NOT DISTINCT FROM $1 AND namespace_id IS NOT DISTINCT FROM $2 ORDER BY id",
        project_id,
        namespace_id
    )
    .fetch_all(e)
    .await
}

pub async fn find(e: impl PgExecutor<'_>, id: i64) -> Result<Option<Webhook>> {
    sqlx::query_as!(
        Webhook,
        "SELECT id, project_id, namespace_id, url, secret_enc, events, enabled, created_at, format FROM webhooks WHERE id = $1",
        id
    )
    .fetch_optional(e)
    .await
}

pub async fn delete(e: impl PgExecutor<'_>, id: i64, project_id: Option<i64>, namespace_id: Option<i64>) -> Result<bool> {
    Ok(sqlx::query!(
        "DELETE FROM webhooks WHERE id = $1 AND project_id IS NOT DISTINCT FROM $2 AND namespace_id IS NOT DISTINCT FROM $3",
        id,
        project_id,
        namespace_id
    )
    .execute(e)
    .await?
    .rows_affected()
        > 0)
}

/// Enabled hooks for an event on a project: its own and those of every ancestor group.
pub async fn for_event(e: impl PgExecutor<'_>, project_id: i64, event: &str) -> Result<Vec<Webhook>> {
    sqlx::query_as!(
        Webhook,
        "SELECT w.id, w.project_id, w.namespace_id, w.url, w.secret_enc, w.events, w.enabled, w.created_at, w.format
         FROM webhooks w
         WHERE w.enabled AND $2 = ANY(w.events)
           AND (w.project_id = $1
                OR w.namespace_id = ANY((SELECT n.traversal_ids FROM projects p JOIN namespaces n ON n.id = p.namespace_id WHERE p.id = $1)::bigint[]))",
        project_id,
        event
    )
    .fetch_all(e)
    .await
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Delivery {
    pub id: i64,
    pub webhook_id: i64,
    pub event: String,
    pub status: String,
    pub attempts: i32,
    pub response_code: Option<i32>,
    pub error: Option<String>,
    pub duration_ms: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

pub async fn create_delivery(e: impl PgExecutor<'_>, webhook_id: i64, event: &str, payload: &serde_json::Value) -> Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO webhook_deliveries (webhook_id, event, payload) VALUES ($1, $2, $3) RETURNING id",
        webhook_id,
        event,
        payload
    )
    .fetch_one(e)
    .await
}

/// What a delivery attempt needs: the hook, event, payload and attempts so far.
pub struct PendingDelivery {
    pub webhook_id: i64,
    pub event: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
}

pub async fn pending_delivery(e: impl PgExecutor<'_>, id: i64) -> Result<Option<PendingDelivery>> {
    sqlx::query_as!(PendingDelivery, "SELECT webhook_id, event, payload, attempts FROM webhook_deliveries WHERE id = $1", id)
        .fetch_optional(e)
        .await
}

pub async fn record_attempt(
    e: impl PgExecutor<'_>,
    id: i64,
    delivered: bool,
    final_failure: bool,
    response_code: Option<i32>,
    error: Option<&str>,
    duration_ms: i32,
) -> Result<()> {
    let status = if delivered {
        "delivered"
    } else if final_failure {
        "failed"
    } else {
        "pending"
    };
    sqlx::query!(
        "UPDATE webhook_deliveries SET attempts = attempts + 1, status = $2, response_code = $3, error = $4, duration_ms = $5,
                delivered_at = CASE WHEN $6 THEN now() ELSE delivered_at END
         WHERE id = $1",
        id,
        status,
        response_code,
        error,
        duration_ms,
        delivered
    )
    .execute(e)
    .await?;
    Ok(())
}

pub async fn deliveries(e: impl PgExecutor<'_>, webhook_id: i64, limit: i64) -> Result<Vec<Delivery>> {
    sqlx::query_as!(
        Delivery,
        "SELECT id, webhook_id, event, status, attempts, response_code, error, duration_ms, created_at, delivered_at
         FROM webhook_deliveries WHERE webhook_id = $1 ORDER BY id DESC LIMIT $2",
        webhook_id,
        limit
    )
    .fetch_all(e)
    .await
}
