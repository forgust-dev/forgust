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
    /// Ask GitHub for new Forgust releases, to tell administrators.
    #[serde(default = "on")]
    pub update_check_enabled: bool,
}

fn on() -> bool {
    true
}

pub async fn get(e: impl PgExecutor<'_>) -> Result<InstanceSettings> {
    sqlx::query_as!(
        InstanceSettings,
        "SELECT signup_enabled, allow_personal_projects, default_branch_name, mcp_enabled, update_check_enabled FROM instance_settings"
    )
    .fetch_one(e)
    .await
}

pub async fn update(e: impl PgExecutor<'_>, s: &InstanceSettings) -> Result<()> {
    sqlx::query!(
        "UPDATE instance_settings SET signup_enabled = $1, allow_personal_projects = $2, default_branch_name = $3, mcp_enabled = $4, update_check_enabled = $5, updated_at = now()",
        s.signup_enabled,
        s.allow_personal_projects,
        s.default_branch_name,
        s.mcp_enabled,
        s.update_check_enabled
    )
    .execute(e)
    .await?;
    Ok(())
}

/// Whether the MCP server answers.
pub async fn mcp_enabled(e: impl PgExecutor<'_>) -> Result<bool> {
    sqlx::query_scalar!("SELECT mcp_enabled FROM instance_settings").fetch_one(e).await
}

/// Whether the worker asks for new releases (Admin > Settings).
pub async fn update_check_enabled(e: impl PgExecutor<'_>) -> Result<bool> {
    sqlx::query_scalar!("SELECT update_check_enabled FROM instance_settings").fetch_one(e).await
}

/// The latest Forgust release found, and its page.
pub struct LatestRelease {
    pub version: String,
    pub url: Option<String>,
}

/// The latest release found, while the release check is on.
pub async fn latest_release(e: impl PgExecutor<'_>) -> Result<Option<LatestRelease>> {
    let row = sqlx::query!("SELECT latest_version, latest_release_url FROM instance_settings WHERE update_check_enabled")
        .fetch_optional(e)
        .await?;
    Ok(row.and_then(|r| Some(LatestRelease { version: r.latest_version?, url: r.latest_release_url })))
}

pub async fn set_latest_release(e: impl PgExecutor<'_>, version: &str, url: Option<&str>) -> Result<()> {
    sqlx::query!("UPDATE instance_settings SET latest_version = $1, latest_release_url = $2", version, url).execute(e).await?;
    Ok(())
}
