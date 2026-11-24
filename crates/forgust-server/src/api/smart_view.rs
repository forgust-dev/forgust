//! The Smart View: a project's code as blocks (what is built, run or deployed apart: a
//! package, an image, a service, a database's schema, the CI, the docs), the links between
//! them and the people to ask. assay finds them (the forgust-assay crate) in a CI job Forgust
//! runs itself when the Smart View is on for the project (`services::smart_view`); what is
//! answered here is the latest map kept, with what only the server knows put in when it is
//! read: whose account a committer is, and who reviewed each block's changes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, Utc};
use forgust_db::merge_requests::{self as mrdb, MergeRequest};
use forgust_db::people as pdb;
use forgust_db::projects::Project;
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::project_for;
use super::repo::readable;
use crate::auth::authz::ProjectAction;
use crate::auth::{Admin, SignedIn, Viewer};
use crate::error::{ApiError, ApiResult};
use crate::services::smart_view as svc;
use crate::state::AppState;
use forgust_db::smart_view as db;

/// The merged merge requests whose reviewers count, at most: the latest; their changes are read
/// a few at a time.
const MAX_REVIEWED: i64 = 300;
const DIFFS_AT_ONCE: usize = 8;
/// The reviewers shown per block.
const SHOWN_REVIEWERS: usize = 5;
/// Who a merge request is suggested to: at most two per block, and five in all.
const SUGGESTED_PER_BLOCK: usize = 2;
const MAX_SUGGESTED: usize = 5;

/// The map of a commit: its blocks, the links between them, and who knows each.
#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct RepoMap {
    /// The commit mapped.
    pub commit: String,
    /// The branch it is the head of: the default one, or a merge request's source.
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub blocks: Vec<MapBlock>,
    /// From the block that needs to the one needed.
    pub links: Vec<MapLink>,
    /// How many files are in no block (READMEs, a workspace's manifest…), and the first ones.
    pub unassigned: usize,
    pub unassigned_sample: Vec<String>,
    /// What could not be read: a manifest that does not parse, a block assay.toml names but
    /// that is not there…
    pub warnings: Vec<String>,
    /// Why assay.toml could not be read: nothing is mapped until it is fixed.
    pub config_error: Option<String>,
    /// The repository has an assay.toml at its top (corrections go there, else in a new one).
    pub config_file: bool,
    /// The days of history that tell who knows a block.
    pub days: u32,
}

/// A part of the repository built, run or deployed apart.
#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct MapBlock {
    /// Stable while its name is: what assay.toml names it by.
    pub id: String,
    pub name: String,
    /// What it is for, in a line: its manifest's (a package's description, an image's
    /// `org.opencontainers.image.description` label), or assay.toml's.
    pub description: Option<String>,
    pub kind: MapKind,
    /// A guess from its manifest (its dependencies, a binary) or its image, which assay.toml
    /// corrects.
    pub role: Option<MapRole>,
    /// The folders or files whose files are its own.
    pub roots: Vec<MapPath>,
    /// The files (or a folder of them) that declare it.
    pub manifests: Vec<MapPath>,
    /// The block it is part of (a database's schema, in its crate).
    pub parent: Option<String>,
    /// The image a service runs.
    pub image: Option<String>,
    pub files: usize,
    /// Code first, then the most files.
    pub languages: Vec<MapLanguage>,
    /// The CI jobs that build or test it.
    pub jobs: Vec<String>,
    /// Its CODEOWNERS owners, then assay.toml's referents: `@user`, `@group` or an address.
    pub owners: Vec<String>,
    /// Who changed it lately, most commits first: people to ask, never a score.
    pub people: Vec<MapPerson>,
    /// Who reviewed its changes in the merge requests merged lately, most reviews first.
    pub reviewers: Vec<MapReviewer>,
    /// How many people made at least a fifth of its recent commits: 1, only one knows it.
    pub bus_factor: Option<usize>,
    /// Declared by assay.toml rather than found.
    pub configured: bool,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MapKind {
    Rust,
    Node,
    Python,
    Go,
    Docker,
    Service,
    Helm,
    Kubernetes,
    Ansible,
    Terraform,
    Pulumi,
    Migrations,
    Ci,
    Docs,
    Other,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MapRole {
    Web,
    Api,
    Cli,
    Library,
    Database,
    Cache,
    Queue,
    Search,
    Storage,
    Mail,
}

/// A path of the repository: `.` is its top.
#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct MapPath {
    pub path: String,
    /// A folder, not a file.
    pub dir: bool,
}

#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct MapLanguage {
    pub name: String,
    pub files: usize,
    /// `#rrggbb`, as the repository's language bar shows it; none for prose and data (JSON…).
    pub color: Option<String>,
}

#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct MapPerson {
    pub name: String,
    /// Their account, by their verified email.
    pub username: Option<String>,
    /// Their commits there, within the days of history read.
    pub commits: usize,
    /// The day of their latest one (`YYYY-MM-DD`).
    pub last: String,
}

/// Someone who reviewed a block's changes: approved, reviewed or commented a merge request that
/// changed it, not as its author.
#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct MapReviewer {
    pub username: String,
    pub name: String,
    /// The merge requests they reviewed there, within the days of history read.
    pub reviews: usize,
    /// The day of the latest (`YYYY-MM-DD`).
    pub last: String,
}

/// `from` needs `to`.
#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct MapLink {
    pub from: String,
    pub to: String,
    pub kind: MapLinkKind,
    /// A guess (a service's environment names the other as a host, a chart or a manifest names
    /// an image by its name), not declared.
    pub inferred: bool,
    /// The file that says so.
    pub via: String,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MapLinkKind {
    Uses,
    Builds,
    Runs,
    DependsOn,
    Connects,
    Calls,
}

/// The Smart View of a project: its latest map, and what is going on with a newer one.
#[derive(Serialize, ToSchema)]
pub struct SmartView {
    /// On for the project, and on the instance: without it there is no map.
    pub enabled: bool,
    /// The latest map kept: the merge request's when one was asked and it has one, else the
    /// default branch's. None until a first job has run.
    pub map: Option<RepoMap>,
    /// A commit newer than the map's being mapped, or the last one that could not be.
    pub run: Option<MapRun>,
}

