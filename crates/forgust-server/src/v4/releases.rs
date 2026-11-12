//! Releases, their asset links, project uploads and tags, as GitLab's API has
//! them (`glab release create/upload/list/view/delete`, CI jobs with `JOB-TOKEN`).
//! A tag is one URL segment, percent-encoded (`release%2Fv1.0`).

use axum::Json;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use forgust_db::projects::Project;
use forgust_db::releases::{Release, ReleaseLink};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{PageQuery, json as j, paged};
use crate::api::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{JobOrViewer, Principal, SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::releases::{self as svc, NewLink, NewRelease, ReleaseChange};
use crate::state::AppState;

fn link_json(state: &AppState, p: &Project, tag: &str, l: &ReleaseLink) -> Value {
    let direct = l.direct_asset_path.as_ref().map(|path| format!("{}/downloads{path}", svc::web_url(state, p, tag)));
    json!({
        "id": l.id,
        "name": l.name,
        "url": svc::absolute_url(state, &l.url),
        "direct_asset_url": direct.unwrap_or_else(|| svc::absolute_url(state, &l.url)),
        "link_type": l.link_type,
    })
}

/// `tagged`: the tag's commit when the caller listed the tags already (a list
/// of releases: one listing, not a git call per release); None to look it up.
async fn release_json(
    state: &AppState,
    code: bool,
    p: &Project,
    r: &Release,
    links: &[ReleaseLink],
    tagged: Option<Option<String>>,
) -> ApiResult<Value> {
    let base = j::base(&state.cfg);
    // Guests see releases, not code: no commit, no source archives
    let tagged = match tagged {
        _ if !code => None,
        Some(known) => known,
        None => state.storage.resolve(p.id, &format!("refs/tags/{}^{{commit}}", r.tag_name)).await?,
    };
    let commit = match tagged {
        Some(sha) => state.storage.commit(p.id, &sha).await?.map(|c| j::commit(&state.cfg, p, &c)),
        None => None,
    };
    let tag = svc::encode_tag(&r.tag_name);
    let sources: Vec<Value> = ["zip", "tar.gz"]
        .iter()
        .filter(|_| code)
        .map(|f| json!({ "format": f, "url": format!("{base}/api/v1/projects/{}/repository/archive?ref={tag}&format={f}", p.id) }))
        .collect();
    let web = svc::web_url(state, p, &r.tag_name);
    let mut out = json!({
        "tag_name": r.tag_name,
        "name": r.name,
        "description": r.description,
        "created_at": j::ts(&r.created_at),
        "released_at": j::ts(&r.released_at),
        "upcoming_release": r.released_at > Utc::now(),
        "author": j::person(&state.cfg, r.author_id, r.author_username.as_deref(), r.author_name.as_deref(), None),
        "commit": commit,
        "milestones": [],
        "commit_path": format!("/{}/-/commit/{}", p.full_path, commit.as_ref().and_then(|c| c["id"].as_str()).unwrap_or("")),
        "tag_path": format!("/{}/-/tags/{tag}", p.full_path),
        "assets": {
            "count": sources.len() + links.len(),
            "sources": sources,
            "links": links.iter().map(|l| link_json(state, p, &r.tag_name, l)).collect::<Vec<_>>(),
        },
        "evidences": [],
        "_links": { "self": web, "edit_url": format!("{web}/edit") },
    });
    if commit.is_none() {
        out["commit_path"] = Value::Null;
    }
    Ok(out)
}

async fn render(state: &AppState, principal: &Principal, p: &Project, r: &Release) -> ApiResult<Value> {
    let links = forgust_db::releases::links(&state.db, &[r.id]).await?;
    release_json(state, svc::sees_code(state, principal, p).await, p, r, &links, None).await
}

pub async fn list(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let list = forgust_db::releases::list(&state.db, p.id, pg.probe(), pg.offset()).await?;
    let total = forgust_db::releases::count(&state.db, p.id).await?;
    let ids: Vec<i64> = list.iter().map(|r| r.id).collect();
    let links = forgust_db::releases::links(&state.db, &ids).await?;
    let code = svc::sees_code(&state, &v, &p).await;
    // every tag's commit in one listing; then each release's commit, a few at a time
    let tags: std::collections::HashMap<String, String> = if code {
        state
            .storage
            .refs(p.id, "refs/tags/")
            .await?
            .into_iter()
            .map(|t| (t.name.trim_start_matches("refs/tags/").to_string(), t.peeled.unwrap_or(t.target)))
            .collect()
    } else {
        Default::default()
    };
    let mut by_release = forgust_db::group_by_id(links.into_iter().map(|l| (l.release_id, l)).collect());
    let rows: Vec<_> = list
        .iter()
        .map(|r| {
            let links = by_release.remove(&r.id).unwrap_or_default();
            let sha = tags.get(&r.tag_name).cloned();
            let (state, p) = (&state, &p);
            async move { release_json(state, code, p, r, &links, Some(sha)).await }
        })
        .collect();
    let items: Vec<Value> = futures::stream::iter(rows).buffered(8).collect::<Vec<_>>().await.into_iter().collect::<ApiResult<_>>()?;
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

pub async fn show(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::find(&state, &v, &p, &tag).await?;
    Ok(Json(render(&state, &v, &p, &r).await?))
}

#[derive(Deserialize, Default)]
pub struct Assets {
    #[serde(default)]
    links: Vec<NewLink>,
}

#[derive(Deserialize)]
pub struct V4Release {
    tag_name: String,
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "ref")]
    git_ref: Option<String>,
    tag_message: Option<String>,
    released_at: Option<DateTime<Utc>>,
    #[serde(default)]
    assets: Assets,
}

