//! assay maps a repository into blocks: what can be built, run or deployed apart (a package,
//! an image, a service, a database's schema, a chart, infrastructure code, the CI, the docs),
//! the links between them, and the people who know them. It reads manifests, so it runs on any repository, on any forge;
//! forgust's Smart View shows what it finds. See docs/smart-view.md.
//!
//! ```no_run
//! let map = forgust_assay::map(std::path::Path::new("."), &forgust_assay::Options::default())?;
//! for b in &map.blocks {
//!     println!("{} ({:?}): {} files", b.id, b.kind, b.files);
//! }
//! println!("{:?}", map.block_of("src/main.rs").map(|b| &b.id));
//! # anyhow::Ok(())
//! ```
//!
//! [`map`] reads a folder; without a working copy, gather a [`Snapshot`] (the paths, the text
//! of those [`reads`] picks out, the history) and call [`map_snapshot`]. Forgust runs
//! `assay report` in a CI job and reads the [`report`] it writes.

mod codeowners;
mod config;
mod detect;
mod files;
mod history;
mod lang;
pub mod model;
mod people;
pub mod report;
mod view;
mod yaml;

use std::collections::{HashMap, HashSet};
use std::path::Path;

pub use config::{BlockConfig, Config, FILE as CONFIG_FILE, LinkConfig, PeopleConfig};
use detect::{Job, To, literal_prefix};
pub use files::Patterns;
pub use history::{BlockRef, Changes, Chapter, History, HistoryOptions, KindChanged, MAX_MAPS, Renamed, Start, changes, history};
pub use model::*;
use model::{depth, holds};
pub use people::Commit;
pub use view::page;

/// How many days of history tell who knows a block, unless assay.toml says.
pub const DAYS: u32 = 365;

#[derive(Debug, Clone)]
pub struct Options {
    /// Read the git history for the people who know each block.
    pub people: bool,
    /// How many days of it; assay.toml's, else [`DAYS`].
    pub days: Option<u32>,
    /// The repository's name, which the block of its top folder takes; the folder's without one
    /// (a CI job's clone is in a folder of the runner's naming).
    pub name: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self { people: true, days: None, name: None }
    }
}

/// A repository as assay reads it, gathered beforehand: what [`map_snapshot`] maps, with no
/// working copy at hand.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The repository's name: the block of its top folder takes it.
    pub name: String,
    /// The commit it is at; none out of git.
    pub commit: Option<String>,
    /// Every file, by its path from the top, `/`-separated.
    pub files: Vec<String>,
    /// The text of the files [`reads`] picks out. One missing is reported as not read.
    pub texts: HashMap<String, String>,
    /// The commits of the last [`history_days`] days, merges left out; `None` leaves the
    /// people out.
    pub history: Option<Vec<Commit>>,
}

/// Whether assay reads this file's text: a manifest, a Dockerfile, a compose file, a CI
/// configuration, CODEOWNERS, assay.toml. Those under the folders left out of every map
/// (`node_modules`, `vendor`, fixtures…) are not.
pub fn reads(path: &str) -> bool {
    (path == CONFIG_FILE || codeowners::LOCATIONS.contains(&path) || detect::reads(path)) && !files::excluded_by_default(path)
}

/// How many days of history a [`Snapshot`] needs: assay.toml's `[people] days` (`texts`: the
/// snapshot's), else [`DAYS`].
pub fn history_days(texts: &HashMap<String, String>) -> u32 {
    texts.get(CONFIG_FILE).and_then(|t| Config::parse(t).ok()).and_then(|c| c.people.days).unwrap_or(DAYS)
}

/// The map of the repository at `root`: a folder, in git or not.
pub fn map(root: &Path, opts: &Options) -> anyhow::Result<RepoMap> {
    map_snapshot(&snapshot(root, opts)?.0)
}

