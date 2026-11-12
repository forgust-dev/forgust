//! Merge requests, their notes and discussions.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use forgust_db::issues as idb;
use forgust_db::merge_requests::{self as mrdb, MergeRequest};
use forgust_db::projects::Project;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use super::json::{self as j, MrExtras};
use super::{Ids, Labels, PageQuery, paged, query_ids, relabel};
use crate::api::project_for;
use crate::auth::authz::ProjectAction;
use crate::auth::{Principal, SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::mr::{self, MergeMethod, MergeOptions};
use crate::state::AppState;

async fn load(state: &AppState, v: &Principal, id: &str, iid: i64) -> ApiResult<(Project, MergeRequest)> {
    let p = project_for(state, v, id, ProjectAction::ReadCode).await?;
    let m = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    Ok((p, m))
}

async fn labels_of(state: &AppState, ids: &[i64]) -> ApiResult<HashMap<i64, Vec<String>>> {
    Ok(forgust_db::group_by_id(idb::labels_of_mrs(&state.db, ids).await?.into_iter().map(|(id, l)| (id, l.name)).collect()))
}

/// An MR with what clients read most: head sha, diff refs, newest pipeline.
/// What a page of MRs needs besides each MR, read once for the page.
struct Page {
    assignees: HashMap<i64, Vec<forgust_db::people::UserBrief>>,
    reviewers: HashMap<i64, Vec<forgust_db::people::Reviewer>>,
    notes: HashMap<i64, i64>,
    diffs: HashMap<i64, mrdb::MrDiff>,
    /// Each MR's head commit (`heads`), then the newest pipeline of each.
    heads: HashMap<i64, String>,
    pipelines: HashMap<String, forgust_db::ci::Pipeline>,
}

/// Everything a page of MRs needs: database rows in a few queries for the
/// page, then the open MRs' heads from git (a few at a time), then their
/// pipelines in one query.
async fn page_of(state: &AppState, p: &Project, mrs: &[&MergeRequest]) -> ApiResult<Page> {
    let ids: Vec<i64> = mrs.iter().map(|m| m.id).collect();
    let ((assignees, reviewers), notes, diffs) = tokio::try_join!(
        crate::api::mr::people_by_mr(state, &ids),
        async { Ok::<_, ApiError>(mrdb::user_notes_counts(&state.db, &ids).await?.into_iter().collect::<HashMap<_, _>>()) },
        async { Ok::<_, ApiError>(mrdb::latest_diffs(&state.db, &ids).await?.into_iter().collect::<HashMap<_, _>>()) },
    )?;
    let lookups: Vec<_> = mrs
        .iter()
        .map(|m| {
            let recorded = diffs.get(&m.id).map(|d| d.head_sha.clone());
            async move {
                let sha = match m.state.as_str() {
                    "opened" => state.storage.resolve(p.id, &mr::head_ref(m.iid)).await?.or(recorded),
                    _ => recorded,
                };
                Ok::<_, ApiError>(sha.map(|s| (m.id, s)))
            }
        })
        .collect();
    let heads: HashMap<i64, String> = futures::stream::iter(lookups)
        .buffered(8)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<ApiResult<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    let shas: Vec<String> = heads.values().cloned().collect();
    let pipelines = forgust_db::ci::newest_pipelines_of(&state.db, p.id, &shas).await?.into_iter().map(|pl| (pl.sha.clone(), pl)).collect();
    Ok(Page { assignees, reviewers, notes, diffs, heads, pipelines })
}

async fn render(
    state: &AppState,
    p: &Project,
    m: &MergeRequest,
    labels: Vec<String>,
    status: Option<&mr::MergeStatus>,
) -> ApiResult<Value> {
    let page = page_of(state, p, &[m]).await?;
    Ok(render_in(state, p, m, labels, status, &page))
}

fn render_in(state: &AppState, p: &Project, m: &MergeRequest, labels: Vec<String>, status: Option<&mr::MergeStatus>, page: &Page) -> Value {
    let latest = page.diffs.get(&m.id);
    let sha = page.heads.get(&m.id).cloned();
    let pipeline = sha.as_ref().and_then(|s| page.pipelines.get(s));
    let x = MrExtras {
        assignees: page.assignees.get(&m.id).cloned().unwrap_or_default(),
        reviewers: page.reviewers.get(&m.id).map(|r| r.iter().map(|r| r.user.clone()).collect()).unwrap_or_default(),
        labels,
        sha: sha.clone(),
        diff_refs: latest.as_ref().map(|d| (d.base_sha.clone(), d.start_sha.clone(), d.head_sha.clone())),
        changes_count: latest.as_ref().map(|d| d.files_count),
        user_notes_count: page.notes.get(&m.id).copied().unwrap_or(0),
        pipeline: pipeline.map(|pl| j::pipeline(&state.cfg, p, pl)),
        status: status.map(|s| {
            let detailed = detailed_status(m, s);
            (if s.conflicts.is_empty() { "can_be_merged" } else { "cannot_be_merged" }, detailed, !s.conflicts.is_empty(), s.user_can_merge)
        }),
    };
    j::merge_request(&state.cfg, p, m, x)
}

/// GitLab's `detailed_merge_status` from Forgust's merge checks.
fn detailed_status(m: &MergeRequest, s: &mr::MergeStatus) -> &'static str {
    if m.state != "opened" {
        return "not_open";
    }
    if m.draft {
        return "draft_status";
    }
    if !s.conflicts.is_empty() {
        return "conflict";
    }
    if s.pipeline_required
        && matches!(s.pipeline_status.as_deref(), Some("created" | "pending" | "running" | "preparing" | "waiting_for_resource"))
    {
        return "ci_still_running";
    }
    if s.pipeline_required && s.pipeline_status.as_deref() != Some("success") {
        return "ci_must_pass";
    }
    if s.approval_rules.iter().any(|r| !r.satisfied) {
        return "not_approved";
    }
    if s.unresolved_threads > 0 {
        return "discussions_not_resolved";
    }
    if s.can_merge { "mergeable" } else { "not_open" }
}

