//! Releases (M7): a name and notes on a tag, with asset links (files uploaded to
//! the project, or anywhere else). GitLab's model, so `glab release` and the
//! `/api/v4` releases API work: deleting the tag deletes the release; deleting a
//! release keeps the tag.
//!
//! Developers create and change releases, and so do CI jobs of the project
//! (job tokens) whose user is a Developer. Files stay when their links go (a
//! link may point at any upload of the project, a comment's image included):
//! they are deleted with the project.

use chrono::{DateTime, Utc};
use forgust_db::projects::Project;
use forgust_db::releases::{Release, ReleaseLink};
use serde::Deserialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

const LINK_TYPES: &[&str] = &["other", "runbook", "image", "package"];
const MAX_LINKS: usize = 100;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewLink {
    pub name: String,
    /// `http(s)://` or `ftp://`, or a file uploaded to this project.
    pub url: String,
    /// Serves the link at `/-/releases/<tag>/downloads/<path>`.
    #[serde(alias = "filepath")]
    pub direct_asset_path: Option<String>,
    /// other (default), runbook, image or package
    pub link_type: Option<String>,
}

pub struct NewRelease<'a> {
    pub tag_name: &'a str,
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    /// Where to create the tag when it does not exist yet.
    pub git_ref: Option<&'a str>,
    /// Makes that new tag annotated.
    pub tag_message: Option<&'a str>,
    pub released_at: Option<DateTime<Utc>>,
    pub links: &'a [NewLink],
}

pub struct ReleaseChange<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub released_at: Option<DateTime<Utc>>,
}

/// Developer+, or a CI job of this project acting for a Developer+.
pub async fn require_release(state: &AppState, principal: &Principal, p: &Project) -> ApiResult<()> {
    crate::auth::authz::writable(p)?;
    if let Principal::Job { project_id, user, .. } = principal
        && *project_id == p.id
    {
        let level = forgust_db::authz::project_level(&state.db, user.id, p.id).await?.unwrap_or(0);
        return if level >= forgust_db::access::DEVELOPER || user.is_admin {
            Ok(())
        } else {
            Err(ApiError::forbidden("the job's user cannot create releases here"))
        };
    }
    require_project(state, principal, ProjectAction::Push, p).await
}

/// Whether the viewer may see the tagged commit and the source archives (Guests see
/// releases, not code).
pub async fn sees_code(state: &AppState, principal: &Principal, p: &Project) -> bool {
    crate::auth::authz::can_project(state, principal, ProjectAction::ReadCode, p).await.unwrap_or(false)
}

pub async fn find(state: &AppState, principal: &Principal, p: &Project, tag: &str) -> ApiResult<Release> {
    require_project(state, principal, ProjectAction::Read, p).await?;
    forgust_db::releases::find(&state.db, p.id, tag).await?.ok_or(ApiError::NotFound)
}

fn uploads_prefix(p: &Project) -> String {
    format!("/-/project/{}/uploads/", p.id)
}

/// Links to this project's uploads are kept relative (and shown absolute).
fn normalize_url(state: &AppState, p: &Project, raw: &str) -> ApiResult<String> {
    let raw = raw.trim();
    if raw.len() > 2048 {
        return Err(ApiError::bad("the link is too long"));
    }
    let base = state.cfg.server.external_url.as_str();
    let local = raw.strip_prefix(base).unwrap_or(raw);
    if local.starts_with(&uploads_prefix(p)) && !local.contains("..") {
        return Ok(local.to_string());
    }
    match url::Url::parse(raw) {
        Ok(u) if matches!(u.scheme(), "http" | "https" | "ftp") && u.host().is_some() => Ok(raw.to_string()),
        _ => Err(ApiError::bad(format!("`{raw}` is not an http(s) or ftp link, nor a file of this project"))),
    }
}

pub fn absolute_url(state: &AppState, url: &str) -> String {
    if url.starts_with('/') { state.cfg.web_url(url) } else { url.to_string() }
}

