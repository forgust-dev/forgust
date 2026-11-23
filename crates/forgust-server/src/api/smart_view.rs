//! The Smart View: a project's code as blocks (what is built, run or deployed apart: a
//! package, an image, a service, a database's schema, the CI, the docs), the links between
//! them and the people to ask. assay finds them (the forgust-assay crate), from the
//! manifests and the history read here through the storage: a bare repository has no
//! working copy for it to read.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use forgust_db::merge_requests::{self as mrdb, MergeRequest};
use forgust_db::people as pdb;
use forgust_db::projects::Project;
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::repo::{RefPath, cached, commit_of, readable};
use crate::auth::Viewer;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, ResponseCache};

/// The files listed, at most: a larger repository is mapped from the first ones.
const MAX_FILES: usize = 100_000;
/// The files whose text is read (manifests, the CI, CODEOWNERS, assay.toml), at most, and
/// the bytes of each: a larger one is reported as not read.
const MAX_TEXTS: usize = 2_000;
const MAX_TEXT: u64 = 1 << 20;
/// The commits that tell who knows a block, at most: the latest.
const MAX_COMMITS: usize = 5_000;
/// The merged merge requests whose reviewers count, at most: the latest; their changes are read
/// a few at a time.
const MAX_REVIEWED: i64 = 300;
const DIFFS_AT_ONCE: usize = 8;
/// The reviewers shown per block.
const SHOWN_REVIEWERS: usize = 5;
/// Who a merge request is suggested to: at most two per block, and five in all.
const SUGGESTED_PER_BLOCK: usize = 2;
const MAX_SUGGESTED: usize = 5;
const SYMLINK: u32 = 0o120000;

/// The map of a commit: its blocks, the links between them, and who knows each.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct RepoMap {
    /// The commit mapped.
    pub commit: String,
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
    /// So many files or commits that only the first files, or the latest commits, were read.
    pub truncated: bool,
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