/// The folder at `root` as assay reads it, and how many days of its history were asked for.
fn snapshot(root: &Path, opts: &Options) -> anyhow::Result<(Snapshot, u32)> {
    let root = root.canonicalize()?;
    let (files, in_git) = files::list(&root)?;
    let mut texts = HashMap::new();
    for f in files.iter().filter(|f| reads(f)) {
        // one that cannot be read is reported as not read
        if let Ok(t) = std::fs::read_to_string(root.join(f)) {
            texts.insert(f.clone(), t);
        }
    }
    let days = opts.days.unwrap_or_else(|| history_days(&texts));
    let history = if in_git && opts.people { people::history(&root, days) } else { None };
    let commit = if in_git { people::head(&root) } else { None };
    let name = opts.name.clone().unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    Ok((Snapshot { name, commit, files, texts, history }, days))
}

/// The map of a repository gathered beforehand. Fails only on an assay.toml that cannot be
/// read: what it says would be left out without a word.
pub fn map_snapshot(s: &Snapshot) -> anyhow::Result<RepoMap> {
    let config = match s.texts.get(CONFIG_FILE) {
        Some(text) => Config::parse(text)?,
        None if s.files.iter().any(|f| f == CONFIG_FILE) => anyhow::bail!("{CONFIG_FILE}: could not be read"),
        None => Config::default(),
    };
    let excludes = files::Patterns::new(files::DEFAULT_EXCLUDES.iter().map(|p| p.to_string()).chain(config.exclude.iter().cloned()))?;
    let listed: Vec<String> = s.files.iter().filter(|f| !excludes.matches(f)).cloned().collect();

    let mut cx = detect::detect(&s.name, &listed, &s.texts);
    let mut warnings = std::mem::take(&mut cx.warnings);
    let jobs = std::mem::take(&mut cx.jobs);
    let pending = std::mem::take(&mut cx.pending);
    let mut drafts = std::mem::take(&mut cx.drafts);

    // ids, settled in a stable order: by kind, then where each was found
    drafts.sort_by(|a, b| (order(a.kind), &a.key).cmp(&(order(b.kind), &b.key)));
    let mut taken = HashSet::new();
    let mut id_of: HashMap<String, String> = HashMap::new();
    let mut npm: HashMap<String, String> = HashMap::new();
    // a Cargo package's id → its name and its binaries'
    let mut cargo_names: HashMap<String, Vec<String>> = HashMap::new();
    // a name an image may be pulled by → the images'
    let mut images: HashMap<String, Vec<String>> = HashMap::new();
    let mut blocks = Vec::new();
    for d in drafts {
        let id = unique(&d.want_id, d.kind, &mut taken);
        id_of.insert(d.key, id.clone());
        for a in d.aliases.iter().map(|a| detect::slug(a)) {
            let ids = images.entry(a).or_default();
            if !ids.contains(&id) {
                ids.push(id.clone());
            }
        }
        match (d.kind, d.package) {
            (Kind::Node, Some(p)) => {
                npm.insert(p, id.clone());
            }
            (Kind::Rust, Some(p)) => {
                cargo_names.insert(id.clone(), std::iter::once(p).chain(d.bins).collect());
            }
            _ => {}
        }
        blocks.push(Block {
            id,
            name: d.name,
            description: d.description,
            kind: d.kind,
            role: d.role,
            roots: d.roots,
            manifests: d.manifests,
            parent: None,
            image: d.image,
            files: 0,
            languages: Vec::new(),
            jobs: Vec::new(),
            owners: Vec::new(),
            people: Vec::new(),
            bus_factor: None,
            source: Source::Detected,
        });
    }

    // assay.toml's corrections first: the links found then reach the blocks as it has them (one
    // it declares, the roots it gives, the block another was merged into)
    let gone = config.apply_blocks(&mut blocks, &mut warnings);
    // a block merged into another lends it its Cargo names, for a build naming them
    for (from, into) in gone.merged() {
        if let Some(names) = cargo_names.get(from).cloned() {
            cargo_names.entry(into).or_default().extend(names);
        }
    }
    let current = |key: &str| id_of.get(key).and_then(|id| gone.follow(id));

    // the links found, now that every block is known
    let mut links = Vec::new();
    for p in pending {
        let Some(from) = current(&p.from) else { continue };
        let targets: Vec<String> = match &p.to {
            To::Key(k) => current(k).into_iter().collect(),
            To::Npm(name) => npm.get(name).and_then(|id| gone.follow(id)).into_iter().collect(),
            To::Within(path) => within(&blocks, path, false),
            To::Built { path, names } => within(&blocks, path, false)
                .into_iter()
                .filter(|id| cargo_names.get(id).is_none_or(|own| own.iter().any(|n| names.contains(n))))
                .collect(),
            To::Image { image, context } => match id_of.get(image) {
                Some(id) => gone.follow(id).into_iter().collect(),
                None => within(&blocks, context, false),
            },
            To::ImageNamed(reference) => pulled(&images, reference, &s.name).iter().filter_map(|id| gone.follow(id)).collect(),
        };
        for to in targets.into_iter().filter(|t| *t != from) {
            push_link(&mut links, Link { from: from.clone(), to, kind: p.kind, certainty: p.certainty, via: p.via.clone() });
        }
    }

    config.apply_links(&blocks, &mut links, &mut warnings);
    parents(&mut blocks);

    // each file to the block with the deepest root holding it
    let index = Index::new(&blocks);
    let mut per_block: Vec<Vec<&str>> = vec![Vec::new(); blocks.len()];
    let mut unassigned = Unassigned::default();
    for f in &listed {
        match index.of(f) {
            Some(i) => per_block[i].push(f),
            None => {
                unassigned.files += 1;
                if unassigned.sample.len() < 10 {
                    unassigned.sample.push(f.clone());
                }
            }
        }
    }
    for (b, fs) in blocks.iter_mut().zip(&per_block) {
        b.files = fs.len();
        b.languages = languages(fs);
    }

    assign_jobs(&mut blocks, &jobs, &config);
    owners(s, &mut blocks, &per_block, &config, &mut warnings);
    if let Some(commits) = &s.history {
        people::attach(&mut blocks, commits, |p| index.of(p), &config.people.exclude);
    }

    links.sort_by(|a, b| (&a.from, &a.to, a.kind as u8).cmp(&(&b.from, &b.to, b.kind as u8)));
    Ok(RepoMap { schema: SCHEMA, commit: s.commit.clone(), blocks, links, unassigned, warnings })
}