#[derive(Deserialize)]
pub struct ListQuery {
    state: Option<String>,
    source_branch: Option<String>,
    target_branch: Option<String>,
    author_id: Option<i64>,
    author_username: Option<String>,
    assignee_id: Option<i64>,
    assignee_username: Option<String>,
    reviewer_id: Option<i64>,
    reviewer_username: Option<String>,
    scope: Option<String>,
    labels: Option<Labels>,
    #[serde(default)]
    search: String,
    updated_after: Option<DateTime<Utc>>,
    order_by: Option<String>,
    sort: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let st = match q.state.as_deref() {
        None | Some("all") => "all",
        Some("opened" | "locked") => "opened",
        Some("closed") => "closed",
        Some("merged") => "merged",
        Some(_) => return Err(ApiError::bad("state must be opened, closed, merged, locked or all")),
    };
    let author_id = match (&q.author_username, q.scope.as_deref()) {
        (_, Some("created_by_me")) => Some(v.direct_user().ok_or(ApiError::Unauthorized)?.id),
        (Some(name), _) => Some(super::user_filter(&state, name).await?),
        _ => q.author_id,
    };
    let me = || v.direct_user().map(|u| u.id).ok_or(ApiError::Unauthorized);
    let assignee_id = match (q.scope.as_deref(), &q.assignee_username) {
        (Some("assigned_to_me"), _) => Some(me()?),
        (_, Some(name)) => Some(super::user_filter(&state, name).await?),
        _ => q.assignee_id,
    };
    let reviewer_id = match &q.reviewer_username {
        Some(name) => Some(super::user_filter(&state, name).await?),
        None => q.reviewer_id,
    };
    let wanted = q.labels.as_ref().map(Labels::names).unwrap_or_default();
    // label filters apply after the query: fetch more, then trim
    let (limit, offset) = if wanted.is_empty() { (pg.probe(), pg.offset()) } else { (2000, 0) };
    let list = mrdb::list_filtered(
        &state.db,
        p.id,
        mrdb::ListFilter {
            state: st,
            source_branch: q.source_branch.as_deref(),
            target_branch: q.target_branch.as_deref(),
            author_id,
            assignee_id,
            reviewer_id,
            updated_after: q.updated_after,
            search: q.search.trim(),
            order_by: q.order_by.as_deref().unwrap_or("created_at"),
            ascending: q.sort.as_deref() == Some("asc"),
            limit,
            offset,
        },
    )
    .await?;
    let mut labels = labels_of(&state, &list.iter().map(|m| m.id).collect::<Vec<_>>()).await?;
    let mut picked: Vec<(MergeRequest, Vec<String>)> = list
        .into_iter()
        .map(|m| {
            let l = labels.remove(&m.id).unwrap_or_default();
            (m, l)
        })
        .filter(|(_, l)| wanted.iter().all(|w| l.contains(w)))
        .collect();
    if !wanted.is_empty() {
        picked = picked.into_iter().skip(pg.offset() as usize).take(pg.probe() as usize).collect();
    }
    let page = page_of(&state, &p, &picked.iter().map(|(m, _)| m).collect::<Vec<_>>()).await?;
    let items: Vec<Value> = picked.iter().map(|(m, l)| render_in(&state, &p, m, l.clone(), None, &page)).collect();
    Ok(paged(&state, &uri, &pg, items, None))
}

