//! Releases and project uploads (M7).

use axum::Json;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use forgust_db::projects::Project;
use forgust_db::releases::{Release, ReleaseLink};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Paging, project_for};
use crate::auth::authz::ProjectAction;
use crate::auth::{JobOrViewer, Principal, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::releases::{self as svc, NewLink, NewRelease, ReleaseChange};
use crate::services::uploads::{self, Uploaded};
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct CommitBrief {
    pub id: String,
    pub short_id: String,
    pub title: String,
}

#[derive(Serialize, ToSchema)]
pub struct LinkDto {
    #[serde(flatten)]
    pub link: ReleaseLink,
    /// The link's URL, absolute.
    pub href: String,
    /// `/-/releases/<tag>/downloads/<path>`, when it has a path.
    pub direct_asset_url: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Source {
    pub format: String,
    pub url: String,
}

#[derive(Serialize, ToSchema)]
pub struct ReleaseDto {
    #[serde(flatten)]
    pub release: Release,
    pub description_html: String,
    /// The tagged commit (none when the tag is gone).
    pub commit: Option<CommitBrief>,
    pub links: Vec<LinkDto>,
    pub sources: Vec<Source>,
    /// Released in the future.
    pub upcoming_release: bool,
}

pub fn link_dto(state: &AppState, p: &Project, tag: &str, l: ReleaseLink) -> LinkDto {
    let direct_asset_url = l.direct_asset_path.as_ref().map(|path| format!("{}/downloads{path}", svc::web_url(state, p, tag)));
    LinkDto { href: svc::absolute_url(state, &l.url), direct_asset_url, link: l }
}

pub async fn views(state: &AppState, principal: &Principal, p: &Project, list: Vec<Release>) -> ApiResult<Vec<ReleaseDto>> {
    let code = svc::sees_code(state, principal, p).await;
    let ids: Vec<i64> = list.iter().map(|r| r.id).collect();
    let mut links = forgust_db::releases::links(&state.db, &ids).await?;
    // every tagged commit at once (one listing, not two git calls per release)
    let mut tagged: std::collections::HashMap<String, forgust_storage::CommitSummary> = if code {
        state
            .storage
            .refs(p.id, "refs/tags/")
            .await?
            .into_iter()
            .filter_map(|r| Some((r.name.trim_start_matches("refs/tags/").to_string(), r.commit?)))
            .collect()
    } else {
        Default::default()
    };
    let items: Vec<(&str, &str)> = list.iter().map(|r| (r.description.as_str(), r.tag_name.as_str())).collect();
    let mut html = crate::services::refs::render_at(state, p, &items, code).await.into_iter();
    let mut out = Vec::new();
    for r in list {
        let commit =
            tagged.remove(&r.tag_name).map(|c| CommitBrief { short_id: c.id[..8.min(c.id.len())].to_string(), id: c.id, title: c.subject });
        let mine: Vec<ReleaseLink> = links.extract_if(.., |l| l.release_id == r.id).collect();
        let sources = ["zip", "tar.gz"]
            .iter()
            .filter(|_| code)
            .map(|f| Source {
                format: f.to_string(),
                url: format!("/api/v1/projects/{}/repository/archive?ref={}&format={f}", p.id, svc::encode_tag(&r.tag_name)),
            })
            .collect();
        out.push(ReleaseDto {
            description_html: html.next().unwrap_or_default(),
            upcoming_release: r.released_at > Utc::now(),
            links: mine.into_iter().map(|l| link_dto(state, p, &r.tag_name, l)).collect(),
            commit,
            sources,
            release: r,
        });
    }
    Ok(out)
}

async fn one(state: &AppState, principal: &Principal, p: &Project, r: Release) -> ApiResult<Json<ReleaseDto>> {
    Ok(Json(views(state, principal, p, vec![r]).await?.pop().ok_or(ApiError::NotFound)?))
}

/// Releases, newest first.
#[utoipa::path(get, path = "/api/v1/projects/{id}/releases", responses((status = 200, body = Vec<ReleaseDto>)), tag = "releases")]
pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(pg): Query<Paging>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let list = forgust_db::releases::list(&state.db, p.id, pg.limit(), pg.offset()).await?;
    let total = forgust_db::releases::count(&state.db, p.id).await?;
    let mut r = Json(views(&state, &v, &p, list).await?).into_response();
    r.headers_mut().insert("x-total", HeaderValue::from(total));
    Ok(r)
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/releases/{tag}", responses((status = 200, body = ReleaseDto)), tag = "releases")]
pub async fn show(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, tag)): Path<(String, String)>,
) -> ApiResult<Json<ReleaseDto>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::find(&state, &v, &p, &tag).await?;
    one(&state, &v, &p, r).await
}

