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

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy)]
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

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy)]
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

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy)]
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
    let none = || Json(MrBlocks { commit: String::new(), blocks: Vec::new(), unassigned: 0, suggested_reviewers: Vec::new() });
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
    Ok(Json(MrBlocks { commit: map.map.commit.clone(), blocks, unassigned, suggested_reviewers }))
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
    out.links = map
        .links
        .into_iter()
        .map(|l| MapLink {
            from: l.from,
            to: l.to,
            kind: link_kind(l.kind),
            inferred: l.certainty == forgust_assay::Certainty::Inferred,
            via: l.via,
        })
        .collect();
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
