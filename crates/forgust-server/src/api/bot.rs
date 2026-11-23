//! The Forgust bot (ADR-0026): a project's switch, a group's default.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::SignedIn;
use crate::auth::authz::{GroupAction, ProjectAction};
use crate::error::{ApiError, ApiResult};
use crate::services::bot as svc;
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct BotStatus {
    /// The instance offers the bot.
    pub available: bool,
    pub enabled: bool,
    /// The account that carries out the commands.
    pub username: String,
    /// Where the project's rules are read from.
    pub config_path: String,
    /// What is wrong with the rules on the default branch, when something is:
    /// no command runs until it is fixed.
    pub config_error: Option<String>,
    /// The commands it knows (`label`: written `/label`).
    pub commands: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct BotSettingsBody {
    pub enabled: bool,
}

async fn status_of(state: &AppState, p: &forgust_db::projects::Project) -> ApiResult<BotStatus> {
    let enabled = forgust_db::bot::enabled(&state.db, p.id).await? == Some(true);
    let config_error = if enabled { svc::rules(state, p, None).await?.err() } else { None };
    Ok(BotStatus {
        available: state.cfg.bot.enabled,
        enabled,
        username: svc::USERNAME.into(),
        config_path: svc::config::PATH.into(),
        config_error,
        commands: svc::parse::Kind::ALL.iter().map(|k| k.name().to_string()).collect(),
    })
}

/// The Forgust bot of the project (Maintainers).
#[utoipa::path(get, path = "/api/v1/projects/{id}/bot", responses((status = 200, body = BotStatus)), tag = "bot")]
pub async fn get(State(state): State<AppState>, SignedIn(_, principal): SignedIn, Path(id): Path<String>) -> ApiResult<Json<BotStatus>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    Ok(Json(status_of(&state, &p).await?))
}

/// Turn the Forgust bot on or off: on, it joins the project as a Developer
/// and carries out the commands written in comments.
#[utoipa::path(put, path = "/api/v1/projects/{id}/bot", request_body = BotSettingsBody, responses((status = 200, body = BotStatus)), tag = "bot")]
pub async fn set(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(body): Json<BotSettingsBody>,
) -> ApiResult<Json<BotStatus>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    crate::auth::authz::writable(&p)?;
    svc::configure(&state, &principal, &p, body.enabled).await?;
    Ok(Json(status_of(&state, &p).await?))
}

#[derive(Serialize, ToSchema)]
pub struct BotCommand {
    /// `label`: written `/label`.
    pub name: String,
    /// How its arguments are written (`~label …`; in brackets: optional), or "".
    pub arguments: String,
    pub description: String,
    /// Only on merge requests.
    pub merge_request_only: bool,
}

/// The commands a comment can use here, for the composer's suggestions: none
/// while the bot is off, and not those the default branch's
/// `.forgust/bot.toml` turns off.
#[utoipa::path(get, path = "/api/v1/projects/{id}/bot/commands", responses((status = 200, body = Vec<BotCommand>)), tag = "bot")]
pub async fn commands(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<BotCommand>>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Read).await?;
    if !svc::is_on(&state, p.id).await? {
        return Ok(Json(Vec::new()));
    }
    // rules that cannot be read: every command is offered, and the bot's reply says what is wrong
    let rules = svc::rules(&state, &p, None).await?.unwrap_or_default();
    Ok(Json(
        svc::parse::Kind::ALL
            .into_iter()
            .filter(|k| rules.command(*k).enabled)
            .map(|k| BotCommand {
                name: k.name().into(),
                arguments: k.arguments().into(),
                description: k.about().into(),
                merge_request_only: k.merge_request_only(),
            })
            .collect(),
    ))
}

// ------------------------------------------------------------ groups

#[derive(Serialize, Deserialize, ToSchema)]
pub struct GroupBot {
    /// The bot is on for the group's projects that never chose, and new ones.
    pub enabled: bool,
}

#[derive(Serialize, ToSchema)]
pub struct GroupBotStatus {
    pub available: bool,
    pub enabled: bool,
    /// Projects turned on by the last change.
    pub turned_on: usize,
}

#[utoipa::path(get, path = "/api/v1/groups/{id}/bot", responses((status = 200, body = GroupBotStatus)), tag = "bot")]
pub async fn group_get(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<GroupBotStatus>> {
    let ns = super::group_for(&state, &principal, &id, GroupAction::Admin).await?;
    let enabled = forgust_db::bot::group_default(&state.db, ns.id).await?;
    Ok(Json(GroupBotStatus { available: state.cfg.bot.enabled, enabled, turned_on: 0 }))
}

/// Turn the Forgust bot on for every project of the group (those that never chose, and new ones).
#[utoipa::path(put, path = "/api/v1/groups/{id}/bot", request_body = GroupBot, responses((status = 200, body = GroupBotStatus)), tag = "bot")]
pub async fn group_set(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(body): Json<GroupBot>,
) -> ApiResult<Json<GroupBotStatus>> {
    let ns = super::group_for(&state, &principal, &id, GroupAction::Admin).await?;
    if !ns.is_group() {
        return Err(ApiError::bad("only groups have a default"));
    }
    let turned_on = svc::set_group_default(&state, &principal, &ns, body.enabled).await?;
    Ok(Json(GroupBotStatus { available: true, enabled: body.enabled, turned_on }))
}