#[derive(Serialize, ToSchema)]
pub struct MapRun {
    pub commit: String,
    pub state: MapRunState,
    /// The pipeline that maps it.
    pub pipeline_iid: Option<i64>,
    /// Why it could not be mapped.
    pub error: Option<String>,
}

#[derive(Serialize, ToSchema, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MapRunState {
    Pending,
    Failed,
}

#[derive(Deserialize)]
pub struct MapQuery {
    /// A merge request's iid: its head's map, when it has one.
    pub merge_request: Option<i64>,
}

/// The Smart View of the default branch, or of a merge request's head: the latest map a job
/// of Forgust's made of it.
#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/map", params(("merge_request" = Option<i64>, Query, description = "A merge request's iid: its head's map, when it has one")), responses((status = 200, body = SmartView)), tag = "repository")]
pub async fn map(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<MapQuery>,
) -> ApiResult<Json<SmartView>> {
    let p = readable(&state, &v, &id).await?;
    if !svc::is_on(&state, p.id).await? {
        return Ok(Json(SmartView { enabled: false, map: None, run: None }));
    }
    let mr_id = match q.merge_request {
        Some(iid) => mrdb::find(&state.db, p.id, iid).await?.map(|mr| mr.id),
        None => None,
    };
    let map = kept_or_default(&state, &p, mr_id).await?.map(|m| m.map.clone());
    // what goes on: with the merge request's own map once a job of its head was started
    let mut latest = None;
    if mr_id.is_some() {
        latest = db::latest(&state.db, p.id, mr_id).await?;
    }
    if latest.is_none() {
        latest = db::latest(&state.db, p.id, None).await?;
    }
    let run = match latest.filter(|r| r.state != "ready") {
        Some(r) => {
            let pipeline_iid = match r.pipeline_id {
                Some(id) => forgust_db::ci::pipeline(&state.db, id).await?.map(|pl| pl.iid),
                None => None,
            };
            let state = if r.state == "failed" { MapRunState::Failed } else { MapRunState::Pending };
            Some(MapRun { commit: r.sha, state, pipeline_iid, error: r.error })
        }
        None => None,
    };
    Ok(Json(SmartView { enabled: true, map, run }))
}

/// The latest map kept of the default branch (`merge_request_id` None) or of a merge request,
/// as the Smart View shows it ([`crate::state::SmartViewMaps`]): its report is read, and the
/// accounts and reviewers put in, once for those who ask at the same time.
async fn kept(state: &AppState, p: &Project, merge_request_id: Option<i64>) -> ApiResult<Option<Arc<ShownMap>>> {
    let Some(row) = db::latest_ready(&state.db, p.id, merge_request_id).await? else { return Ok(None) };
    let shown = state
        .smart_view_maps
        .try_get_with(row.id, async {
            let report = db::report(&state.db, row.id).await?.ok_or_else(|| anyhow::anyhow!("the map went while it was read"))?;
            anyhow::Ok(Arc::new(present(state, p, &row, serde_json::from_value(report)?).await))
        })
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e:#}")))?;
    Ok(Some(shown))
}

/// The merge request's map, or the default branch's until a job mapped its head.
async fn kept_or_default(state: &AppState, p: &Project, merge_request_id: Option<i64>) -> ApiResult<Option<Arc<ShownMap>>> {
    match kept(state, p, merge_request_id).await? {
        None if merge_request_id.is_some() => kept(state, p, None).await,
        m => Ok(m),
    }
}

/// [`kept_or_default`] when the Smart View is on: what a file's and a merge request's blocks
/// are read from.
async fn kept_if_on(state: &AppState, p: &Project, merge_request_id: Option<i64>) -> ApiResult<Option<Arc<ShownMap>>> {
    if !svc::is_on(state, p.id).await? {
        return Ok(None);
    }
    kept_or_default(state, p, merge_request_id).await
}

/// A map as the Smart View shows it, and what tells a path's block as assay does.
pub struct ShownMap {
    pub map: RepoMap,
    /// The files left out of every block (assay's defaults: `vendor/`, `fixtures/`…, then
    /// assay.toml's `exclude`): in none, whatever root holds them.
    left_out: forgust_assay::Patterns,
}

impl ShownMap {
    /// The block a file or a folder is in, if any.
    fn block_of(&self, path: &str) -> Option<&MapBlock> {
        if self.left_out.matches(path) { None } else { block_of(&self.map, path) }
    }
}

/// The block a path is in: the one whose root holds it the deepest, as assay finds it.
fn block_of<'a>(map: &'a RepoMap, path: &str) -> Option<&'a MapBlock> {
    let holds = |root: &str| root == "." || path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'));
    let depth = |root: &str| if root == "." { 0 } else { root.len() };
    let mut best: Option<(&MapBlock, usize)> = None;
    for b in &map.blocks {
        for r in b.roots.iter().filter(|r| holds(&r.path)) {
            if best.is_none_or(|(_, d)| depth(&r.path) > d) {
                best = Some((b, depth(&r.path)));
            }
        }
    }
    best.map(|(b, _)| b)
}

/// The blocks a merge request changes, with who knows them.
#[derive(Serialize, ToSchema)]
pub struct MrBlocks {
    /// The commit of the map they are read from: its head once a job mapped it, else the
    /// default branch's (empty without a map: the Smart View is off, or no job has run yet).
    pub commit: String,
    /// The most changed first.
    pub blocks: Vec<MrBlock>,
    /// How many of its changed files are in no block.
    pub unassigned: usize,
    /// Who knows these blocks and could review it: its blocks' owners, then their reviewers, then
    /// who changed them; not its author nor a reviewer yet.
    pub suggested_reviewers: Vec<pdb::UserBrief>,
    /// What it changes to the map: none until a job mapped its head (or when it is the default
    /// branch's commit).
    pub changes: Option<MapChanges>,
}