pub async fn create(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path(id): Path<String>,
    Json(b): Json<V4Release>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::create(
        &state,
        &v,
        &p,
        NewRelease {
            tag_name: &b.tag_name,
            name: b.name.as_deref(),
            description: b.description.as_deref(),
            git_ref: b.git_ref.as_deref(),
            tag_message: b.tag_message.as_deref(),
            released_at: b.released_at,
            links: &b.assets.links,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(render(&state, &v, &p, &r).await?)).into_response())
}

#[derive(Deserialize)]
pub struct V4ReleaseChange {
    name: Option<String>,
    description: Option<String>,
    released_at: Option<DateTime<Utc>>,
}

pub async fn update(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag)): Path<(String, String)>,
    Json(b): Json<V4ReleaseChange>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::update(
        &state,
        &v,
        &p,
        &tag,
        ReleaseChange { name: b.name.as_deref(), description: b.description.as_deref(), released_at: b.released_at },
    )
    .await?;
    Ok(Json(render(&state, &v, &p, &r).await?))
}

/// Answers with the deleted release, as GitLab does.
pub async fn delete(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::find(&state, &v, &p, &tag).await?;
    let out = render(&state, &v, &p, &r).await?;
    svc::delete(&state, &v, &p, &tag).await?;
    Ok(Json(out))
}

pub async fn links(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::find(&state, &v, &p, &tag).await?;
    let links = forgust_db::releases::links(&state.db, &[r.id]).await?;
    Ok(Json(json!(links.iter().map(|l| link_json(&state, &p, &tag, l)).collect::<Vec<_>>())))
}

pub async fn link(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag, link_id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let r = svc::find(&state, &v, &p, &tag).await?;
    let links = forgust_db::releases::links(&state.db, &[r.id]).await?;
    let l = links.iter().find(|l| l.id == link_id).ok_or(ApiError::NotFound)?;
    Ok(Json(link_json(&state, &p, &tag, l)))
}

pub async fn add_link(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag)): Path<(String, String)>,
    Json(l): Json<NewLink>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let link = svc::add_link(&state, &v, &p, &tag, &l).await?;
    Ok((StatusCode::CREATED, Json(link_json(&state, &p, &tag, &link))).into_response())
}

pub async fn delete_link(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag, link_id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let l = svc::delete_link(&state, &v, &p, &tag, link_id).await?;
    Ok(Json(link_json(&state, &p, &tag, &l)))
}

/// `GET .../releases/:tag/downloads/*path`: to where the link points.
pub async fn download(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, tag, path)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Read).await?;
    let url = svc::download_url(&state, &v, &p, &tag, &path).await?;
    Ok((StatusCode::FOUND, [(header::LOCATION, url)]).into_response())
}

// ---------------------------------------------------------------- tags

async fn tag_json(state: &AppState, p: &Project, r: &forgust_storage::RefInfo, release: Option<&Release>) -> ApiResult<Value> {
    let name = r.name.trim_start_matches("refs/tags/");
    let sha = r.peeled.clone().unwrap_or_else(|| r.target.clone());
    let commit = state.storage.commit(p.id, &sha).await?.map(|c| j::commit(&state.cfg, p, &c));
    Ok(json!({
        "name": name,
        "message": null,
        "target": r.target,
        "commit": commit,
        "release": release.map(|rel| json!({ "tag_name": rel.tag_name, "description": rel.description })),
        "protected": false,
        "created_at": null,
    }))
}

#[derive(Deserialize, Default)]
pub struct TagsQuery {
    #[serde(default)]
    search: String,
}