pub async fn show(State(state): State<AppState>, Viewer(v): Viewer, Path((id, iid)): Path<(String, i64)>) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let labels = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
    let status = if m.state == "opened" && v.direct_user().is_some() { mr::merge_status(&state, &v, &p, &m).await.ok() } else { None };
    Ok(Json(render(&state, &p, &m, labels, status.as_ref()).await?))
}

#[derive(Deserialize)]
pub struct CreateBody {
    source_branch: String,
    target_branch: Option<String>,
    title: String,
    #[serde(default)]
    description: String,
    labels: Option<Labels>,
    #[serde(default)]
    squash: Option<bool>,
    #[serde(default)]
    draft: Option<bool>,
    target_project_id: Option<i64>,
    remove_source_branch: Option<bool>,
    assignee_ids: Option<Ids>,
    assignee_id: Option<Ids>,
    reviewer_ids: Option<Ids>,
    milestone_id: Option<i64>,
}

/// Set labels by name, creating the missing ones (as GitLab does).
async fn label(state: &AppState, v: &Principal, p: &Project, m: &MergeRequest, names: Vec<String>) -> ApiResult<()> {
    crate::services::issues::ensure_labels(state, v, p, &names).await?;
    mr::set_labels(state, v, p, m, &names).await?;
    Ok(())
}

pub async fn create(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path(id): Path<String>,
    Json(req): Json<CreateBody>,
) -> ApiResult<Response> {
    let p = project_for(&state, &v, &id, ProjectAction::Push).await?;
    if req.target_project_id.is_some_and(|t| t != p.id) {
        return Err(ApiError::bad("merge requests between projects (forks) are not supported"));
    }
    let target = req.target_branch.clone().or(p.default_branch.clone()).ok_or_else(|| ApiError::bad("target_branch is required"))?;
    let m = match mr::create(
        &state,
        &v,
        &p,
        mr::CreateMr {
            source_branch: &req.source_branch,
            target_branch: &target,
            title: &req.title,
            description: &req.description,
            draft: req.draft.unwrap_or(false),
            squash: req.squash.unwrap_or(false),
            stack_on: None,
            remove_source_branch: req.remove_source_branch.unwrap_or(false),
        },
    )
    .await
    {
        Ok(m) => m,
        // GitLab answers 409 with this wording for a duplicate
        Err(ApiError::Conflict(msg)) => {
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({ "message": [format!("Another open merge request already exists for this source branch: {msg}")] })),
            )
                .into_response());
        }
        Err(e) => return Err(e),
    };
    if let Some(l) = &req.labels {
        label(&state, &v, &p, &m, l.names()).await?;
    }
    let assignees = req.assignee_ids.as_ref().or(req.assignee_id.as_ref()).map(Ids::ids);
    let reviewers = req.reviewer_ids.as_ref().map(Ids::ids);
    crate::services::people::set_mr(&state, &v, &p, &m, assignees.as_deref(), reviewers.as_deref()).await?;
    if req.milestone_id.is_some() {
        crate::services::milestones::set_merge_request(&state, &v, &p, &m, req.milestone_id).await?;
    }
    let m = mrdb::find(&state.db, p.id, m.iid).await?.ok_or(ApiError::NotFound)?;
    let labels = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
    Ok((StatusCode::CREATED, Json(render(&state, &p, &m, labels, None).await?)).into_response())
}