/// What a merge request changes to the map: its head's against the default branch's, only what
/// the files it changes tell (a manifest, the file a link is read in, assay.toml). What the
/// default branch changed since the merge request left it is not the merge request's.
#[derive(Serialize, ToSchema, Default)]
pub struct MapChanges {
    pub blocks_added: Vec<MapBlock>,
    pub blocks_removed: Vec<MapBlock>,
    /// A block whose kind or role changes: as it is at the head.
    pub blocks_changed: Vec<BlockChange>,
    /// A block found again under another id: once merged, as the history tells it.
    pub blocks_renamed: Vec<ChapterRename>,
    pub links_added: Vec<MapLink>,
    pub links_removed: Vec<MapLink>,
}

#[derive(Serialize, ToSchema)]
pub struct BlockChange {
    pub block: MapBlock,
    pub kind_before: MapKind,
    pub role_before: Option<MapRole>,
}

/// [`MapChanges`] from the default branch's map (`base`) to the head's, of a merge request
/// changing `changed`.
fn map_changes(base: &RepoMap, head: &RepoMap, changed: &[String]) -> MapChanges {
    let touched = |path: &str| changed.iter().any(|c| c == path || c.strip_prefix(path).is_some_and(|r| r.starts_with('/')));
    let config = touched(forgust_assay::CONFIG_FILE);
    // a block is the merge request's when it changes one of its manifests, or assay.toml says it
    let its_block = |b: &MapBlock| b.manifests.iter().any(|m| touched(&m.path)) || ((b.configured || b.manifests.is_empty()) && config);
    let its_link = |via: &str| touched(via);
    let key = |l: &MapLink| (l.from.clone(), l.to.clone(), l.kind);
    let mut out = MapChanges::default();
    for b in &head.blocks {
        match base.blocks.iter().find(|x| x.id == b.id) {
            None if its_block(b) => out.blocks_added.push(b.clone()),
            Some(before) if its_block(b) && (before.kind != b.kind || before.role != b.role) => {
                out.blocks_changed.push(BlockChange { block: b.clone(), kind_before: before.kind, role_before: before.role })
            }
            _ => {}
        }
    }
    out.blocks_removed = base.blocks.iter().filter(|b| !head.blocks.iter().any(|x| x.id == b.id) && its_block(b)).cloned().collect();
    let base_links: HashSet<_> = base.links.iter().map(key).collect();
    let head_links: HashSet<_> = head.links.iter().map(key).collect();
    out.links_added = head.links.iter().filter(|l| !base_links.contains(&key(l)) && its_link(&l.via)).cloned().collect();
    out.links_removed = base.links.iter().filter(|l| !head_links.contains(&key(l)) && its_link(&l.via)).cloned().collect();
    out
}

#[derive(Serialize, ToSchema)]
pub struct MrBlock {
    pub block: MapBlock,
    /// Its files the merge request changes.
    pub files: usize,
}

/// The blocks of the Smart View a merge request changes: of its head's map, or the default
/// branch's until a job mapped it.
#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/blocks", responses((status = 200, body = MrBlocks)), tag = "merge_requests")]
pub async fn mr_blocks(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<MrBlocks>> {
    let p = readable(&state, &v, &id).await?;
    let mr = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let none =
        || Json(MrBlocks { commit: String::new(), blocks: Vec::new(), unassigned: 0, suggested_reviewers: Vec::new(), changes: None });
    let Some(map) = kept_if_on(&state, &p, Some(mr.id)).await? else { return Ok(none()) };
    let Some(diff) = mrdb::latest_diff(&state.db, mr.id).await? else { return Ok(none()) };
    let changed = state.storage.changed_paths(p.id, None, &diff.base_sha, &diff.head_sha).await?;
    let mut files: HashMap<&str, usize> = HashMap::new();
    let mut unassigned = 0;
    for path in &changed {
        match map.block_of(path) {
            Some(b) => *files.entry(b.id.as_str()).or_default() += 1,
            None => unassigned += 1,
        }
    }
    let mut blocks: Vec<MrBlock> =
        map.map.blocks.iter().filter_map(|b| files.get(b.id.as_str()).map(|n| MrBlock { block: b.clone(), files: *n })).collect();
    blocks.sort_by_key(|t| std::cmp::Reverse(t.files));
    let suggested_reviewers = suggest(&state, &p, &mr, &blocks).await?;
    // its own map against the default branch's, once both are there; merged, what the history
    // tells it changed (its own map goes, and the default branch's has its changes already)
    let changes = if mr.state == "merged" {
        match kept(&state, &p, None).await? {
            Some(now) => merged_changes(&state, &p, &mr, &now.map).await?,
            None => None,
        }
    } else {
        match (kept(&state, &p, Some(mr.id)).await?, kept(&state, &p, None).await?) {
            (Some(own), Some(base)) if own.map.commit != base.map.commit => Some(map_changes(&base.map, &own.map, &changed)),
            _ => None,
        }
    };
    Ok(Json(MrBlocks { commit: map.map.commit.clone(), blocks, unassigned, suggested_reviewers, changes }))
}

/// What a merged merge request changed to the map, as the default branch's history tells it: the
/// changes of the commits it merged, each block as the map has it now (else as the history knew
/// it). None when the history has none of it.
async fn merged_changes(state: &AppState, p: &Project, mr: &MergeRequest, now: &RepoMap) -> ApiResult<Option<MapChanges>> {
    if !db::history(&state.db, p.id).await?.is_some_and(|h| p.default_branch.as_deref() == Some(h.git_ref.as_str())) {
        return Ok(None);
    }
    let block = |b: forgust_assay::BlockRef| now.blocks.iter().find(|x| x.id == b.id).cloned().unwrap_or_else(|| bare(b));
    let mut out = MapChanges::default();
    let mut any = false;
    for c in db::chapters(&state.db, p.id).await?.into_iter().filter(|c| c.mr_iid == Some(mr.iid)) {
        // one a newer assay wrote, which this server does not read, tells nothing
        let Ok(ch) = serde_json::from_value::<forgust_assay::Changes>(c.changes) else { continue };
        any = true;
        out.blocks_added.extend(ch.blocks_added.into_iter().map(block));
        out.blocks_removed.extend(ch.blocks_removed.into_iter().map(block));
        out.blocks_renamed.extend(ch.blocks_renamed.into_iter().map(chapter_rename));
        // a kind told as such: the role the block has now is not the one it had
        out.blocks_changed.extend(ch.kinds_changed.into_iter().map(|k| BlockChange {
            kind_before: kind(k.from),
            role_before: None,
            block: MapBlock { role: None, ..block(bare_of(&k.id, k.to)) },
        }));
        out.links_added.extend(ch.links_added.into_iter().map(map_link));
        out.links_removed.extend(ch.links_removed.into_iter().map(map_link));
    }
    Ok(any.then_some(out))
}

/// A block the map no longer has, as the history knew it.
fn bare(b: forgust_assay::BlockRef) -> MapBlock {
    MapBlock {
        id: b.id,
        name: b.name,
        description: None,
        kind: kind(b.kind),
        role: b.role.map(role),
        roots: Vec::new(),
        manifests: Vec::new(),
        parent: None,
        image: None,
        files: 0,
        languages: Vec::new(),
        jobs: Vec::new(),
        owners: Vec::new(),
        people: Vec::new(),
        reviewers: Vec::new(),
        bus_factor: None,
        configured: false,
    }
}

/// A block known by its id and kind alone, named by its id.
fn bare_of(id: &str, k: forgust_assay::Kind) -> forgust_assay::BlockRef {
    forgust_assay::BlockRef { id: id.to_string(), name: id.to_string(), kind: k, role: None }
}

/// Who could review a merge request changing `blocks` (the most changed first): each block's
/// declared owners, then who reviewed it, then who changed it, two a block and five in all. Not
/// its author nor a reviewer yet, and only who could be asked: active, Reporter here or up.
async fn suggest(state: &AppState, p: &Project, mr: &MergeRequest, blocks: &[MrBlock]) -> ApiResult<Vec<pdb::UserBrief>> {
    let asked: HashSet<i64> = pdb::mr_reviewers(&state.db, &[mr.id]).await?.into_iter().map(|(_, r)| r.user.id).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for t in blocks {
        let b = &t.block;
        let names: Vec<String> = b
            .owners
            .iter()
            .filter_map(|o| o.strip_prefix('@').map(str::to_string))
            .chain(b.reviewers.iter().map(|r| r.username.clone()))
            .chain(b.people.iter().filter_map(|x| x.username.clone()))
            .collect();
        let mut here = 0;
        for name in names {
            if here == SUGGESTED_PER_BLOCK || out.len() == MAX_SUGGESTED {
                break;
            }
            if !seen.insert(name.to_lowercase()) {
                continue;
            }
            // a group, someone gone, a bot, or someone who cannot review here
            let Some(u) = forgust_db::users::find_by_username(&state.db, &name).await? else { continue };
            if Some(u.id) == mr.author_id || asked.contains(&u.id) || !u.is_active() || u.user_type != "human" {
                continue;
            }
            if forgust_db::authz::project_level(&state.db, u.id, p.id).await?.unwrap_or(0) < forgust_db::access::REPORTER {
                continue;
            }
            out.push(pdb::UserBrief { id: u.id, username: u.username, name: u.name });
            here += 1;
        }
    }
    Ok(out)
}

/// The block a path is in.
#[derive(Serialize, ToSchema)]
pub struct PathBlock {
    pub block: Option<MapBlock>,
}

#[derive(Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    pub path: String,
}

