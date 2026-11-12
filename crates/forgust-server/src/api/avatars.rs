//! Avatar upload and download (users, projects, groups).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::{group_for, project_for};
use crate::auth::authz::{GroupAction, ProjectAction};
use crate::auth::{SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::avatars::{self, Owner};
use crate::state::AppState;

/// The PNG, cached for a while (URLs carry a version, `?v=`, when it changes);
/// 404, cached too, when there is none.
async fn serve(state: &AppState, owner: Owner, version: Option<i64>, headers: &HeaderMap) -> ApiResult<Response> {
    let cache = HeaderValue::from_static("public, max-age=300");
    let Some(v) = version else {
        return Ok((StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, cache)]).into_response());
    };
    let etag = format!("\"{v}\"");
    if headers.get(header::IF_NONE_MATCH).and_then(|h| h.to_str().ok()) == Some(etag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::CACHE_CONTROL, cache)]).into_response());
    }
    let Some(png) = avatars::get(state, owner).await? else {
        return Ok((StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, cache)]).into_response());
    };
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (header::CACHE_CONTROL, cache),
            (header::ETAG, HeaderValue::from_str(&etag).map_err(|e| ApiError::Internal(e.into()))?),
            (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        ],
        png,
    )
        .into_response())
}

fn done() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// A person's avatar, by username. Anyone may fetch it; unknown usernames and
/// people without one both answer 404.
pub async fn user_avatar(State(state): State<AppState>, headers: HeaderMap, Path(username): Path<String>) -> ApiResult<Response> {
    let u = forgust_db::users::find_by_username(&state.db, &username).await?.filter(|u| u.is_active());
    match u {
        Some(u) => serve(&state, Owner::User(u.id), u.avatar_at.map(|t| t.timestamp()), &headers).await,
        None => serve(&state, Owner::User(0), None, &headers).await,
    }
}

pub async fn set_my_avatar(State(state): State<AppState>, SignedIn(u, _): SignedIn, body: Bytes) -> ApiResult<Response> {
    avatars::set(&state, Owner::User(u.id), &body).await?;
    Ok(done())
}

pub async fn delete_my_avatar(State(state): State<AppState>, SignedIn(u, _): SignedIn) -> ApiResult<Response> {
    avatars::remove(&state, Owner::User(u.id)).await?;
    Ok(done())
}

pub async fn project_avatar(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    serve(&state, Owner::Project(p.id), p.avatar_at.map(|t| t.timestamp()), &headers).await
}

/// Maintainers set a project's avatar.
pub async fn set_project_avatar(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Admin).await?;
    avatars::set(&state, Owner::Project(p.id), &body).await?;
    Ok(done())
}

pub async fn delete_project_avatar(State(state): State<AppState>, SignedIn(_, v): SignedIn, Path(id): Path<String>) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Admin).await?;
    avatars::remove(&state, Owner::Project(p.id)).await?;
    Ok(done())
}

pub async fn group_avatar(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let g = group_for(&state, &v, &id, GroupAction::Read).await?;
    serve(&state, Owner::Group(g.id), g.avatar_at.map(|t| t.timestamp()), &headers).await
}

pub async fn set_group_avatar(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let g = group_for(&state, &v, &id, GroupAction::Admin).await?;
    avatars::set(&state, Owner::Group(g.id), &body).await?;
    Ok(done())
}

pub async fn delete_group_avatar(State(state): State<AppState>, SignedIn(_, v): SignedIn, Path(id): Path<String>) -> ApiResult<Response> {
    let g = group_for(&state, &v, &id, GroupAction::Admin).await?;
    avatars::remove(&state, Owner::Group(g.id)).await?;
    Ok(done())
}