#[derive(Deserialize, Default)]
pub struct UpdateBody {
    title: Option<String>,
    description: Option<String>,
    state_event: Option<String>,
    target_branch: Option<String>,
    labels: Option<Labels>,
    add_labels: Option<Labels>,
    remove_labels: Option<Labels>,
    draft: Option<bool>,
    assignee_ids: Option<Ids>,
    assignee_id: Option<Ids>,
    reviewer_ids: Option<Ids>,
    remove_source_branch: Option<bool>,
    #[serde(default, deserialize_with = "super::nullable")]
    milestone_id: Option<Option<i64>>,
}

/// PUT .../merge_requests/:iid; Renovate sends assignees in the query, with no body.
pub async fn update(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    OriginalUri(uri): OriginalUri,
    Path((id, iid)): Path<(String, i64)>,
    body: Option<Json<UpdateBody>>,
) -> ApiResult<Json<Value>> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let (p, m) = load(&state, &v, &id, iid).await?;
    let assignees = req.assignee_ids.as_ref().or(req.assignee_id.as_ref()).map(Ids::ids).or_else(|| query_ids(&uri, "assignee_ids"));
    let reviewers = req.reviewer_ids.as_ref().map(Ids::ids).or_else(|| query_ids(&uri, "reviewer_ids"));
    crate::services::people::set_mr(&state, &v, &p, &m, assignees.as_deref(), reviewers.as_deref()).await?;
    if let Some(r) = req.remove_source_branch {
        mr::may_edit(&state, &v, &p, &m).await?;
        mrdb::set_remove_source_branch(&state.db, m.id, r).await?;
    }
    if let Some(ms) = req.milestone_id {
        crate::services::milestones::set_merge_request(&state, &v, &p, &m, Some(ms.unwrap_or(0))).await?;
    }
    if req.title.is_some() || req.description.is_some() || req.target_branch.is_some() || req.draft.is_some() {
        let change = mr::DetailsChange {
            title: req.title.as_deref(),
            description: req.description.as_deref(),
            draft: req.draft,
            target_branch: req.target_branch.as_deref(),
        };
        mr::update_details(&state, &v, &p, &m, change).await?;
    }
    match req.state_event.as_deref() {
        Some("close") => {
            mr::set_open(&state, &v, &p, &m, false).await?;
        }
        Some("reopen") => {
            mr::set_open(&state, &v, &p, &m, true).await?;
        }
        Some(_) => return Err(ApiError::bad("state_event must be close or reopen")),
        None => {}
    }
    let current = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
    if let Some(names) = relabel(&current, req.labels.as_ref(), req.add_labels.as_ref(), req.remove_labels.as_ref()) {
        label(&state, &v, &p, &m, names).await?;
    }
    let m = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let labels = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
    Ok(Json(render(&state, &p, &m, labels, None).await?))
}

#[derive(Deserialize, Default)]
pub struct TrainBody {
    squash: Option<bool>,
}

#[derive(Deserialize, Default)]
pub struct MergeBody {
    squash: Option<bool>,
    should_remove_source_branch: Option<bool>,
    merge_when_pipeline_succeeds: Option<bool>,
    auto_merge: Option<bool>,
    sha: Option<String>,
    merge_commit_message: Option<String>,
    squash_commit_message: Option<String>,
}

fn refuse(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "message": msg }))).into_response()
}

