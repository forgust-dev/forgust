use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct InstanceSettings {
    pub signup_enabled: bool,
    pub allow_personal_projects: bool,
    pub default_branch_name: String,
    /// The MCP server answers.
    #[serde(default = "on")]
    pub mcp_enabled: bool,
}

fn on() -> bool {
    true
}

pub async fn get(e: impl PgExecutor<'_>) -> Result<InstanceSettings> {
    sqlx::query_as!(
        InstanceSettings,
        "SELECT signup_enabled, allow_personal_projects, default_branch_name, mcp_enabled FROM instance_settings"
    )
    .fetch_one(e)
    .await
}

pub async fn update(e: impl PgExecutor<'_>, s: &InstanceSettings) -> Result<()> {
    sqlx::query!(
        "UPDATE instance_settings SET signup_enabled = $1, allow_personal_projects = $2, default_branch_name = $3, mcp_enabled = $4, updated_at = now()",
        s.signup_enabled,
        s.allow_personal_projects,
        s.default_branch_name,
        s.mcp_enabled
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Whether the MCP server answers.
pub async fn mcp_enabled(e: impl PgExecutor<'_>) -> Result<bool> {
    sqlx::query_scalar!("SELECT mcp_enabled FROM instance_settings").fetch_one(e).await
}