#[derive(Deserialize, ToSchema)]
pub struct CreateReleaseBody {
    pub tag_name: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Where to create the tag when it does not exist.
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    /// Makes that new tag annotated.
    pub tag_message: Option<String>,
    pub released_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub links: Vec<NewLink>,
}

/// Create a release (Developer+), and its tag when `ref` is given and the tag is new.
#[utoipa::path(post, path = "/api/v1/projects/{id}/releases", request_body = CreateReleaseBody, responses((status = 201, body = ReleaseDto)), tag = "releases")]
pub async fn create(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path(id): Path<String>,
    Json(b): Json<CreateReleaseBody>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::create(&state, &v, &p, new_release(&b)).await?;
    Ok((StatusCode::CREATED, one(&state, &v, &p, r).await?).into_response())
}

pub fn new_release(b: &CreateReleaseBody) -> NewRelease<'_> {
    NewRelease {
        tag_name: &b.tag_name,
        name: b.name.as_deref(),
        description: b.description.as_deref(),
        git_ref: b.git_ref.as_deref(),
        tag_message: b.tag_message.as_deref(),
        released_at: b.released_at,
        links: &b.links,
    }
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateReleaseBody {
    pub name: Option<String>,
    pub description: Option<String>,
    pub released_at: Option<DateTime<Utc>>,
}

#[utoipa::path(put, path = "/api/v1/projects/{id}/releases/{tag}", request_body = UpdateReleaseBody, responses((status = 200, body = ReleaseDto)), tag = "releases")]
pub async fn update(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag)): Path<(String, String)>,
    Json(b): Json<UpdateReleaseBody>,
) -> ApiResult<Json<ReleaseDto>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::update(
        &state,
        &v,
        &p,
        &tag,
        ReleaseChange { name: b.name.as_deref(), description: b.description.as_deref(), released_at: b.released_at },
    )
    .await?;
    one(&state, &v, &p, r).await
}

/// Delete a release; its tag stays.
#[utoipa::path(delete, path = "/api/v1/projects/{id}/releases/{tag}", responses((status = 204)), tag = "releases")]
pub async fn delete(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    svc::delete(&state, &v, &p, &tag).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/releases/{tag}/links", request_body = NewLink, responses((status = 201, body = LinkDto)), tag = "releases")]
pub async fn add_link(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag)): Path<(String, String)>,
    Json(l): Json<NewLink>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let link = svc::add_link(&state, &v, &p, &tag, &l).await?;
    Ok((StatusCode::CREATED, Json(link_dto(&state, &p, &tag, link))).into_response())
}

#[utoipa::path(delete, path = "/api/v1/projects/{id}/releases/{tag}/links/{link_id}", responses((status = 204)), tag = "releases")]
pub async fn delete_link(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag, link_id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    svc::delete_link(&state, &v, &p, &tag, link_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Upload a file (multipart, part `file`) to show or link in Markdown, or to attach
/// to a release.
#[utoipa::path(post, path = "/api/v1/projects/{id}/uploads", responses((status = 201, body = Uploaded)), tag = "projects")]
pub async fn upload(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path(id): Path<String>,
    multipart: Multipart,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    if matches!(v, Principal::Anonymous) {
        return Err(ApiError::Unauthorized);
    }
    let u = uploads::store(&state, &v, &p, multipart).await?;
    Ok((StatusCode::CREATED, Json(uploads::describe(&u))).into_response())
}