/// The block of the Smart View a file or folder is in, by the default branch's map: a map per
/// ref browsed would cost more than it tells. None when the Smart View is off, or has no map yet.
#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/map/block", params(("path" = Option<String>, Query, description = "A file or a folder; the top without one")), responses((status = 200, body = PathBlock)), tag = "repository")]
pub async fn path_block(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<PathBlock>> {
    let p = readable(&state, &v, &id).await?;
    let Some(map) = kept_if_on(&state, &p, None).await? else { return Ok(Json(PathBlock { block: None })) };
    let path = q.path.trim_matches('/');
    Ok(Json(PathBlock { block: map.block_of(if path.is_empty() { "." } else { path }).cloned() }))
}

/// What the commits of the default branch changed to the map, as the map jobs read it. Empty
/// until one has (the Smart View off, or no job of the default branch since it came).
#[derive(Serialize, ToSchema)]
pub struct MapHistory {
    /// The last commit read.
    pub head: Option<String>,
    /// Where it starts when it does not go back to the first commit: what came before is not told.
    pub start: Option<HistoryStart>,
    /// The commits that changed the map, oldest first. A revert and what it undid are left out.
    pub chapters: Vec<MapChapter>,
}

#[derive(Serialize, ToSchema)]
pub struct HistoryStart {
    pub commit: String,
    pub at: DateTime<Utc>,
}

/// A commit that changed the map, and what it changed.
#[derive(Serialize, ToSchema)]
pub struct MapChapter {
    /// The commit that made the changes.
    pub commit: String,
    /// The default branch's merge commit that brought them, when it was made on the branch merged.
    pub merge_commit: Option<String>,
    /// When they reached the default branch.
    pub at: DateTime<Utc>,
    /// The first line of the commit's message.
    pub title: String,
    pub author: ChapterAuthor,
    /// The merge request merged as the commit (squashed) or as its merge.
    pub merge_request: Option<ChapterMergeRequest>,
    pub changes: ChapterChanges,
}

#[derive(Serialize, ToSchema)]
pub struct ChapterAuthor {
    pub name: String,
    /// Their account, by their verified email.
    pub username: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ChapterMergeRequest {
    pub iid: i64,
    pub title: String,
    /// The start of its description: why it was made. None without one.
    pub description: Option<String>,
    /// Its comments, not the system's.
    pub comments: i64,
}

/// What a commit changed to the map. A block found again under another id is renamed rather
/// than removed and added; a role is a guess whose changes are not told, a kind's are.
#[derive(Serialize, ToSchema)]
pub struct ChapterChanges {
    pub blocks_added: Vec<ChapterBlock>,
    pub blocks_removed: Vec<ChapterBlock>,
    pub blocks_renamed: Vec<ChapterRename>,
    pub kinds_changed: Vec<ChapterKind>,
    pub links_added: Vec<MapLink>,
    pub links_removed: Vec<MapLink>,
}

/// A block as it was then: enough to draw it.
#[derive(Serialize, ToSchema)]
pub struct ChapterBlock {
    pub id: String,
    pub name: String,
    pub kind: MapKind,
    pub role: Option<MapRole>,
}

#[derive(Serialize, ToSchema)]
pub struct ChapterRename {
    pub from: ChapterBlock,
    pub to: ChapterBlock,
}

#[derive(Serialize, ToSchema)]
pub struct ChapterKind {
    pub id: String,
    pub from: MapKind,
    pub to: MapKind,
}

/// The history of the Smart View's map: what each commit of the default branch changed to it,
/// with the merge request that brought it, and who made it.
#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/map/history", responses((status = 200, body = MapHistory)), tag = "repository")]
pub async fn history(State(state): State<AppState>, Viewer(v): Viewer, Path(id): Path<String>) -> ApiResult<Json<MapHistory>> {
    let p = readable(&state, &v, &id).await?;
    let none = || Json(MapHistory { head: None, start: None, chapters: Vec::new() });
    if !svc::is_on(&state, p.id).await? {
        return Ok(none());
    }
    // the default branch's as it is now: another one's starts again at its next map
    let Some(h) = db::history(&state.db, p.id).await?.filter(|h| p.default_branch.as_deref() == Some(h.git_ref.as_str())) else {
        return Ok(none());
    };
    // a chapter a newer assay wrote, which this server does not read, is left out
    let read: Vec<(db::Chapter, forgust_assay::Changes)> = db::chapters(&state.db, p.id)
        .await?
        .into_iter()
        .filter_map(|c| serde_json::from_value(c.changes.clone()).ok().map(|ch| (c, ch)))
        .collect();
    let mut emails: Vec<String> = read.iter().map(|(c, _)| c.author_email.to_lowercase()).collect();
    emails.sort();
    emails.dedup();
    let accounts: HashMap<String, String> = usernames(&state, &emails).await.into_iter().collect();
    let chapters = fold_reverts(read, |c| &c.title)
        .into_iter()
        .map(|(c, ch)| MapChapter {
            author: ChapterAuthor { username: accounts.get(&c.author_email.to_lowercase()).cloned(), name: c.author_name },
            merge_request: c.mr_iid.zip(c.mr_title).map(|(iid, title)| ChapterMergeRequest {
                iid,
                title,
                description: c.mr_description.as_deref().and_then(excerpt),
                comments: c.mr_comments.unwrap_or(0),
            }),
            commit: c.sha,
            merge_commit: c.merge_sha,
            at: c.at,
            title: c.title,
            changes: chapter_changes(ch),
        })
        .collect();
    Ok(Json(MapHistory {
        head: Some(h.head),
        start: h.start_sha.zip(h.start_at).map(|(commit, at)| HistoryStart { commit, at }),
        chapters,
    }))
}

/// Leaves out each revert (a commit named so) and what it undid: a change that came and went
/// tells nothing of the map. The revert of a revert stays, as what made the change.
fn fold_reverts<T>(chapters: Vec<(T, forgust_assay::Changes)>, title: impl Fn(&T) -> &str) -> Vec<(T, forgust_assay::Changes)> {
    let mut gone = vec![false; chapters.len()];
    for j in 0..chapters.len() {
        if !title(&chapters[j].0).starts_with("Revert \"") {
            continue;
        }
        if let Some(i) = (0..j).rev().find(|&i| !gone[i] && chapters[j].1.undoes(&chapters[i].1)) {
            gone[i] = true;
            gone[j] = true;
        }
    }
    chapters.into_iter().zip(gone).filter(|(_, g)| !g).map(|(c, _)| c).collect()
}

/// The start of a merge request's description, under its title: its first paragraph that is not a
/// heading nor a template's comment, on one line, cut on a word past 240 characters.
fn excerpt(text: &str) -> Option<String> {
    const MAX: usize = 240;
    let first = text.split("\n\n").map(str::trim).find(|p| !p.is_empty() && !p.starts_with('#') && !p.starts_with("<!--"))?;
    let line = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX {
        return Some(line);
    }
    let cut: String = line.chars().take(MAX).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(words, _)| words);
    Some(format!("{}…", cut.trim_end_matches([',', ';', ':', '.'])))
}

fn chapter_block(b: forgust_assay::BlockRef) -> ChapterBlock {
    ChapterBlock { id: b.id, name: b.name, kind: kind(b.kind), role: b.role.map(role) }
}

fn chapter_rename(r: forgust_assay::Renamed) -> ChapterRename {
    ChapterRename { from: chapter_block(r.from), to: chapter_block(r.to) }
}

fn chapter_changes(c: forgust_assay::Changes) -> ChapterChanges {
    let block = chapter_block;
    ChapterChanges {
        blocks_added: c.blocks_added.into_iter().map(block).collect(),
        blocks_removed: c.blocks_removed.into_iter().map(block).collect(),
        blocks_renamed: c.blocks_renamed.into_iter().map(chapter_rename).collect(),
        kinds_changed: c.kinds_changed.into_iter().map(|k| ChapterKind { id: k.id, from: kind(k.from), to: kind(k.to) }).collect(),
        links_added: c.links_added.into_iter().map(map_link).collect(),
        links_removed: c.links_removed.into_iter().map(map_link).collect(),
    }
}

fn map_link(l: forgust_assay::Link) -> MapLink {
    MapLink { from: l.from, to: l.to, kind: link_kind(l.kind), inferred: l.certainty == forgust_assay::Certainty::Inferred, via: l.via }
}

/// A report read from a job, as the Smart View shows it: with the accounts of its people and
/// who reviewed each block's changes.
async fn present(state: &AppState, p: &Project, row: &db::Map, report: svc::report::Report) -> ShownMap {
    // assay wrote them, and read them itself: one that would not read leaves none out
    let excludes = forgust_assay::Patterns::new(&report.exclude)
        .or_else(|_| forgust_assay::Patterns::new::<&str>([]))
        .expect("no pattern is a valid set");
    let mut out = RepoMap {
        commit: row.sha.clone(),
        git_ref: row.git_ref.clone(),
        blocks: Vec::new(),
        links: Vec::new(),
        unassigned: 0,
        unassigned_sample: Vec::new(),
        warnings: Vec::new(),
        config_error: report.error,
        config_file: report.config_file,
        days: report.days,
    };
    let Some(map) = report.map else {
        out.config_error.get_or_insert_with(|| "no map".to_string());
        return ShownMap { map: out, left_out: excludes };
    };
    let accounts = accounts(state, &map).await;
    let left_out = opted_out(state, &report.excluded).await;
    let mut reviewed = reviewers(state, p, &map, &excludes, report.days, &left_out).await;
    let files: HashSet<&str> = report.files.iter().map(String::as_str).collect();
    out.blocks = map
        .blocks
        .into_iter()
        .map(|b| {
            let reviewers = reviewed.remove(&b.id).unwrap_or_default();
            block(b, &accounts, reviewers, &files)
        })
        .collect();
    out.links = map.links.into_iter().map(map_link).collect();
    out.unassigned = map.unassigned.files;
    out.unassigned_sample = map.unassigned.sample;
    out.warnings = map.warnings;
    ShownMap { map: out, left_out: excludes }
}

/// The accounts of the people found, by verified email (lowercased). Best effort.
async fn accounts(state: &AppState, map: &forgust_assay::RepoMap) -> HashMap<String, String> {
    let mut emails: Vec<String> = map.blocks.iter().flat_map(|b| b.people.iter().map(|p| p.email.to_lowercase())).collect();
    emails.sort();
    emails.dedup();
    usernames(state, &emails).await.into_iter().collect()
}

/// The accounts of who opted out (assay.toml's `[people] exclude`, by the addresses the job
/// found them under), left out of the reviewers as assay leaves them out of the people. Best
/// effort.
async fn opted_out(state: &AppState, emails: &[String]) -> HashSet<String> {
    usernames(state, emails).await.into_iter().map(|(_, u)| u.to_lowercase()).collect()
}

/// (verified email, username) of the accounts under `emails`; none when they cannot be read.
async fn usernames(state: &AppState, emails: &[String]) -> Vec<(String, String)> {
    if emails.is_empty() {
        return Vec::new();
    }
    pdb::usernames_by_email(&state.db, emails).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the map's people not linked to their accounts");
        Vec::new()
    })
}

