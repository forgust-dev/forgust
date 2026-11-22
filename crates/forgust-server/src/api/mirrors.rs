//! A project's repository mirrors (Maintainers): pulled from another forge, or
//! pushed to one.

use axum::Json;
use axum::extract::{Path, State};
use forgust_db::project_mirrors::{self as mdb, Mirror, MirrorChange, NewMirror};
use forgust_db::projects::Project;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{Principal, SignedIn};
use crate::error::{ApiError, ApiResult};
use crate::services::repo_mirrors::{self, MAX_PUSH_MIRRORS, MirrorUpdate};
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct MirrorDto {
    #[serde(flatten)]
    pub mirror: Mirror,
    /// Credentials are stored (never shown).
    pub has_token: bool,
}

/// The project's mirrors, as every handler answers.
async fn listed(state: &AppState, p: &Project) -> ApiResult<Json<Vec<MirrorDto>>> {
    let list = mdb::list(&state.db, p.id).await?;
    Ok(Json(list.into_iter().map(|m| MirrorDto { has_token: m.has_token(), mirror: m }).collect()))
}

async fn admin(state: &AppState, p0: &Principal, id: &str) -> ApiResult<Project> {
    project_for(state, p0, id, ProjectAction::Admin).await
}

async fn mine(state: &AppState, p: &Project, mirror_id: i64) -> ApiResult<Mirror> {
    mdb::find(&state.db, mirror_id).await?.filter(|m| m.project_id == p.id).ok_or(ApiError::NotFound)
}

fn interval(minutes: Option<i32>) -> ApiResult<i32> {
    let m = minutes.unwrap_or(60);
    if !(15..=10080).contains(&m) {
        return Err(ApiError::bad("the interval is between 15 minutes and a week (10080)"));
    }
    Ok(m)
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/mirrors", responses((status = 200, body = [MirrorDto])), tag = "projects")]
pub async fn list(State(state): State<AppState>, SignedIn(_, p0): SignedIn, Path(id): Path<String>) -> ApiResult<Json<Vec<MirrorDto>>> {
    let p = admin(&state, &p0, &id).await?;
    listed(&state, &p).await
}

#[derive(Deserialize, ToSchema)]
pub struct NewMirrorBody {
    /// `pull` (this repository follows the address) or `push`.
    pub direction: String,
    /// The other repository's http(s) address, without credentials.
    pub url: String,
    /// Sent with the token (`oauth2` when empty: GitLab, GitHub, Gitea and
    /// Forgejo take a token as the password whatever the name).
    #[serde(default)]
    pub username: String,
    /// A token or password; none for a public repository (pull).
    #[serde(default)]
    pub token: String,
    /// Push: only protected branches, nothing deleted on the remote.
    #[serde(default)]
    pub only_protected: bool,
    /// Pull: run pipelines for what arrives (as you).
    #[serde(default)]
    pub trigger_pipelines: bool,
    /// Pull: minutes between updates (15 to 10080, default 60).
    pub interval_minutes: Option<i32>,
}

/// Add a mirror; its first update starts at once. A pull mirror makes the
/// repository read-only and replaces its branches and tags with the source's.
#[utoipa::path(post, path = "/api/v1/projects/{id}/mirrors", request_body = NewMirrorBody, responses((status = 200, body = [MirrorDto])), tag = "projects")]
pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<NewMirrorBody>,
) -> ApiResult<Json<Vec<MirrorDto>>> {
    let p = admin(&state, &p0, &id).await?;
    let user = p0.direct_user().ok_or(ApiError::Unauthorized)?;
    if !matches!(req.direction.as_str(), "pull" | "push") {
        return Err(ApiError::bad("direction must be pull or push"));
    }
    let url = repo_mirrors::checked(&state, &req.url).await.map_err(ApiError::bad)?;
    let existing = mdb::list(&state.db, p.id).await?;
    if req.direction == "pull" && existing.iter().any(|m| m.direction == "pull") {
        return Err(ApiError::conflict("the project is already a pull mirror"));
    }
    if req.direction == "push" && existing.iter().filter(|m| m.direction == "push").count() >= MAX_PUSH_MIRRORS {
        return Err(ApiError::conflict(format!("a project has at most {MAX_PUSH_MIRRORS} push mirrors")));
    }
    // in the other direction too: pushing to the address the repository is
    // pulled from would undo there whatever landed since the last pull
    if existing.iter().any(|m| m.url == url.as_str()) {
        return Err(ApiError::conflict("that address is already a mirror of this repository"));
    }
    let sealed = match req.token.trim() {
        "" => None,
        t => Some(state.secrets.encrypt(t.as_bytes()).map_err(ApiError::Internal)?),
    };
    let mirror_id = repo_mirrors::add(
        &state,
        &p0,
        NewMirror {
            project_id: p.id,
            direction: &req.direction,
            url: url.as_str(),
            username: req.username.trim(),
            token_enc: sealed.as_deref(),
            only_protected: req.only_protected && req.direction == "push",
            trigger_pipelines: req.trigger_pipelines && req.direction == "pull",
            interval_minutes: interval(req.interval_minutes)?,
            created_by: Some(user.id),
        },
    )
    .await?;
    let mut conn = state.db.acquire().await?;
    state.jobs.enqueue(&mut conn, MirrorUpdate::request(p.id, mirror_id)).await.map_err(ApiError::Internal)?;
    listed(&state, &p).await
}