/// Adds a link, unless the same one (ends and kind) is there; a declared one replaces a guess.
pub(crate) fn push_link(links: &mut Vec<Link>, l: Link) {
    match links.iter_mut().find(|x| x.from == l.from && x.to == l.to && x.kind == l.kind) {
        Some(have) if have.certainty == Certainty::Inferred && l.certainty == Certainty::Declared => *have = l,
        Some(_) => {}
        None => links.push(l),
    }
}

/// The images a reference may pull (`registry.example.com/shop/api:1.2`): those named `api`;
/// else, for `shop-api` in the repository `shop`, those named `api`.
fn pulled<'a>(images: &'a HashMap<String, Vec<String>>, reference: &str, repository: &str) -> &'a [String] {
    let Some(name) = detect::image_name(reference) else { return &[] };
    let short = name.strip_prefix(&format!("{}-", detect::slug(repository))).map(str::to_string);
    images.get(&name).or_else(|| short.and_then(|s| images.get(&s))).map(Vec::as_slice).unwrap_or_default()
}

/// Packages first, then what runs them, what deploys them, then the rest.
fn order(k: Kind) -> u8 {
    match k {
        Kind::Rust => 0,
        Kind::Node => 1,
        Kind::Python => 2,
        Kind::Go => 3,
        Kind::Migrations => 4,
        Kind::Docker => 5,
        Kind::Service => 6,
        Kind::Helm => 7,
        Kind::Kubernetes => 8,
        Kind::Ansible => 9,
        Kind::Terraform => 10,
        Kind::Pulumi => 11,
        Kind::Ci => 12,
        Kind::Docs => 13,
        Kind::Other => 14,
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Rust => "rust",
        Kind::Node => "node",
        Kind::Python => "python",
        Kind::Go => "go",
        Kind::Docker => "docker",
        Kind::Service => "service",
        Kind::Helm => "helm",
        Kind::Kubernetes => "kubernetes",
        Kind::Ansible => "ansible",
        Kind::Terraform => "terraform",
        Kind::Pulumi => "pulumi",
        Kind::Migrations => "migrations",
        Kind::Ci => "ci",
        Kind::Docs => "docs",
        Kind::Other => "other",
    }
}