pub async fn tags(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<TagsQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let mut refs: Vec<forgust_storage::RefInfo> = state
        .storage
        .refs(p.id, "refs/tags/")
        .await?
        .into_iter()
        .filter(|r| q.search.is_empty() || r.name.trim_start_matches("refs/tags/").contains(&q.search))
        .collect();
    // newest first, as GitLab orders by update time by default
    refs.sort_by_key(|r| std::cmp::Reverse(r.commit.as_ref().map(|c| c.committed_at).unwrap_or(0)));
    let total = refs.len() as i64;
    let page: Vec<_> = refs.into_iter().skip(pg.offset() as usize).take(pg.per_page() as usize).collect();
    let names: Vec<String> = page.iter().map(|r| r.name.trim_start_matches("refs/tags/").to_string()).collect();
    let releases = forgust_db::releases::for_tags(&state.db, p.id, &names).await?;
    let rows: Vec<_> = page
        .iter()
        .map(|r| tag_json(&state, &p, r, releases.iter().find(|x| x.tag_name == r.name.trim_start_matches("refs/tags/"))))
        .collect();
    let items: Vec<Value> = futures::stream::iter(rows).buffered(8).collect::<Vec<_>>().await.into_iter().collect::<ApiResult<_>>()?;
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

pub async fn tag(
    State(state): State<AppState>,
    JobOrViewer(v): JobOrViewer,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let refname = format!("refs/tags/{name}");
    let r = state.storage.refs(p.id, &refname).await?.into_iter().find(|r| r.name == refname).ok_or(ApiError::NotFound)?;
    let release = forgust_db::releases::find(&state.db, p.id, &name).await?;
    Ok(Json(tag_json(&state, &p, &r, release.as_ref()).await?))
}

#[derive(Deserialize, Default)]
pub struct V4NewTag {
    tag_name: Option<String>,
    #[serde(rename = "ref")]
    from: Option<String>,
    message: Option<String>,
}

pub async fn create_tag(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Query(q): Query<V4NewTag>,
    body: Option<Json<V4NewTag>>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let name = q.tag_name.or(b.tag_name).ok_or_else(|| ApiError::bad("tag_name is missing"))?;
    let from = q.from.or(b.from).ok_or_else(|| ApiError::bad("ref is missing"))?;
    let message = q.message.or(b.message);
    let t = crate::services::edits::create_tag(&state, &v, &p, &name, &from, message.as_deref()).await?;
    let refname = format!("refs/tags/{}", t.name);
    let r = state.storage.refs(p.id, &refname).await?.into_iter().find(|r| r.name == refname).ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(tag_json(&state, &p, &r, None).await?)).into_response())
}

pub async fn delete_tag(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    crate::services::edits::delete_tag(&state, &v, &p, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- protected tags

fn protected_tag_json(t: &forgust_db::protected_branches::ProtectedTag) -> Value {
    let desc = match t.create_access_level {
        0 => "No one",
        30 => "Developers + Maintainers",
        _ => "Maintainers",
    };
    json!({ "name": t.name, "create_access_levels": [{ "access_level": t.create_access_level, "access_level_description": desc }] })
}

pub async fn protected_tags(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let list = forgust_db::protected_branches::tags(&state.db, p.id).await?;
    Ok(Json(json!(list.iter().map(protected_tag_json).collect::<Vec<_>>())))
}

pub async fn protected_tag(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let list = forgust_db::protected_branches::tags(&state.db, p.id).await?;
    Ok(Json(protected_tag_json(list.iter().find(|t| t.name == name).ok_or(ApiError::NotFound)?)))
}

#[derive(Deserialize, Default)]
pub struct V4ProtectTag {
    name: Option<String>,
    create_access_level: Option<i16>,
}

pub async fn protect_tag(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Query(q): Query<V4ProtectTag>,
    body: Option<Json<V4ProtectTag>>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Admin).await?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let name = q.name.or(b.name).ok_or_else(|| ApiError::bad("name is missing"))?;
    let level = q.create_access_level.or(b.create_access_level).unwrap_or(40);
    crate::services::protected_refs::protect_tag(&state, &v, &p, &name, level).await?;
    let list = forgust_db::protected_branches::tags(&state.db, p.id).await?;
    let t = list.iter().find(|t| t.name == name.trim()).ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(protected_tag_json(t))).into_response())
}

pub async fn unprotect_tag(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let p = project_for(&state, &v, &id, ProjectAction::Admin).await?;
    let list = forgust_db::protected_branches::tags(&state.db, p.id).await?;
    let t = list.iter().find(|t| t.name == name).ok_or(ApiError::NotFound)?;
    crate::services::protected_refs::unprotect_tag(&state, &v, &p, t.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