/// A merged merge request's latest version, and who reviewed it: username, name, when.
struct Reviewed {
    base: String,
    head: String,
    by: Vec<(String, String, DateTime<Utc>)>,
}

/// Someone's reviews of a block: how many, and the latest.
struct Tally {
    name: String,
    reviews: usize,
    last: DateTime<Utc>,
}

/// Who reviewed each block's changes, by its id: who took part in the merge requests merged within
/// the days of history (the latest `MAX_REVIEWED`) that changed its files (not those `excludes`
/// leave out of every block), but who opted out
/// (`left_out`). Best effort: a merge request whose changes cannot be read is left out.
async fn reviewers(
    state: &AppState,
    p: &Project,
    map: &forgust_assay::RepoMap,
    excludes: &forgust_assay::Patterns,
    days: u32,
    left_out: &HashSet<String>,
) -> HashMap<String, Vec<MapReviewer>> {
    // whatever assay.toml's days: never a date chrono cannot hold
    let since =
        chrono::Duration::try_days(i64::from(days)).and_then(|d| Utc::now().checked_sub_signed(d)).unwrap_or(DateTime::<Utc>::MIN_UTC);
    let rows = match mrdb::reviewed_since(&state.db, p.id, since, MAX_REVIEWED).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "the map's reviewers not read");
            return HashMap::new();
        }
    };
    // each merge request's latest version, and who reviewed it when
    let mut mrs: BTreeMap<i64, Reviewed> = BTreeMap::new();
    for r in rows.into_iter().filter(|r| !left_out.contains(&r.username.to_lowercase())) {
        mrs.entry(r.mr_id)
            .or_insert_with(|| Reviewed { base: r.base_sha, head: r.head_sha, by: Vec::new() })
            .by
            .push((r.username, r.name, r.at));
    }
    // the paths each changed, a few at a time (each read owns its ends: a stream's closure that
    // borrowed them would not be Send for every lifetime)
    let storage = &state.storage;
    let reads: Vec<_> = mrs
        .iter()
        .map(|(id, mr)| {
            let (id, base, head) = (*id, mr.base.clone(), mr.head.clone());
            async move { (id, storage.changed_paths(p.id, None, &base, &head).await.unwrap_or_default()) }
        })
        .collect();
    let changed: Vec<(i64, Vec<String>)> = stream::iter(reads).buffer_unordered(DIFFS_AT_ONCE).collect().await;
    // block → username → their reviews there
    let mut per: HashMap<String, HashMap<String, Tally>> = HashMap::new();
    for (id, paths) in changed {
        let blocks: HashSet<&str> =
            paths.iter().filter(|path| !excludes.matches(path)).filter_map(|path| map.block_of(path)).map(|b| b.id.as_str()).collect();
        for (username, name, at) in &mrs[&id].by {
            for b in &blocks {
                let e = per.entry(b.to_string()).or_default().entry(username.clone()).or_insert(Tally {
                    name: name.clone(),
                    reviews: 0,
                    last: *at,
                });
                e.reviews += 1;
                e.last = e.last.max(*at);
            }
        }
    }
    per.into_iter()
        .map(|(b, people)| {
            let mut list: Vec<MapReviewer> = people
                .into_iter()
                .map(|(username, t)| MapReviewer {
                    username,
                    name: t.name,
                    reviews: t.reviews,
                    last: t.last.format("%Y-%m-%d").to_string(),
                })
                .collect();
            list.sort_by(|a, b| b.reviews.cmp(&a.reviews).then_with(|| b.last.cmp(&a.last)).then_with(|| a.username.cmp(&b.username)));
            list.truncate(SHOWN_REVIEWERS);
            (b, list)
        })
        .collect()
}