pub async fn merge(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    body: Option<Json<MergeBody>>,
) -> ApiResult<Response> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let (p, m) = load(&state, &v, &id, iid).await?;
    if m.state != "opened" {
        return Ok(refuse(StatusCode::METHOD_NOT_ALLOWED, "405 Method Not Allowed"));
    }
    let status = mr::merge_status(&state, &v, &p, &m).await?;
    if req.merge_when_pipeline_succeeds.unwrap_or(false) || req.auto_merge.unwrap_or(false) {
        if req.sha.as_deref().is_some_and(|s| Some(s) != status.head_sha.as_deref()) {
            return Ok(refuse(StatusCode::CONFLICT, "SHA does not match HEAD of source branch"));
        }
        // with merge trains on, auto-merge means joining the train (as GitLab)
        if status.merge_trains_enabled {
            let opts = crate::services::merge_train::TrainOptions {
                squash: req.squash.unwrap_or(m.squash),
                rebase: false,
                remove_source_branch: req.should_remove_source_branch.unwrap_or(m.remove_source_branch),
            };
            return match crate::services::merge_train::add(&state, &v, &p, &m, opts).await {
                Ok(_) => {
                    let m = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
                    let labels = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
                    Ok(Json(render(&state, &p, &m, labels, None).await?).into_response())
                }
                Err(ApiError::Conflict(msg) | ApiError::Forbidden(msg)) => Ok(refuse(StatusCode::METHOD_NOT_ALLOWED, &msg)),
                Err(e) => Err(e),
            };
        }
        let squash = req.squash.unwrap_or(m.squash);
        let method = if squash { MergeMethod::Squash } else { MergeMethod::Merge };
        let remove = req.should_remove_source_branch.unwrap_or(m.remove_source_branch);
        return match crate::services::auto_merge::enable(&state, &v, &p, &m, method, status.head_sha.as_deref(), remove).await {
            Ok(_) => {
                let m = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
                let labels = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
                Ok(Json(render(&state, &p, &m, labels, None).await?).into_response())
            }
            Err(ApiError::Conflict(msg) | ApiError::Forbidden(msg)) => Ok(refuse(StatusCode::METHOD_NOT_ALLOWED, &msg)),
            Err(e) => Err(e),
        };
    }
    if !status.can_merge {
        return Ok(refuse(StatusCode::METHOD_NOT_ALLOWED, &status.blockers.join("; ")));
    }
    if req.sha.as_deref().is_some_and(|s| Some(s) != status.head_sha.as_deref()) {
        return Ok(refuse(StatusCode::CONFLICT, "SHA does not match HEAD of source branch"));
    }
    let squash = req.squash.unwrap_or(m.squash);
    let message = if squash { req.squash_commit_message.as_deref() } else { req.merge_commit_message.as_deref() };
    let merged = mr::merge(
        &state,
        &v,
        &p,
        &m,
        MergeOptions {
            method: if squash { MergeMethod::Squash } else { MergeMethod::Merge },
            expected_head: status.head_sha.as_deref(),
            remove_source_branch: req.should_remove_source_branch.unwrap_or(m.remove_source_branch),
            message,
            train: None,
        },
    )
    .await;
    match merged {
        Ok(m) => {
            let labels = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
            Ok(Json(render(&state, &p, &m, labels, None).await?).into_response())
        }
        Err(ApiError::Conflict(msg)) => Ok(refuse(StatusCode::NOT_ACCEPTABLE, &msg)),
        Err(e) => Err(e),
    }
}

pub async fn approve(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    mr::approve(&state, &v, &p, &m, true).await?;
    approval_state_json(&state, &p, &m).await.map(Json)
}

pub async fn unapprove(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    mr::approve(&state, &v, &p, &m, false).await?;
    approval_state_json(&state, &p, &m).await.map(Json)
}

async fn approval_state_json(state: &AppState, p: &Project, m: &MergeRequest) -> ApiResult<Value> {
    let approvals = mrdb::approvals(&state.db, m.id, p.id).await?;
    let by: Vec<Value> = approvals.iter().map(|a| json!({ "user": { "id": a.user_id, "username": a.username } })).collect();
    Ok(
        json!({ "id": m.id, "iid": m.iid, "project_id": p.id, "approved": !approvals.is_empty(), "approved_by": by, "approvals_required": 0, "approvals_left": 0 }),
    )
}