/// The Smart View of a ref (without one, the default branch), kept per commit.
#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/map", responses((status = 200, body = RepoMap)), tag = "repository")]
pub async fn map(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Response> {
    let p = readable(&state, &v, &id).await?;
    let (_, commit) = commit_of(&state, &p, q.git_ref.as_deref()).await?;
    cached(&state, map_key(&p, &commit), || build(&state, &p, commit.clone())).await
}

fn map_key(p: &Project, commit: &str) -> String {
    ResponseCache::key(p.id, "map", &[commit])
}

/// The map of a commit, kept as the Smart View's page has it: what the other answers read.
async fn map_at(state: &AppState, p: &Project, commit: &str) -> ApiResult<RepoMap> {
    let key = map_key(p, commit);
    if let Some(b) = state.cache.get(&key).await
        && let Ok(m) = serde_json::from_slice(&b)
    {
        return Ok(m);
    }
    let m = build(state, p, commit.to_string()).await?;
    let b = Bytes::from(serde_json::to_vec(&m).map_err(|e| ApiError::Internal(e.into()))?);
    state.cache.put(key, b).await;
    Ok(m)
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
    /// What was mapped: the head of its latest version (empty before it has one).
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

/// The blocks of the Smart View a merge request changes, mapped at its latest version's head.
#[utoipa::path(get, path = "/api/v1/projects/{id}/merge_requests/{iid}/blocks", responses((status = 200, body = MrBlocks)), tag = "merge_requests")]
pub async fn mr_blocks(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path((id, iid)): Path<(String, i64)>,
) -> ApiResult<Json<MrBlocks>> {
    let p = readable(&state, &v, &id).await?;
    let mr = mrdb::find(&state.db, p.id, iid).await?.ok_or(ApiError::NotFound)?;
    let Some(diff) = mrdb::latest_diff(&state.db, mr.id).await? else {
        return Ok(Json(MrBlocks { commit: String::new(), blocks: Vec::new(), unassigned: 0, suggested_reviewers: Vec::new() }));
    };
    let map = map_at(&state, &p, &diff.head_sha).await?;
    let changed = state.storage.changed_paths(p.id, None, &diff.base_sha, &diff.head_sha).await?;
    let mut files: HashMap<&str, usize> = HashMap::new();
    let mut unassigned = 0;
    for path in &changed {
        match block_of(&map, path) {
            Some(b) => *files.entry(b.id.as_str()).or_default() += 1,
            None => unassigned += 1,
        }
    }
    let mut blocks: Vec<MrBlock> =
        map.blocks.iter().filter_map(|b| files.get(b.id.as_str()).map(|n| MrBlock { block: b.clone(), files: *n })).collect();
    blocks.sort_by_key(|t| std::cmp::Reverse(t.files));
    let suggested_reviewers = suggest(&state, &p, &mr, &blocks).await?;
    Ok(Json(MrBlocks { commit: diff.head_sha, blocks, unassigned, suggested_reviewers }))
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
    /// The ref whose map it is: the branch or tag asked, else the default branch.
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub block: Option<MapBlock>,
}

/// The block of the Smart View a file or folder is in. A commit of the history is answered from
/// the default branch's map: a map per commit browsed would cost more than it tells.
#[utoipa::path(get, path = "/api/v1/projects/{id}/repository/map/block", responses((status = 200, body = PathBlock)), tag = "repository")]
pub async fn path_block(
    State(state): State<AppState>,
    Viewer(v): Viewer,
    Path(id): Path<String>,
    Query(q): Query<RefPath>,
) -> ApiResult<Json<PathBlock>> {
    let p = readable(&state, &v, &id).await?;
    let asked = q.git_ref.filter(|r| !(r.len() >= 7 && r.len() <= 40 && r.bytes().all(|b| b.is_ascii_hexdigit())));
    let (git_ref, commit) = commit_of(&state, &p, asked.as_deref()).await?;
    let map = map_at(&state, &p, &commit).await?;
    let path = q.path.trim_matches('/');
    Ok(Json(PathBlock { git_ref, block: block_of(&map, if path.is_empty() { "." } else { path }).cloned() }))
}

async fn build(state: &AppState, p: &Project, commit: String) -> ApiResult<RepoMap> {
    let (blobs, files_cut) = state.storage.tree_blobs(p.id, &commit, MAX_FILES).await?;
    // the text of what assay reads; a link's target is no file's text
    let wanted: Vec<&forgust_storage::TreeBlob> =
        blobs.iter().filter(|b| b.mode != SYMLINK && b.size <= MAX_TEXT && forgust_assay::reads(&b.path)).take(MAX_TEXTS).collect();
    let oids: Vec<String> = wanted.iter().map(|b| b.oid.clone()).collect();
    let contents = state.storage.blobs_by_id(p.id, &oids, MAX_TEXT as usize).await?;
    let texts: HashMap<String, String> =
        wanted.iter().zip(contents).filter_map(|(b, c)| Some((b.path.clone(), String::from_utf8(c?.1).ok()?))).collect();
    let days = forgust_assay::history_days(&texts);
    let since = chrono::Utc::now().timestamp() - i64::from(days) * 86_400;
    let (log, log_cut) = state.storage.log_paths(p.id, &commit, since, MAX_COMMITS).await?;
    let config_file = blobs.iter().any(|b| b.path == forgust_assay::CONFIG_FILE);
    let snapshot = forgust_assay::Snapshot {
        name: p.path.clone(),
        commit: Some(commit.clone()),
        files: blobs.into_iter().map(|b| b.path).collect(),
        texts,
        history: Some(
            log.into_iter().map(|c| forgust_assay::Commit { name: c.name, email: c.email, time: c.time, files: c.paths }).collect(),
        ),
    };
    let (mapped, snapshot) = tokio::task::spawn_blocking(move || (forgust_assay::map_snapshot(&snapshot), snapshot))
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let mut out = RepoMap {
        commit,
        blocks: Vec::new(),
        links: Vec::new(),
        unassigned: 0,
        unassigned_sample: Vec::new(),
        warnings: Vec::new(),
        config_error: None,
        config_file,
        truncated: files_cut || log_cut,
        days,
    };
    let map = match mapped {
        Ok(m) => m,
        Err(e) => {
            out.config_error = Some(e.to_string());
            return Ok(out);
        }
    };
    let accounts = accounts(state, &map).await;
    let left_out = opted_out(state, &snapshot).await;
    let mut reviewed = reviewers(state, p, &map, days, &left_out).await;
    let files: HashSet<&str> = snapshot.files.iter().map(String::as_str).collect();
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
    Ok(out)
}

/// The accounts of the people found, by verified email (lowercased). Best effort.
async fn accounts(state: &AppState, map: &forgust_assay::RepoMap) -> HashMap<String, String> {
    let mut emails: Vec<String> = map.blocks.iter().flat_map(|b| b.people.iter().map(|p| p.email.to_lowercase())).collect();
    emails.sort();
    emails.dedup();
    if emails.is_empty() {
        return HashMap::new();
    }
    match pdb::usernames_by_email(&state.db, &emails).await {
        Ok(rows) => rows.into_iter().collect(),
        Err(e) => {
            tracing::warn!(error = %e, "the map's people not linked to their accounts");
            HashMap::new()
        }
    }
}

/// The accounts of who opted out (assay.toml's `[people] exclude`), left out of the reviewers as
/// assay leaves them out of the people: by an address of theirs, or by a name or an address their
/// commits bear. Best effort.
async fn opted_out(state: &AppState, s: &forgust_assay::Snapshot) -> HashSet<String> {
    let Some(config) = s.texts.get(forgust_assay::CONFIG_FILE).and_then(|t| forgust_assay::Config::parse(t).ok()) else {
        return HashSet::new();
    };
    let exclude = &config.people.exclude;
    if exclude.is_empty() {
        return HashSet::new();
    }
    let mut emails: HashSet<String> = exclude.iter().filter(|x| x.contains('@')).map(|x| x.to_lowercase()).collect();
    for c in s.history.iter().flatten() {
        if exclude.iter().any(|x| x.eq_ignore_ascii_case(&c.name) || x.eq_ignore_ascii_case(&c.email)) {
            emails.insert(c.email.to_lowercase());
        }
    }
    match pdb::usernames_by_email(&state.db, &emails.into_iter().collect::<Vec<_>>()).await {
        Ok(rows) => rows.into_iter().map(|(_, u)| u.to_lowercase()).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "who opted out of the map's reviewers not read");
            HashSet::new()
        }
    }
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
/// the days of history (the latest `MAX_REVIEWED`) that changed its files, but who opted out
/// (`left_out`). Best effort: a merge request whose changes cannot be read is left out.
async fn reviewers(
    state: &AppState,
    p: &Project,
    map: &forgust_assay::RepoMap,
    days: u32,
    left_out: &HashSet<String>,
) -> HashMap<String, Vec<MapReviewer>> {
    let since = Utc::now() - chrono::Duration::days(i64::from(days));
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
        let blocks: HashSet<&str> = paths.iter().filter_map(|path| map.block_of(path)).map(|b| b.id.as_str()).collect();
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