/// `files`: its roots and manifests that are files, as the job saw them.
fn block(b: forgust_assay::Block, accounts: &HashMap<String, String>, reviewers: Vec<MapReviewer>, files: &HashSet<&str>) -> MapBlock {
    let paths = |list: Vec<String>| list.into_iter().map(|path| MapPath { dir: !files.contains(path.as_str()), path }).collect();
    MapBlock {
        id: b.id,
        name: b.name,
        description: b.description,
        kind: kind(b.kind),
        role: b.role.map(role),
        roots: paths(b.roots),
        manifests: paths(b.manifests),
        parent: b.parent,
        image: b.image,
        files: b.files,
        languages: b
            .languages
            .into_iter()
            .map(|l| MapLanguage { color: crate::languages::color_of(&l.name).map(str::to_string), name: l.name, files: l.files })
            .collect(),
        jobs: b.jobs,
        owners: b.owners,
        people: b
            .people
            .into_iter()
            .map(|p| MapPerson { username: accounts.get(&p.email.to_lowercase()).cloned(), name: p.name, commits: p.commits, last: p.last })
            .collect(),
        reviewers,
        bus_factor: b.bus_factor,
        configured: b.source == forgust_assay::Source::Configured,
    }
}

fn kind(k: forgust_assay::Kind) -> MapKind {
    match k {
        forgust_assay::Kind::Rust => MapKind::Rust,
        forgust_assay::Kind::Node => MapKind::Node,
        forgust_assay::Kind::Python => MapKind::Python,
        forgust_assay::Kind::Go => MapKind::Go,
        forgust_assay::Kind::Docker => MapKind::Docker,
        forgust_assay::Kind::Service => MapKind::Service,
        forgust_assay::Kind::Helm => MapKind::Helm,
        forgust_assay::Kind::Kubernetes => MapKind::Kubernetes,
        forgust_assay::Kind::Ansible => MapKind::Ansible,
        forgust_assay::Kind::Terraform => MapKind::Terraform,
        forgust_assay::Kind::Pulumi => MapKind::Pulumi,
        forgust_assay::Kind::Migrations => MapKind::Migrations,
        forgust_assay::Kind::Ci => MapKind::Ci,
        forgust_assay::Kind::Docs => MapKind::Docs,
        forgust_assay::Kind::Other => MapKind::Other,
    }
}