/// Approval rules live on the project here; MRs show none of their own.
pub async fn approval_rules(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Vec<Value>>> {
    load(&state, &v, &id, iid).await?;
    Ok(Json(Vec::new()))
}

pub async fn approval_state(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let mut out = approval_state_json(&state, &p, &m).await?;
    out["approval_rules_overwritten"] = json!(false);
    out["rules"] = json!([]);
    Ok(Json(out))
}

// ------------------------------------------------------------------ notes

pub async fn notes(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path((id, iid)): Path<(String, i64)>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let (_p, m) = load(&state, &v, &id, iid).await?;
    let all = mrdb::notes(&state.db, m.id).await?;
    let total = all.len() as i64;
    let items: Vec<Value> =
        all.iter().skip(pg.offset() as usize).take(pg.per_page() as usize).map(|n| j::note(&state.cfg, n, m.iid)).collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

#[derive(Deserialize)]
pub struct NoteBody {
    body: String,
}

pub async fn create_note(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<NoteBody>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let n = mr::add_note(&state, &v, &p, &m, &req.body, None, None).await?;
    let n = mrdb::note(&state.db, m.id, n.id).await?.ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(j::note(&state.cfg, &n, m.iid))).into_response())
}

pub async fn edit_note(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid, note_id)): Path<(String, i64, i64)>,
    Json(req): Json<NoteBody>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    mr::edit_note(&state, &v, &p, &m, note_id, Some(&req.body)).await?;
    let n = mrdb::note(&state.db, m.id, note_id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(j::note(&state.cfg, &n, m.iid)))
}

pub async fn delete_note(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid, note_id)): Path<(String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    mr::edit_note(&state, &v, &p, &m, note_id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Threads: a root note and its replies; the discussion id is the root's id.
fn discussion(state: &AppState, root: &forgust_db::merge_requests::Note, replies: &[&forgust_db::merge_requests::Note], iid: i64) -> Value {
    let mut notes = vec![j::note(&state.cfg, root, iid)];
    notes.extend(replies.iter().map(|n| j::note(&state.cfg, n, iid)));
    json!({ "id": root.id.to_string(), "individual_note": replies.is_empty() && root.head_sha.is_none(), "notes": notes })
}

pub async fn discussions(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path((id, iid)): Path<(String, i64)>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let (_p, m) = load(&state, &v, &id, iid).await?;
    let all = mrdb::notes(&state.db, m.id).await?;
    let roots: Vec<_> = all.iter().filter(|n| n.parent_id.is_none()).collect();
    let total = roots.len() as i64;
    let items: Vec<Value> = roots
        .iter()
        .skip(pg.offset() as usize)
        .take(pg.per_page() as usize)
        .map(|r| {
            let replies: Vec<_> = all.iter().filter(|n| n.parent_id == Some(r.id)).collect();
            discussion(&state, r, &replies, m.iid)
        })
        .collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}

pub async fn create_discussion(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    Json(req): Json<NoteBody>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let n = mr::add_note(&state, &v, &p, &m, &req.body, None, None).await?;
    let n = mrdb::note(&state.db, m.id, n.id).await?.ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(discussion(&state, &n, &[], m.iid))).into_response())
}

pub async fn reply(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid, discussion_id)): Path<(String, i64, String)>,
    Json(req): Json<NoteBody>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let root: i64 = discussion_id.parse().map_err(|_| ApiError::NotFound)?;
    let n = mr::add_note(&state, &v, &p, &m, &req.body, Some(root), None).await?;
    let n = mrdb::note(&state.db, m.id, n.id).await?.ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(j::note(&state.cfg, &n, m.iid))).into_response())
}

/// POST .../merge_requests/:iid/cancel_merge_when_pipeline_succeeds
pub async fn cancel_auto_merge(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    if m.auto_merge_by.is_none() {
        return Ok(refuse(StatusCode::NOT_ACCEPTABLE, "Can't cancel the automatic merge"));
    }
    crate::services::auto_merge::disable(&state, &v, &p, &m).await?;
    let m = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let labels = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
    Ok((StatusCode::CREATED, Json(render(&state, &p, &m, labels, None).await?)).into_response())
}