/// `want`, else with its kind (`web-node` beside the `web` crate), else numbered.
fn unique(want: &str, kind: Kind, taken: &mut HashSet<String>) -> String {
    let mut id = want.to_string();
    if taken.contains(&id) {
        id = format!("{want}-{}", kind_name(kind));
    }
    let base = id.clone();
    let mut n = 2;
    while taken.contains(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    taken.insert(id.clone());
    id
}

fn is_package(k: Kind) -> bool {
    matches!(k, Kind::Rust | Kind::Node | Kind::Python | Kind::Go)
}

/// What deploys: a chart, manifests, playbooks, infrastructure code.
fn deploys(k: Kind) -> bool {
    matches!(k, Kind::Helm | Kind::Kubernetes | Kind::Ansible | Kind::Terraform | Kind::Pulumi)
}

/// The block holding `path` (the deepest root), else the packages under it, when it is a
/// folder of them (`COPY crates crates`). For a job's paths (`job`), images count too, and so
/// does a folder of what deploys (`deploy/helm/**`: its charts).
fn within(blocks: &[Block], path: &str, job: bool) -> Vec<String> {
    let candidates = || blocks.iter().filter(|b| b.kind != Kind::Service && (job || b.kind != Kind::Docker));
    let mut best: Option<(&Block, usize)> = None;
    for b in candidates() {
        for r in &b.roots {
            if holds(r, path) && best.is_none_or(|(_, d)| depth(r) > d) {
                best = Some((b, depth(r)));
            }
        }
    }
    match best {
        Some((b, _)) => vec![b.id.clone()],
        None => candidates()
            .filter(|b| (is_package(b.kind) || (job && deploys(b.kind))) && b.roots.iter().any(|r| holds(path, r)))
            .map(|b| b.id.clone())
            .collect(),
    }
}

/// Each block's parent: the block with the deepest root holding its first one, but one that is
/// already inside it (blocks of several roots may hold each other's first one): never a loop.
fn parents(blocks: &mut [Block]) {
    let roots: Vec<(usize, String)> = blocks.iter().enumerate().flat_map(|(i, b)| b.roots.iter().map(move |r| (i, r.clone()))).collect();
    let mut parent_of: Vec<Option<usize>> = vec![None; blocks.len()];
    for i in 0..blocks.len() {
        let Some(first) = blocks[i].roots.first() else { continue };
        let mut holding: Vec<&(usize, String)> = roots.iter().filter(|(j, r)| *j != i && r != first && holds(r, first)).collect();
        holding.sort_by_key(|(_, r)| std::cmp::Reverse(depth(r)));
        // whether `j` is inside `i`, by the parents settled so far
        let inside = |mut j: usize| {
            while let Some(up) = parent_of[j] {
                if up == i {
                    return true;
                }
                j = up;
            }
            false
        };
        parent_of[i] = holding.into_iter().map(|(j, _)| *j).find(|j| !inside(*j));
    }
    for (i, parent) in parent_of.into_iter().enumerate() {
        blocks[i].parent = parent.map(|j| blocks[j].id.clone());
    }
}

/// Roots to blocks, for finding a path's block by walking up its folders.
struct Index(HashMap<String, usize>);

impl Index {
    fn new(blocks: &[Block]) -> Self {
        let mut by_root = HashMap::new();
        for (i, b) in blocks.iter().enumerate() {
            for r in &b.roots {
                by_root.entry(r.clone()).or_insert(i);
            }
        }
        Self(by_root)
    }

    fn of(&self, path: &str) -> Option<usize> {
        let mut cur = path;
        loop {
            if let Some(i) = self.0.get(cur) {
                return Some(*i);
            }
            match cur.rsplit_once('/') {
                Some((up, _)) => cur = up,
                None => return self.0.get(".").copied(),
            }
        }
    }
}

fn languages(files: &[&str]) -> Vec<Language> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for f in files {
        if let Some(l) = lang::of(f) {
            *count.entry(l).or_default() += 1;
        }
    }
    let mut out: Vec<Language> = count.into_iter().map(|(name, files)| Language { name: name.to_string(), files }).collect();
    // code first: a crate is Rust, though its README and Cargo.toml outnumber its one source file
    let prose = |l: &Language| matches!(l.name.as_str(), "Markdown" | "JSON" | "YAML" | "TOML");
    out.sort_by(|a, b| prose(a).cmp(&prose(b)).then_with(|| b.files.cmp(&a.files)).then_with(|| a.name.cmp(&b.name)));
    out
}