fn check_link(l: &NewLink) -> ApiResult<(String, Option<String>, String)> {
    let name = l.name.trim();
    if name.is_empty() || name.len() > 255 {
        return Err(ApiError::bad("a link needs a name of 1-255 characters"));
    }
    let kind = l.link_type.as_deref().unwrap_or("other");
    if !LINK_TYPES.contains(&kind) {
        return Err(ApiError::bad(format!("link_type must be one of {}", LINK_TYPES.join(", "))));
    }
    let path = match l.direct_asset_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(p) => {
            let p = format!("/{}", p.trim_start_matches('/'));
            if p.len() > 1024 || p.split('/').any(|seg| seg == ".." || seg == ".") || p.contains("//") || p.chars().any(char::is_control) {
                return Err(ApiError::bad(format!("`{p}` is not a valid asset path")));
            }
            Some(p)
        }
    };
    Ok((name.to_string(), path, kind.to_string()))
}

async fn insert_link(state: &AppState, p: &Project, release_id: i64, l: &NewLink) -> ApiResult<ReleaseLink> {
    let (name, path, kind) = check_link(l)?;
    let url = normalize_url(state, p, &l.url)?;
    forgust_db::releases::add_link(&state.db, release_id, &name, &url, path.as_deref(), &kind)
        .await?
        .ok_or_else(|| ApiError::conflict(format!("this release already has a link named `{name}` or at that path")))
}

pub async fn create(state: &AppState, principal: &Principal, p: &Project, req: NewRelease<'_>) -> ApiResult<Release> {
    require_release(state, principal, p).await?;
    let tag = crate::services::edits::tag_name(req.tag_name)?;
    let description = req.description.unwrap_or("");
    if description.len() > 1_000_000 {
        return Err(ApiError::bad("the description is too long"));
    }
    if req.links.len() > MAX_LINKS {
        return Err(ApiError::bad(format!("at most {MAX_LINKS} links")));
    }
    // all links checked first: a release is not left half made
    let mut seen = std::collections::HashSet::new();
    for l in req.links {
        let (name, path, _) = check_link(l)?;
        normalize_url(state, p, &l.url)?;
        if !seen.insert(format!("n:{name}")) || path.is_some_and(|p| !seen.insert(format!("p:{p}"))) {
            return Err(ApiError::bad("two links share a name or a path"));
        }
    }
    if forgust_db::releases::find(&state.db, p.id, &tag).await?.is_some() {
        return Err(ApiError::conflict(format!("release for `{tag}` already exists")));
    }
    if state.storage.resolve(p.id, &format!("refs/tags/{tag}")).await?.is_none() {
        let from = req
            .git_ref
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or_else(|| ApiError::bad(format!("tag `{tag}` does not exist: give a ref to create it from")))?;
        crate::services::edits::write_tag(state, principal, p, &tag, from, req.tag_message).await?;
    }
    let name = req.name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&tag);
    let author = principal.user().map(|u| u.id);
    let id = forgust_db::releases::create(&state.db, p.id, &tag, name, description, author, req.released_at)
        .await?
        .ok_or_else(|| ApiError::conflict(format!("release for `{tag}` already exists")))?;
    for l in req.links {
        insert_link(state, p, id, l).await?;
    }
    let r = forgust_db::releases::find(&state.db, p.id, &tag).await?.ok_or(ApiError::NotFound)?;
    emit(state, p, &r, "create").await;
    super::audit(state, principal, "release_created", Some(("project", p.id)), json!({ "tag": tag }), None).await;
    Ok(r)
}

pub async fn update(state: &AppState, principal: &Principal, p: &Project, tag: &str, c: ReleaseChange<'_>) -> ApiResult<Release> {
    require_release(state, principal, p).await?;
    let r = forgust_db::releases::find(&state.db, p.id, tag).await?.ok_or(ApiError::NotFound)?;
    let name = c.name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&r.name);
    let description = c.description.unwrap_or(&r.description);
    if description.len() > 1_000_000 || name.len() > 1000 {
        return Err(ApiError::bad("the name or description is too long"));
    }
    forgust_db::releases::update(&state.db, r.id, name, description, c.released_at.unwrap_or(r.released_at)).await?;
    let r = forgust_db::releases::find(&state.db, p.id, tag).await?.ok_or(ApiError::NotFound)?;
    emit(state, p, &r, "update").await;
    Ok(r)
}