// ------------------------------------------------------------------ merge trains

async fn car_json(state: &AppState, p: &Project, c: &forgust_db::merge_trains::Car, position: usize) -> ApiResult<Value> {
    let base = &state.cfg.server.external_url;
    let user = match c.user_id {
        Some(u) => forgust_db::users::find(&state.db, u).await?.map(|u| j::user(&state.cfg, &u)),
        None => None,
    };
    let pipeline = match c.pipeline_id {
        Some(pid) => forgust_db::ci::pipeline(&state.db, pid).await?.map(|pl| j::pipeline(&state.cfg, p, &pl)),
        None => None,
    };
    // GitLab's car states: the first car "fresh" once built, the others "idle" until then
    let status = if c.train_sha.is_none() {
        "idle"
    } else if position == 0 {
        "fresh"
    } else {
        "idle"
    };
    Ok(json!({
        "id": c.id,
        "merge_request": {
            "id": c.merge_request_id, "iid": c.iid, "project_id": p.id, "title": c.title, "state": "opened",
            "web_url": format!("{base}/{}/-/merge_requests/{}", p.full_path, c.iid),
        },
        "user": user,
        "pipeline": pipeline,
        "created_at": j::ts(&c.created_at),
        "updated_at": j::ts(&c.updated_at),
        "target_branch": c.target_branch,
        "status": status,
        "merged_at": null,
        "duration": null,
    }))
}

async fn train_json(state: &AppState, p: &Project, cars: &[forgust_db::merge_trains::Car]) -> ApiResult<Vec<Value>> {
    let mut out = Vec::new();
    let mut pos: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for c in cars {
        let i = pos.entry(c.target_branch.as_str()).or_insert(0);
        out.push(car_json(state, p, c, *i).await?);
        *i += 1;
    }
    Ok(out)
}

/// `GET /projects/:id/merge_trains`: every active car (merged ones are not kept).
pub async fn merge_trains(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<Vec<Value>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let cars = forgust_db::merge_trains::cars(&state.db, p.id, None).await?;
    Ok(Json(train_json(&state, &p, &cars).await?))
}

/// `GET /projects/:id/merge_trains/:target_branch`
pub async fn merge_train(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, target)): Path<(String, String)>,
) -> ApiResult<Json<Vec<Value>>> {
    let p = project_for(&state, &v, &id, ProjectAction::ReadCode).await?;
    let cars = forgust_db::merge_trains::cars(&state.db, p.id, Some(&target)).await?;
    Ok(Json(train_json(&state, &p, &cars).await?))
}

/// `GET /projects/:id/merge_trains/merge_requests/:iid`
pub async fn merge_train_car(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let car = forgust_db::merge_trains::of_mr(&state.db, m.id).await?.ok_or(ApiError::NotFound)?;
    let cars = forgust_db::merge_trains::cars(&state.db, p.id, Some(&car.target_branch)).await?;
    let pos = cars.iter().position(|c| c.id == car.id).unwrap_or(0);
    Ok(Json(car_json(&state, &p, &car, pos).await?))
}

/// `POST /projects/:id/merge_trains/merge_requests/:iid`: join the train;
/// answers with the train.
pub async fn add_to_merge_train(
    State(state): State<AppState>,
    SignedIn(_, v): SignedIn,
    Path((id, iid)): Path<(String, i64)>,
    body: Option<Json<TrainBody>>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let opts = crate::services::merge_train::TrainOptions {
        squash: b.squash.unwrap_or(m.squash),
        rebase: false,
        remove_source_branch: m.remove_source_branch,
    };
    match crate::services::merge_train::add(&state, &v, &p, &m, opts).await {
        Ok(car) => {
            let cars = forgust_db::merge_trains::cars(&state.db, p.id, Some(&car.target_branch)).await?;
            Ok((StatusCode::CREATED, Json(train_json(&state, &p, &cars).await?)).into_response())
        }
        Err(ApiError::Conflict(msg)) => Ok(refuse(StatusCode::BAD_REQUEST, &msg)),
        Err(e) => Err(e),
    }
}

