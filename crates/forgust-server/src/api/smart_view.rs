//! The Smart View: a project's code as blocks (what is built, run or deployed apart: a
//! package, an image, a service, a database's schema, the CI, the docs), the links between
//! them and the people to ask. assay finds them (github.com/forgust-dev/assay), from the
//! manifests and the history read here through the storage: a bare repository has no
//! working copy for it to read.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::response::Response;
use forgust_db::people as pdb;
use forgust_db::projects::Project;
use serde::Serialize;
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
const SYMLINK: u32 = 0o120000;

/// The map of a commit: its blocks, the links between them, and who knows each.
#[derive(Serialize, ToSchema)]
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
    /// So many files or commits that only the first files, or the latest commits, were read.
    pub truncated: bool,
    /// The days of history that tell who knows a block.
    pub days: u32,
}

/// A part of the repository built, run or deployed apart.
#[derive(Serialize, ToSchema)]
pub struct MapBlock {
    /// Stable while its name is: what assay.toml names it by.
    pub id: String,
    pub name: String,
    pub kind: MapKind,
    /// A guess from its dependencies or its image, which assay.toml corrects.
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
    /// How many people made at least a fifth of its recent commits: 1, only one knows it.
    pub bus_factor: Option<usize>,
    /// Declared by assay.toml rather than found.
    pub configured: bool,
}

#[derive(Serialize, ToSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum MapKind {
    Rust,
    Node,
    Python,
    Go,
    Docker,
    Service,
    Migrations,
    Ci,
    Docs,
    Other,
}

#[derive(Serialize, ToSchema, Clone, Copy)]
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
#[derive(Serialize, ToSchema)]
pub struct MapPath {
    pub path: String,
    /// A folder, not a file.
    pub dir: bool,
}

#[derive(Serialize, ToSchema)]
pub struct MapLanguage {
    pub name: String,
    pub files: usize,
    /// `#rrggbb`, as the repository's language bar shows it; none for prose and data (JSON…).
    pub color: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct MapPerson {
    pub name: String,
    /// Their account, by their verified email.
    pub username: Option<String>,
    /// Their commits there, within the days of history read.
    pub commits: usize,
    /// The day of their latest one (`YYYY-MM-DD`).
    pub last: String,
}

/// `from` needs `to`.
#[derive(Serialize, ToSchema)]
pub struct MapLink {
    pub from: String,
    pub to: String,
    pub kind: MapLinkKind,
    /// A guess (a service's environment names the other as a host), not declared.
    pub inferred: bool,
    /// The file that says so.
    pub via: String,
}

#[derive(Serialize, ToSchema, Clone, Copy)]
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
    let key = ResponseCache::key(p.id, "map", &[&commit]);
    cached(&state, key, || build(&state, &p, commit.clone())).await
}

async fn build(state: &AppState, p: &Project, commit: String) -> ApiResult<RepoMap> {
    let (blobs, files_cut) = state.storage.tree_blobs(p.id, &commit, MAX_FILES).await?;
    // the text of what assay reads; a link's target is no file's text
    let wanted: Vec<&forgust_storage::TreeBlob> =
        blobs.iter().filter(|b| b.mode != SYMLINK && b.size <= MAX_TEXT && assay::reads(&b.path)).take(MAX_TEXTS).collect();
    let oids: Vec<String> = wanted.iter().map(|b| b.oid.clone()).collect();
    let contents = state.storage.blobs_by_id(p.id, &oids, MAX_TEXT as usize).await?;
    let texts: HashMap<String, String> =
        wanted.iter().zip(contents).filter_map(|(b, c)| Some((b.path.clone(), String::from_utf8(c?.1).ok()?))).collect();
    let days = assay::history_days(&texts);
    let since = chrono::Utc::now().timestamp() - i64::from(days) * 86_400;
    let (log, log_cut) = state.storage.log_paths(p.id, &commit, since, MAX_COMMITS).await?;
    let snapshot = assay::Snapshot {
        name: p.path.clone(),
        commit: Some(commit.clone()),
        files: blobs.into_iter().map(|b| b.path).collect(),
        texts,
        history: Some(log.into_iter().map(|c| assay::Commit { name: c.name, email: c.email, time: c.time, files: c.paths }).collect()),
    };
    let (mapped, snapshot) =
        tokio::task::spawn_blocking(move || (assay::map_snapshot(&snapshot), snapshot)).await.map_err(|e| ApiError::Internal(e.into()))?;
    let mut out = RepoMap {
        commit,
        blocks: Vec::new(),
        links: Vec::new(),
        unassigned: 0,
        unassigned_sample: Vec::new(),
        warnings: Vec::new(),
        config_error: None,
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
    let files: HashSet<&str> = snapshot.files.iter().map(String::as_str).collect();
    out.blocks = map.blocks.into_iter().map(|b| block(b, &accounts, &files)).collect();
    out.links = map
        .links
        .into_iter()
        .map(|l| MapLink {
            from: l.from,
            to: l.to,
            kind: link_kind(l.kind),
            inferred: l.certainty == assay::Certainty::Inferred,
            via: l.via,
        })
        .collect();
    out.unassigned = map.unassigned.files;
    out.unassigned_sample = map.unassigned.sample;
    out.warnings = map.warnings;
    Ok(out)
}

/// The accounts of the people found, by verified email (lowercased). Best effort.
async fn accounts(state: &AppState, map: &assay::RepoMap) -> HashMap<String, String> {
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

fn block(b: assay::Block, accounts: &HashMap<String, String>, files: &HashSet<&str>) -> MapBlock {
    let paths = |list: Vec<String>| list.into_iter().map(|path| MapPath { dir: !files.contains(path.as_str()), path }).collect();
    MapBlock {
        id: b.id,
        name: b.name,
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
        bus_factor: b.bus_factor,
        configured: b.source == assay::Source::Configured,
    }
}

fn kind(k: assay::Kind) -> MapKind {
    match k {
        assay::Kind::Rust => MapKind::Rust,
        assay::Kind::Node => MapKind::Node,
        assay::Kind::Python => MapKind::Python,
        assay::Kind::Go => MapKind::Go,
        assay::Kind::Docker => MapKind::Docker,
        assay::Kind::Service => MapKind::Service,
        assay::Kind::Migrations => MapKind::Migrations,
        assay::Kind::Ci => MapKind::Ci,
        assay::Kind::Docs => MapKind::Docs,
        assay::Kind::Other => MapKind::Other,
    }
}

fn role(r: assay::Role) -> MapRole {
    match r {
        assay::Role::Web => MapRole::Web,
        assay::Role::Api => MapRole::Api,
        assay::Role::Cli => MapRole::Cli,
        assay::Role::Library => MapRole::Library,
        assay::Role::Database => MapRole::Database,
        assay::Role::Cache => MapRole::Cache,
        assay::Role::Queue => MapRole::Queue,
        assay::Role::Search => MapRole::Search,
        assay::Role::Storage => MapRole::Storage,
        assay::Role::Mail => MapRole::Mail,
    }
}

fn link_kind(k: assay::LinkKind) -> MapLinkKind {
    match k {
        assay::LinkKind::Uses => MapLinkKind::Uses,
        assay::LinkKind::Builds => MapLinkKind::Builds,
        assay::LinkKind::Runs => MapLinkKind::Runs,
        assay::LinkKind::DependsOn => MapLinkKind::DependsOn,
        assay::LinkKind::Connects => MapLinkKind::Connects,
        assay::LinkKind::Calls => MapLinkKind::Calls,
    }
}