/// The release goes, the tag stays.
pub async fn delete(state: &AppState, principal: &Principal, p: &Project, tag: &str) -> ApiResult<Release> {
    require_release(state, principal, p).await?;
    let r = forgust_db::releases::find(&state.db, p.id, tag).await?.ok_or(ApiError::NotFound)?;
    emit(state, p, &r, "delete").await;
    forgust_db::releases::delete(&state.db, r.id).await?;
    super::audit(state, principal, "release_deleted", Some(("project", p.id)), json!({ "tag": r.tag_name }), None).await;
    Ok(r)
}

pub async fn add_link(state: &AppState, principal: &Principal, p: &Project, tag: &str, l: &NewLink) -> ApiResult<ReleaseLink> {
    require_release(state, principal, p).await?;
    let r = forgust_db::releases::find(&state.db, p.id, tag).await?.ok_or(ApiError::NotFound)?;
    if forgust_db::releases::links(&state.db, &[r.id]).await?.len() >= MAX_LINKS {
        return Err(ApiError::bad(format!("at most {MAX_LINKS} links")));
    }
    insert_link(state, p, r.id, l).await
}

pub async fn delete_link(state: &AppState, principal: &Principal, p: &Project, tag: &str, link_id: i64) -> ApiResult<ReleaseLink> {
    require_release(state, principal, p).await?;
    let r = forgust_db::releases::find(&state.db, p.id, tag).await?.ok_or(ApiError::NotFound)?;
    forgust_db::releases::delete_link(&state.db, r.id, link_id).await?.ok_or(ApiError::NotFound)
}

/// Where `/-/releases/<tag>/downloads/<path>` goes.
pub async fn download_url(state: &AppState, principal: &Principal, p: &Project, tag: &str, path: &str) -> ApiResult<String> {
    let r = find(state, principal, p, tag).await?;
    let path = format!("/{}", path.trim_start_matches('/'));
    let l = forgust_db::releases::link_by_path(&state.db, r.id, &path).await?.ok_or(ApiError::NotFound)?;
    Ok(absolute_url(state, &l.url))
}

/// Tags deleted by a push (or the API): their releases go too, as in GitLab.
pub async fn on_tags_deleted(state: &AppState, p: &Project, tags: &[String]) -> anyhow::Result<()> {
    let found = forgust_db::releases::for_tags(&state.db, p.id, tags).await?;
    if found.is_empty() {
        return Ok(());
    }
    for r in &found {
        emit(state, p, r, "delete").await;
    }
    forgust_db::releases::delete_for_tags(&state.db, p.id, tags).await?;
    Ok(())
}

pub fn web_url(state: &AppState, p: &Project, tag: &str) -> String {
    state.cfg.project_page(&p.full_path, &format!("releases/{}", encode_tag(tag)))
}

/// A tag as one URL segment (`release/v1.0` → `release%2Fv1.0`).
pub fn encode_tag(tag: &str) -> String {
    crate::blob::encode_segment(tag)
}

/// GitLab's `release` hook (`object_kind: release`, action create/update/delete).
async fn emit(state: &AppState, p: &Project, r: &Release, action: &str) {
    let links = forgust_db::releases::links(&state.db, &[r.id]).await.unwrap_or_default();
    let commit = state.storage.resolve(p.id, &format!("refs/tags/{}^{{commit}}", r.tag_name)).await.ok().flatten();
    let payload: Value = json!({
        "object_kind": "release",
        "action": action,
        "id": r.id,
        "tag": r.tag_name,
        "name": r.name,
        "description": r.description,
        "created_at": r.created_at,
        "released_at": r.released_at,
        "url": web_url(state, p, &r.tag_name),
        "project": super::mr::project_json(state, p),
        "commit": { "id": commit },
        "assets": {
            "count": links.len(),
            "links": links.iter().map(|l| json!({ "id": l.id, "name": l.name, "url": absolute_url(state, &l.url), "link_type": l.link_type })).collect::<Vec<_>>(),
        },
    });
    super::webhooks::emit(state, p, "release", payload).await;
}
