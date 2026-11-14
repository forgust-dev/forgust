//! Notification levels (M7, ADR-0021): a person's default and their choice per
//! project (`global` = the default). Same shapes under /api/v4. Then where each
//! kind of notification goes (the app, email), and the inbox the app shows.

use axum::Json;
use axum::extract::{Path, Query, State};
use forgust_db::notifications::{self as ndb, Channel, Kind, LEVELS};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::project_for;
use crate::auth::SignedIn;
use crate::auth::authz::ProjectAction;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize, ToSchema)]
pub struct LevelBody {
    /// watch, participating, mention or disabled (`global` for a project: the default)
    pub level: String,
}

#[derive(Serialize, ToSchema)]
pub struct LevelDto {
    pub level: String,
    /// The level that applies (a project's `global` resolved).
    pub effective: String,
}

fn check(level: &str, project: bool) -> ApiResult<()> {
    if LEVELS.contains(&level) || (project && level == "global") {
        Ok(())
    } else {
        Err(ApiError::bad("level must be watch, participating, mention or disabled (or global for a project)"))
    }
}

#[utoipa::path(get, path = "/api/v1/user/notification_settings", responses((status = 200, body = LevelDto)), tag = "user")]
pub async fn mine(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<LevelDto>> {
    let level = ndb::global_level(&state.db, u.id).await?;
    Ok(Json(LevelDto { effective: level.clone(), level }))
}

#[utoipa::path(put, path = "/api/v1/user/notification_settings", request_body = LevelBody, responses((status = 200, body = LevelDto)), tag = "user")]
pub async fn set_mine(State(state): State<AppState>, SignedIn(u, v): SignedIn, Json(b): Json<LevelBody>) -> ApiResult<Json<LevelDto>> {
    if v.direct_user().is_none() {
        return Err(ApiError::Unauthorized);
    }
    check(&b.level, false)?;
    ndb::set_global_level(&state.db, u.id, &b.level).await?;
    Ok(Json(LevelDto { level: b.level.clone(), effective: b.level }))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/notification_settings", responses((status = 200, body = LevelDto)), tag = "projects")]
pub async fn project(State(state): State<AppState>, SignedIn(u, v): SignedIn, Path(id): Path<String>) -> ApiResult<Json<LevelDto>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let global = ndb::global_level(&state.db, u.id).await?;
    let own = ndb::project_level(&state.db, u.id, p.id).await?;
    Ok(Json(LevelDto { level: own.clone().unwrap_or_else(|| "global".into()), effective: own.unwrap_or(global) }))
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/notification_settings", request_body = LevelBody, responses((status = 200, body = LevelDto)), tag = "projects")]
pub async fn set_project(
    State(state): State<AppState>,
    SignedIn(u, v): SignedIn,
    Path(id): Path<String>,
    Json(b): Json<LevelBody>,
) -> ApiResult<Json<LevelDto>> {
    if v.direct_user().is_none() {
        return Err(ApiError::Unauthorized);
    }
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    check(&b.level, true)?;
    let own = (b.level != "global").then_some(b.level.as_str());
    ndb::set_project_level(&state.db, u.id, p.id, own).await?;
    let global = ndb::global_level(&state.db, u.id).await?;
    Ok(Json(LevelDto { level: b.level.clone(), effective: own.map(str::to_string).unwrap_or(global) }))
}

#[derive(Serialize, ToSchema)]
pub struct Unsubscription {
    pub project: String,
    /// The person's level for the project now.
    pub level: String,
}

async fn signed(
    state: &AppState,
    user_id: i64,
    project_id: i64,
    sig: &str,
) -> ApiResult<(forgust_db::users::User, forgust_db::projects::Project)> {
    if !state.secrets.verify(&format!("unsubscribe:{user_id}:{project_id}"), sig) {
        return Err(ApiError::NotFound);
    }
    let u = forgust_db::users::find(&state.db, user_id).await?.ok_or(ApiError::NotFound)?;
    let p = forgust_db::projects::find(&state.db, project_id).await?.ok_or(ApiError::NotFound)?;
    Ok((u, p))
}

/// What a signed unsubscribe link is for (changes nothing: link scanners GET).
#[utoipa::path(get, path = "/api/v1/unsubscribe/{user_id}/{project_id}/{sig}", responses((status = 200, body = Unsubscription)), tag = "user")]
pub async fn unsubscribe_info(
    State(state): State<AppState>,
    Path((user_id, project_id, sig)): Path<(i64, i64, String)>,
) -> ApiResult<Json<Unsubscription>> {
    let (u, p) = signed(&state, user_id, project_id, &sig).await?;
    let level = match ndb::project_level(&state.db, u.id, p.id).await? {
        Some(l) => l,
        None => ndb::global_level(&state.db, u.id).await?,
    };
    Ok(Json(Unsubscription { project: p.full_path, level }))
}

/// One-click unsubscribe (RFC 8058, `List-Unsubscribe-Post`): the link's
/// signature is the authorization; the project's emails stop for that person.
#[utoipa::path(post, path = "/api/v1/unsubscribe/{user_id}/{project_id}/{sig}", responses((status = 200, body = Unsubscription)), tag = "user")]
pub async fn unsubscribe(
    State(state): State<AppState>,
    Path((user_id, project_id, sig)): Path<(i64, i64, String)>,
) -> ApiResult<Json<Unsubscription>> {
    let (u, p) = signed(&state, user_id, project_id, &sig).await?;
    ndb::set_project_level(&state.db, u.id, p.id, Some("disabled")).await?;
    Ok(Json(Unsubscription { project: p.full_path, level: "disabled".into() }))
}

/// Where one kind of notification goes.
#[derive(Serialize, ToSchema)]
pub struct ChannelDto {
    pub kind: Kind,
    /// The bell in the app.
    pub web: bool,
    pub email: bool,
    /// A message on screen as it arrives, while the app is open (only what
    /// reaches the bell).
    pub screen: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct ChannelBody {
    pub kind: Kind,
    pub channel: Channel,
    pub enabled: bool,
}

async fn channels_of(state: &AppState, user_id: i64) -> ApiResult<Vec<ChannelDto>> {
    let own = ndb::channels(&state.db, user_id).await?;
    let on = |kind: Kind, channel| {
        ndb::enabled(own.iter().filter(|(k, ..)| k == kind.as_str()).map(|(_, c, on)| (c.as_str(), *on)), kind, channel)
    };
    Ok(Kind::ALL
        .into_iter()
        .map(|kind| ChannelDto { kind, web: on(kind, Channel::Web), email: on(kind, Channel::Email), screen: on(kind, Channel::Screen) })
        .collect())
}

/// Where each kind of notification goes for the caller.
#[utoipa::path(get, path = "/api/v1/user/notification_channels", responses((status = 200, body = [ChannelDto])), tag = "user")]
pub async fn channels(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Vec<ChannelDto>>> {
    Ok(Json(channels_of(&state, u.id).await?))
}

/// Turn one kind on or off for one channel.
#[utoipa::path(put, path = "/api/v1/user/notification_channels", request_body = ChannelBody, responses((status = 200, body = [ChannelDto])), tag = "user")]
pub async fn set_channel(
    State(state): State<AppState>,
    SignedIn(u, v): SignedIn,
    Json(b): Json<ChannelBody>,
) -> ApiResult<Json<Vec<ChannelDto>>> {
    if v.direct_user().is_none() {
        return Err(ApiError::Unauthorized);
    }
    ndb::set_channel(&state.db, u.id, b.kind, b.channel, b.enabled).await?;
    Ok(Json(channels_of(&state, u.id).await?))
}

#[derive(Serialize, ToSchema)]
pub struct Inbox {
    /// How many are unread (of everything, not only `items`).
    pub unread: i64,
    /// Newest first.
    pub items: Vec<ndb::Notification>,
}

#[derive(Deserialize)]
pub struct InboxQuery {
    limit: Option<i64>,
}

/// The caller's notifications in the app.
#[utoipa::path(get, path = "/api/v1/user/notifications", responses((status = 200, body = Inbox)), tag = "user")]
pub async fn inbox(State(state): State<AppState>, SignedIn(u, _): SignedIn, Query(q): Query<InboxQuery>) -> ApiResult<Json<Inbox>> {
    let items = ndb::inbox(&state.db, u.id, q.limit.unwrap_or(30).clamp(1, 100)).await?;
    Ok(Json(Inbox { unread: ndb::unread_count(&state.db, u.id).await?, items }))
}

async fn read(state: &AppState, user_id: i64, id: Option<i64>) -> ApiResult<Json<Inbox>> {
    let n = ndb::mark_read(&state.db, user_id, id).await?;
    if n == 0 && id.is_some() {
        return Err(ApiError::NotFound);
    }
    // the person's other tabs follow
    state.events.publish(crate::events::Event::Inbox { user_id });
    Ok(Json(Inbox { unread: ndb::unread_count(&state.db, user_id).await?, items: vec![] }))
}

/// Mark one notification read.
pub async fn read_one(State(state): State<AppState>, SignedIn(u, _): SignedIn, Path(id): Path<i64>) -> ApiResult<Json<Inbox>> {
    read(&state, u.id, Some(id)).await
}

/// Mark every notification read.
pub async fn read_all(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Json<Inbox>> {
    read(&state, u.id, None).await
}