/// The CI jobs of each block: those whose paths it holds (or which hold it), and those
/// assay.toml gives it by name or glob.
fn assign_jobs(blocks: &mut [Block], jobs: &[Job], config: &Config) {
    for job in jobs {
        let mut ids: Vec<String> = Vec::new();
        for p in job.paths.iter().map(|p| literal_prefix(p)).filter(|p| !p.is_empty()) {
            ids.extend(within(blocks, &p, true));
        }
        for b in blocks.iter_mut().filter(|b| ids.contains(&b.id)) {
            if !b.jobs.contains(&job.name) {
                b.jobs.push(job.name.clone());
            }
        }
    }
    for b in blocks.iter_mut() {
        let Some(patterns) = config.blocks.get(&b.id).map(|c| &c.jobs).filter(|j| !j.is_empty()) else { continue };
        let globs: Vec<globset::GlobMatcher> =
            patterns.iter().filter_map(|p| globset::Glob::new(p).ok()).map(|g| g.compile_matcher()).collect();
        let mut names: Vec<&str> = jobs.iter().map(|j| j.name.as_str()).filter(|n| globs.iter().any(|g| g.is_match(n))).collect();
        // a plain name stands for a job even when the CI could not be read (includes)
        names.extend(patterns.iter().map(String::as_str).filter(|p| !p.contains(['*', '?', '[', '{'])));
        for n in names {
            if !b.jobs.iter().any(|j| j == n) {
                b.jobs.push(n.to_string());
            }
        }
    }
}

/// The people declared for each block: CODEOWNERS' owners of at least half of its files, and
/// assay.toml's referents.
fn owners(s: &Snapshot, blocks: &mut [Block], per_block: &[Vec<&str>], config: &Config, warnings: &mut Vec<String>) {
    let rules = codeowners::LOCATIONS.iter().find(|l| s.files.iter().any(|f| f == *l)).and_then(|l| {
        let text = s.texts.get(*l).ok_or_else(|| anyhow::anyhow!("could not be read"));
        match text.and_then(|t| codeowners::Codeowners::parse(t)) {
            Ok(c) => Some(c),
            Err(e) => {
                warnings.push(format!("{l}: {e}"));
                None
            }
        }
    });
    for (b, files) in blocks.iter_mut().zip(per_block) {
        if let Some(rules) = &rules
            && !files.is_empty()
        {
            let mut count: Vec<(String, usize)> = Vec::new();
            for f in files {
                for o in rules.owners_of(f) {
                    match count.iter_mut().find(|(x, _)| *x == o) {
                        Some((_, n)) => *n += 1,
                        None => count.push((o, 1)),
                    }
                }
            }
            b.owners = count.into_iter().filter(|(_, n)| n * 2 >= files.len()).map(|(o, _)| o).collect();
        }
        for r in config.blocks.get(&b.id).map(|c| c.referents.as_slice()).unwrap_or_default() {
            if !b.owners.contains(r) {
                b.owners.push(r.clone());
            }
        }
    }
}