fn role(r: forgust_assay::Role) -> MapRole {
    match r {
        forgust_assay::Role::Web => MapRole::Web,
        forgust_assay::Role::Api => MapRole::Api,
        forgust_assay::Role::Cli => MapRole::Cli,
        forgust_assay::Role::Library => MapRole::Library,
        forgust_assay::Role::Database => MapRole::Database,
        forgust_assay::Role::Cache => MapRole::Cache,
        forgust_assay::Role::Queue => MapRole::Queue,
        forgust_assay::Role::Search => MapRole::Search,
        forgust_assay::Role::Storage => MapRole::Storage,
        forgust_assay::Role::Mail => MapRole::Mail,
    }
}

fn link_kind(k: forgust_assay::LinkKind) -> MapLinkKind {
    match k {
        forgust_assay::LinkKind::Uses => MapLinkKind::Uses,
        forgust_assay::LinkKind::Builds => MapLinkKind::Builds,
        forgust_assay::LinkKind::Runs => MapLinkKind::Runs,
        forgust_assay::LinkKind::DependsOn => MapLinkKind::DependsOn,
        forgust_assay::LinkKind::Connects => MapLinkKind::Connects,
        forgust_assay::LinkKind::Calls => MapLinkKind::Calls,
    }
}

// ------------------------------------------------------------ settings

#[derive(Serialize, ToSchema)]
pub struct SmartViewSettings {
    /// The instance lets projects turn the Smart View on.
    pub available: bool,
    pub enabled: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct SmartViewSettingsBody {
    pub enabled: bool,
}

async fn settings_of(state: &AppState, project_id: i64) -> ApiResult<SmartViewSettings> {
    let c = svc::config(state).await.map_err(ApiError::Internal)?;
    let enabled = db::get(&state.db, project_id).await?.is_some_and(|s| s.enabled);
    Ok(SmartViewSettings { available: c.enabled, enabled })
}

/// The Smart View of the project (Maintainers).
#[utoipa::path(get, path = "/api/v1/projects/{id}/smart_view", responses((status = 200, body = SmartViewSettings)), tag = "smart_view")]
pub async fn get(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
) -> ApiResult<Json<SmartViewSettings>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    Ok(Json(settings_of(&state, p.id).await?))
}

/// Turn the Smart View on or off for the project. On: a job of Forgust's maps the default
/// branch now, then each new commit of it and of open merge requests. Off: its maps go.
#[utoipa::path(put, path = "/api/v1/projects/{id}/smart_view", request_body = SmartViewSettingsBody, responses((status = 200, body = SmartViewSettings)), tag = "smart_view")]
pub async fn set(
    State(state): State<AppState>,
    SignedIn(_, principal): SignedIn,
    Path(id): Path<String>,
    Json(body): Json<SmartViewSettingsBody>,
) -> ApiResult<Json<SmartViewSettings>> {
    let p = project_for(&state, &principal, &id, ProjectAction::Admin).await?;
    svc::configure(&state, &principal, &p, body.enabled).await?;
    Ok(Json(settings_of(&state, p.id).await?))
}

// ------------------------------------------------------------ admin

#[derive(Serialize, Deserialize, ToSchema)]
pub struct AdminSmartView {
    /// Whether projects may turn the Smart View on.
    pub enabled: bool,
    /// The assay image the job runs; empty: the one released with this server.
    pub image: String,
    /// Runner tags the job asks for (instance runners only).
    pub tags: Vec<String>,
    pub timeout_secs: u64,
}

#[derive(Serialize, ToSchema)]
pub struct AdminSmartViewStatus {
    pub settings: AdminSmartView,
    /// The image used when none is set: the config file's, by default this version's.
    pub default_image: String,
    pub enabled_projects: i64,
}

async fn admin_status(state: &AppState) -> ApiResult<AdminSmartViewStatus> {
    let saved = db::instance(&state.db).await?;
    // the image set here, apart from the default it falls back to
    let image = saved.get("image").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let c = svc::config_of(state, saved);
    Ok(AdminSmartViewStatus {
        settings: AdminSmartView { enabled: c.enabled, image, tags: c.tags, timeout_secs: c.timeout_secs },
        default_image: state.cfg.smart_view.image.clone(),
        enabled_projects: db::enabled_count(&state.db).await?,
    })
}

/// The Smart View on this instance: the settings in effect (the config file, then changes made here).
#[utoipa::path(get, path = "/api/v1/admin/smart_view", responses((status = 200, body = AdminSmartViewStatus)), tag = "smart_view")]
pub async fn admin_get(State(state): State<AppState>, Admin(..): Admin) -> ApiResult<Json<AdminSmartViewStatus>> {
    Ok(Json(admin_status(&state).await?))
}