/// One file of a diff as GitLab's `changes` and `diffs` give it: the unified
/// diff text of its hunks.
fn change_json(f: &forgust_storage::DiffFile) -> Value {
    use forgust_storage::DiffLineKind;
    let mut diff = String::new();
    for h in &f.hunks {
        diff.push_str(&h.header);
        diff.push('\n');
        for l in &h.lines {
            match l.kind {
                DiffLineKind::Context => diff.push(' '),
                DiffLineKind::Add => diff.push('+'),
                DiffLineKind::Del => diff.push('-'),
                DiffLineKind::NoNewline => {
                    diff.push_str("\\ No newline at end of file\n");
                    continue;
                }
            }
            diff.push_str(&l.text);
            diff.push('\n');
        }
    }
    json!({
        "old_path": f.old_path, "new_path": f.new_path,
        "a_mode": f.old_mode.clone().unwrap_or_else(|| "0".into()), "b_mode": f.new_mode.clone().unwrap_or_else(|| "0".into()),
        "new_file": f.status == "added", "renamed_file": f.status == "renamed", "deleted_file": f.status == "deleted",
        "generated_file": false, "too_large": f.collapsed, "diff": diff,
    })
}

/// Files per answer, as GitLab's default limit for `changes`.
const MAX_CHANGES: usize = 1000;

/// `GET .../merge_requests/:iid/changes`: the MR with its latest version's files.
pub async fn changes(State(state): State<AppState>, Viewer(v): Viewer, Path((id, iid)): Path<(String, i64)>) -> ApiResult<Json<Value>> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let labels = labels_of(&state, &[m.id]).await?.remove(&m.id).unwrap_or_default();
    let mut out = render(&state, &p, &m, labels, None).await?;
    let (files, total) = match mrdb::latest_diff(&state.db, m.id).await? {
        Some(d) => {
            let opts = forgust_storage::DiffOptions { offset: 0, limit: MAX_CHANGES, ..Default::default() };
            let diff = state.storage.diff(p.id, Some(&d.base_sha), &d.head_sha, opts).await?;
            (diff.files.iter().map(change_json).collect::<Vec<_>>(), diff.total_files)
        }
        None => (Vec::new(), 0),
    };
    out["changes_count"] = json!(if total > MAX_CHANGES { format!("{MAX_CHANGES}+") } else { total.to_string() });
    out["overflow"] = json!(total > MAX_CHANGES);
    out["changes"] = json!(files);
    Ok(Json(out))
}

/// `GET .../merge_requests/:iid/diffs`: the same files, paged.
pub async fn diffs(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path((id, iid)): Path<(String, i64)>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let Some(d) = mrdb::latest_diff(&state.db, m.id).await? else { return Ok(paged(&state, &uri, &pg, Vec::<Value>::new(), Some(0))) };
    let opts = forgust_storage::DiffOptions { offset: pg.offset() as usize, limit: pg.per_page() as usize, ..Default::default() };
    let diff = state.storage.diff(p.id, Some(&d.base_sha), &d.head_sha, opts).await?;
    let files: Vec<Value> = diff.files.iter().map(change_json).collect();
    Ok(paged(&state, &uri, &pg, files, Some(diff.total_files as i64)))
}

/// `GET .../merge_requests/:iid/commits`: the latest version's commits, newest first.
pub async fn commits(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    OriginalUri(uri): OriginalUri,
    Path((id, iid)): Path<(String, i64)>,
    Query(pg): Query<PageQuery>,
) -> ApiResult<Response> {
    let (p, m) = load(&state, &v, &id, iid).await?;
    let Some(d) = mrdb::latest_diff(&state.db, m.id).await? else { return Ok(paged(&state, &uri, &pg, Vec::<Value>::new(), Some(0))) };
    let all = state.storage.log_between(p.id, &d.base_sha, &d.head_sha, 10_000).await?;
    let total = all.len() as i64;
    let items: Vec<Value> =
        all.iter().skip(pg.offset() as usize).take(pg.per_page() as usize).map(|c| j::commit(&state.cfg, &p, c)).collect();
    Ok(paged(&state, &uri, &pg, items, Some(total)))
}