#[derive(Deserialize, ToSchema)]
pub struct MirrorChangeBody {
    pub enabled: Option<bool>,
    pub only_protected: Option<bool>,
    pub trigger_pipelines: Option<bool>,
    pub interval_minutes: Option<i32>,
    /// New credentials (the address never changes); an empty token removes them.
    pub username: Option<String>,
    pub token: Option<String>,
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/mirrors/{mirror_id}", request_body = MirrorChangeBody, responses((status = 200, body = [MirrorDto])), tag = "projects")]
pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, mirror_id)): Path<(String, i64)>,
    Json(req): Json<MirrorChangeBody>,
) -> ApiResult<Json<Vec<MirrorDto>>> {
    let p = admin(&state, &p0, &id).await?;
    let m = mine(&state, &p, mirror_id).await?;
    let push = m.direction == "push";
    let change = MirrorChange {
        enabled: req.enabled.unwrap_or(m.enabled),
        only_protected: push && req.only_protected.unwrap_or(m.only_protected),
        trigger_pipelines: !push && req.trigger_pipelines.unwrap_or(m.trigger_pipelines),
        interval_minutes: interval(req.interval_minutes.or(Some(m.interval_minutes)))?,
    };
    let mut tx = state.db.begin().await?;
    mdb::update(&mut *tx, m.id, &change).await?;
    // pipelines run as whoever turns them on, never as someone else
    if change.trigger_pipelines && !m.trigger_pipelines {
        let user = p0.direct_user().ok_or(ApiError::Unauthorized)?;
        mdb::set_creator(&mut *tx, m.id, user.id).await?;
    }
    let credentials = req.token.is_some() || req.username.is_some();
    if credentials {
        let sealed = match req.token.as_deref().map(str::trim) {
            None => m.token_enc.clone(),
            Some("") => None,
            Some(t) => Some(state.secrets.encrypt(t.as_bytes()).map_err(ApiError::Internal)?),
        };
        let username = req.username.as_deref().map(str::trim).unwrap_or(&m.username);
        mdb::set_credentials(&mut *tx, m.id, username, sealed.as_deref()).await?;
    }
    // turned on again, or new credentials: try at once
    if change.enabled && (!m.enabled || credentials) {
        state.jobs.enqueue(&mut tx, MirrorUpdate::request(p.id, m.id)).await.map_err(ApiError::Internal)?;
    }
    tx.commit().await?;
    crate::services::audit(
        &state,
        &p0,
        "mirror_changed",
        Some(("project", p.id)),
        json!({
            "mirror": m.id, "enabled": change.enabled, "only_protected": change.only_protected,
            "trigger_pipelines": change.trigger_pipelines, "interval_minutes": change.interval_minutes, "credentials": credentials,
            "previous": { "enabled": m.enabled, "only_protected": m.only_protected, "trigger_pipelines": m.trigger_pipelines,
                          "interval_minutes": m.interval_minutes },
        }),
        None,
    )
    .await;
    listed(&state, &p).await
}

/// Remove a mirror; a pull mirror's repository becomes writable again.
#[utoipa::path(delete, path = "/api/v1/projects/{id}/mirrors/{mirror_id}", responses((status = 200, body = [MirrorDto])), tag = "projects")]
pub async fn delete(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, mirror_id)): Path<(String, i64)>,
) -> ApiResult<Json<Vec<MirrorDto>>> {
    let p = admin(&state, &p0, &id).await?;
    let m = mine(&state, &p, mirror_id).await?;
    mdb::delete(&state.db, m.id).await?;
    crate::services::audit(
        &state,
        &p0,
        "mirror_removed",
        Some(("project", p.id)),
        json!({ "mirror": m.id, "direction": m.direction, "url": m.url }),
        None,
    )
    .await;
    listed(&state, &p).await
}

/// Update the mirror now instead of at its next turn.
#[utoipa::path(post, path = "/api/v1/projects/{id}/mirrors/{mirror_id}/update", responses((status = 200, body = [MirrorDto])), tag = "projects")]
pub async fn update_now(
    State(state): State<AppState>,
    SignedIn(_, p0): SignedIn,
    Path((id, mirror_id)): Path<(String, i64)>,
) -> ApiResult<Json<Vec<MirrorDto>>> {
    let p = admin(&state, &p0, &id).await?;
    let m = mine(&state, &p, mirror_id).await?;
    if !m.enabled {
        return Err(ApiError::conflict("the mirror is paused: turn it on first"));
    }
    let mut conn = state.db.acquire().await?;
    state.jobs.enqueue(&mut conn, MirrorUpdate::request(p.id, m.id)).await.map_err(ApiError::Internal)?;
    listed(&state, &p).await
}
