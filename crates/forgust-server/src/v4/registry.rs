//! GitLab's container registry API (`/projects/:id/registry/repositories`),
//! for tools that list and prune images. Pushing and pulling go
//! through `/v2/`.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::response::Response;
use forgust_db::registry as rdb;
use serde::Deserialize;
use serde_json::{Value, json};

use super::json as j;
use super::{PageQuery, paged};
use crate::api::project_for;
use crate::auth::Viewer;
use crate::auth::authz::ProjectAction;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize, Default)]
pub struct ReposQuery {
    #[serde(default)]
    tags: bool,
    #[serde(default)]
    tags_count: bool,
}

/// `group/project[/name]`, and with this instance's registry host.
fn paths(state: &AppState, project_path: &str, name: &str) -> (String, String) {
    let path = if name.is_empty() { project_path.to_string() } else { format!("{project_path}/{name}") };
    let location = crate::registry::image_of(state, &path);
    (path, location)
}

fn tag_json(state: &AppState, project_path: &str, repo: &str, name: &str) -> Value {
    let (path, location) = paths(state, project_path, repo);
    json!({ "name": name, "path": format!("{path}:{name}"), "location": format!("{location}:{name}") })
}

/// `GET /projects/:id/registry/repositories` (`tags`, `tags_count` as GitLab).
pub async fn repositories(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<ReposQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let all = rdb::repositories(&state.db, p.id).await?;
    let total = all.len() as i64;
    let mut items = Vec::new();
    for r in all.iter().skip(pg.offset() as usize).take(pg.per_page() as usize) {
        let (path, location) = paths(&state, &p.full_path, &r.name);
        let mut o = json!({
            "id": r.id, "name": r.name, "path": path, "project_id": p.id, "location": location,
            "created_at": j::ts(&r.created_at), "cleanup_policy_started_at": null, "status": null,
        });
        if q.tags_count {
            o["tags_count"] = json!(r.tags_count);
        }
        if q.tags {
            let tags = rdb::tags(&state.db, r.id, "", 10_000).await?;
            o["tags"] = json!(tags.iter().map(|t| tag_json(&state, &p.full_path, &r.name, &t.name)).collect::<Vec<_>>());
        }
        items.push(o);
    }
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

/// `GET /projects/:id/registry/repositories/:repository_id/tags`, by name.
pub async fn tags(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path((id, repo_id)): Path<(String, i64)>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let repo = crate::api::registry::repo_of(&state, p.id, repo_id).await?;
    let all = rdb::tags(&state.db, repo.id, "", 10_000).await?;
    let total = all.len() as i64;
    let items: Vec<Value> = all
        .iter()
        .skip(pg.offset() as usize)
        .take(pg.per_page() as usize)
        .map(|t| tag_json(&state, &p.full_path, &repo.name, &t.name))
        .collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

/// `GET /projects/:id/registry/repositories/:repository_id/tags/:tag`: with its digest and size.
pub async fn tag(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, repo_id, name)): Path<(String, i64, String)>,
) -> ApiResult<axum::Json<Value>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let repo = crate::api::registry::repo_of(&state, p.id, repo_id).await?;
    // the one tag, not the repository's list
    let t = rdb::manifest_by_tag(&state.db, repo.id, &name).await?.ok_or(ApiError::NotFound)?;
    let mut o = tag_json(&state, &p.full_path, &repo.name, &name);
    let revision = t.digest.trim_start_matches("sha256:").to_string();
    o["revision"] = json!(revision);
    o["short_revision"] = json!(&revision[..revision.len().min(9)]);
    o["digest"] = json!(t.digest);
    o["created_at"] = json!(j::ts(&t.created_at));
    o["total_size"] = json!(t.total_size);
    Ok(axum::Json(o))
}