#[utoipa::path(put, path = "/api/v1/admin/smart_view", request_body = AdminSmartView, responses((status = 200, body = AdminSmartViewStatus)), tag = "smart_view")]
pub async fn admin_set(
    State(state): State<AppState>,
    Admin(_, principal): Admin,
    Json(s): Json<AdminSmartView>,
) -> ApiResult<Json<AdminSmartViewStatus>> {
    // the default is not saved: an upgrade moves it
    let image = Some(s.image.trim()).filter(|i| !i.is_empty() && *i != state.cfg.smart_view.image);
    if image.is_some_and(|i| i.chars().any(|c| c.is_whitespace() || c.is_control()) || i.len() > 512) {
        return Err(ApiError::bad(format!("the image is a name like ghcr.io/forgust-dev/forgust-assay:{}", env!("CARGO_PKG_VERSION"))));
    }
    if !(60..=7_200).contains(&s.timeout_secs) {
        return Err(ApiError::bad("the timeout is 1 minute to 2 hours"));
    }
    let tags: Vec<String> = s.tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    if tags.len() > 20 || tags.iter().any(|t| t.len() > 100) {
        return Err(ApiError::bad("at most 20 tags, of at most 100 characters each"));
    }
    let overrides = svc::Overrides { enabled: s.enabled, image: image.map(str::to_string), tags, timeout_secs: s.timeout_secs };
    let saved = serde_json::to_value(&overrides).map_err(|e| ApiError::Internal(e.into()))?;
    db::set_instance(&state.db, &saved).await?;
    crate::services::audit(&state, &principal, "smart_view_instance_updated", None, saved, None).await;
    Ok(Json(admin_status(&state).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: &str, kind: MapKind, manifest: &str) -> MapBlock {
        MapBlock {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            kind,
            role: None,
            roots: Vec::new(),
            manifests: vec![MapPath { path: manifest.to_string(), dir: false }],
            parent: None,
            image: None,
            files: 1,
            languages: Vec::new(),
            jobs: Vec::new(),
            owners: Vec::new(),
            people: Vec::new(),
            reviewers: Vec::new(),
            bus_factor: None,
            configured: false,
        }
    }

    fn link(from: &str, to: &str, via: &str) -> MapLink {
        MapLink { from: from.to_string(), to: to.to_string(), kind: MapLinkKind::Uses, inferred: false, via: via.to_string() }
    }

    fn map(blocks: Vec<MapBlock>, links: Vec<MapLink>) -> RepoMap {
        RepoMap {
            commit: String::new(),
            git_ref: String::new(),
            blocks,
            links,
            unassigned: 0,
            unassigned_sample: Vec::new(),
            warnings: Vec::new(),
            config_error: None,
            config_file: true,
            days: 365,
        }
    }

    /// A description's start: its first paragraph of words, on a line, cut on a word.
    #[test]
    fn a_description_starts_with_its_first_paragraph() {
        assert_eq!(excerpt(""), None);
        assert_eq!(excerpt("## Why\n\nReads the stock\nfrom the table.\n\nMore."), Some("Reads the stock from the table.".into()));
        assert_eq!(excerpt("<!-- the template -->\n\nA fix."), Some("A fix.".into()));
        let long = excerpt(&"word ".repeat(80)).unwrap();
        assert!(long.ends_with("word…") && long.chars().count() <= 241, "{long}");
    }

    /// A revert and what it undid tell nothing of the map; the revert of a revert is what made the
    /// change, and what undoes a change without being a revert is a change of its own.
    #[test]
    fn a_revert_and_what_it_undid_are_left_out() {
        let web = |added: bool| {
            let b = vec![forgust_assay::BlockRef { id: "web".into(), name: "web".into(), kind: forgust_assay::Kind::Node, role: None }];
            if added {
                forgust_assay::Changes { blocks_added: b, ..Default::default() }
            } else {
                forgust_assay::Changes { blocks_removed: b, ..Default::default() }
            }
        };
        let chapters = vec![
            ("a web app", web(true)),
            ("Revert \"a web app\"", web(false)),
            ("Revert \"Revert \"a web app\"\"", web(true)),
            ("drop the web app", web(false)),
        ];
        let kept: Vec<&str> = fold_reverts(chapters, |t: &&str| *t).into_iter().map(|(t, _)| t).collect();
        assert_eq!(kept, ["Revert \"Revert \"a web app\"\"", "drop the web app"]);
    }

    #[test]
    fn what_a_merge_request_changes_to_the_map() {
        let base = map(
            vec![
                block("web", MapKind::Node, "web/package.json"),
                block("old", MapKind::Rust, "old/Cargo.toml"),
                block("main-only", MapKind::Go, "x/go.mod"),
            ],
            vec![link("web", "old", "web/package.json")],
        );
        // the default branch moved on (`main-only`, `new-on-main`): none of that is the merge request's
        let mut lib = block("billing", MapKind::Rust, "billing/Cargo.toml");
        lib.role = Some(MapRole::Library);
        let head = map(
            vec![block("web", MapKind::Node, "web/package.json"), lib, block("db", MapKind::Node, "db/package.json")],
            vec![link("web", "billing", "web/package.json"), link("web", "db", "web/package.json"), link("x", "y", "x/go.mod")],
        );
        let changed = ["web/package.json", "billing/Cargo.toml", "billing/src/lib.rs", "old/Cargo.toml"].map(str::to_string);
        let c = map_changes(&base, &head, &changed);
        let ids = |bs: &[MapBlock]| bs.iter().map(|b| b.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&c.blocks_added), ["billing"], "db's manifest is not changed by it");
        assert_eq!(ids(&c.blocks_removed), ["old"], "main-only went on the default branch");
        let links = |ls: &[MapLink]| ls.iter().map(|l| format!("{}>{}", l.from, l.to)).collect::<Vec<_>>();
        assert_eq!(links(&c.links_added), ["web>billing", "web>db"]);
        assert_eq!(links(&c.links_removed), ["web>old"]);
        assert!(c.blocks_changed.is_empty());
    }
}
