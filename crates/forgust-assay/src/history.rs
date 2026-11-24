//! The history of a branch's map: what each of its commits changed to it, a block or a link that
//! came or went, a block renamed or turned into another kind. It is read from git, without a
//! working copy: each commit whose files could change the map has them gathered into a
//! [`Snapshot`] and mapped, oldest first. The others (only the text of files assay does not
//! read) leave the map as it was. What a merge brings is told by the commits of its branch that
//! made it, so that a release's merge does not stand for all the work it brings.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::{Block, Kind, Link, LinkKind, Options, RepoMap, Role, SCHEMA, Snapshot, Unassigned};

/// The most maps a history makes by default: the latest commits that could change the map, so
/// that a long history reads in a few minutes. The rest of it is not told.
pub const MAX_MAPS: usize = 2000;
/// The most maps made of a merged branch, to find which of its commits made a change: past it,
/// the merge tells its changes itself.
const MAX_BRANCH_MAPS: usize = 300;
const SYMLINK: &str = "120000";
const GITLINK: &str = "160000";

#[derive(Debug, Clone)]
pub struct HistoryOptions {
    /// The last commit already read: only those after it are, when the branch still has it.
    pub since: Option<String>,
    /// The most maps to make: [`MAX_MAPS`].
    pub max: usize,
}

impl Default for HistoryOptions {
    fn default() -> Self {
        Self { since: None, max: MAX_MAPS }
    }
}

/// What the commits of a branch changed to its map.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    /// The commit its chapters follow: the `since` asked, which the branch still has. None: they
    /// are the whole history read, from `start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Where the history read starts when it does not go back to the first commit (that would
    /// have taken more maps): the map of this commit is taken as it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Start>,
    /// The commits that changed the map, oldest first.
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Start {
    pub commit: String,
    /// Its commit time, in seconds since 1970.
    pub at: i64,
}

/// A commit that changed the map, and what it changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub commit: String,
    /// The merge that brought its changes to the branch, when it was made on the branch merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<String>,
    pub author: String,
    pub email: String,
    /// When its changes reached the branch: the commit time of the branch's own commit (the
    /// merge's), in seconds since 1970.
    pub at: i64,
    /// The first line of its message.
    pub title: String,
    pub changes: Changes,
}

/// What changed to a map. A block found again under another id, in the same manifest (a service:
/// running the same image), is renamed rather than removed and added, and its links follow it. A
/// role is a guess that comes and goes with the tooling: its changes are not told, a kind's are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Changes {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks_added: Vec<BlockRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks_removed: Vec<BlockRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks_renamed: Vec<Renamed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds_changed: Vec<KindChanged>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links_added: Vec<Link>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links_removed: Vec<Link>,
}

/// A block as a chapter tells it: enough to draw it as it was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockRef {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Renamed {
    pub from: BlockRef,
    pub to: BlockRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindChanged {
    pub id: String,
    pub from: Kind,
    pub to: Kind,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.blocks_added.is_empty()
            && self.blocks_removed.is_empty()
            && self.blocks_renamed.is_empty()
            && self.kinds_changed.is_empty()
            && self.links_added.is_empty()
            && self.links_removed.is_empty()
    }

    /// Whether these changes put back exactly what `earlier` changed: a revert's.
    pub fn undoes(&self, earlier: &Changes) -> bool {
        fn ids(list: &[BlockRef]) -> HashSet<&str> {
            list.iter().map(|b| b.id.as_str()).collect()
        }
        fn links(list: &[Link]) -> HashSet<(&str, &str, LinkKind)> {
            list.iter().map(|l| (l.from.as_str(), l.to.as_str(), l.kind)).collect()
        }
        let renames = |list: &[Renamed], back: bool| -> HashSet<(String, String)> {
            list.iter().map(|r| if back { (r.to.id.clone(), r.from.id.clone()) } else { (r.from.id.clone(), r.to.id.clone()) }).collect()
        };
        let kinds = |list: &[KindChanged], back: bool| -> HashSet<(String, Kind, Kind)> {
            list.iter().map(|k| if back { (k.id.clone(), k.to, k.from) } else { (k.id.clone(), k.from, k.to) }).collect()
        };
        !self.is_empty()
            && ids(&self.blocks_added) == ids(&earlier.blocks_removed)
            && ids(&self.blocks_removed) == ids(&earlier.blocks_added)
            && renames(&self.blocks_renamed, false) == renames(&earlier.blocks_renamed, true)
            && kinds(&self.kinds_changed, false) == kinds(&earlier.kinds_changed, true)
            && links(&self.links_added) == links(&earlier.links_removed)
            && links(&self.links_removed) == links(&earlier.links_added)
    }

    /// Takes out of these changes those `made` also has, and gives them.
    fn take_common(&mut self, made: &Changes) -> Changes {
        fn take<T>(from: &mut Vec<T>, found: impl Fn(&T) -> bool) -> Vec<T> {
            let (yes, no) = std::mem::take(from).into_iter().partition(found);
            *from = no;
            yes
        }
        let same_link = |a: &Link, b: &Link| a.from == b.from && a.to == b.to && a.kind == b.kind;
        Changes {
            blocks_added: take(&mut self.blocks_added, |b| made.blocks_added.iter().any(|m| m.id == b.id)),
            blocks_removed: take(&mut self.blocks_removed, |b| made.blocks_removed.iter().any(|m| m.id == b.id)),
            blocks_renamed: take(&mut self.blocks_renamed, |r| {
                made.blocks_renamed.iter().any(|m| m.from.id == r.from.id && m.to.id == r.to.id)
            }),
            kinds_changed: take(&mut self.kinds_changed, |k| made.kinds_changed.iter().any(|m| m.id == k.id && m.to == k.to)),
            links_added: take(&mut self.links_added, |l| made.links_added.iter().any(|m| same_link(m, l))),
            links_removed: take(&mut self.links_removed, |l| made.links_removed.iter().any(|m| same_link(m, l))),
        }
    }
}

/// What changed from the map `before` to `after`.
pub fn changes(before: &RepoMap, after: &RepoMap) -> Changes {
    let had: HashMap<&str, &Block> = before.blocks.iter().map(|b| (b.id.as_str(), b)).collect();
    let has: HashSet<&str> = after.blocks.iter().map(|b| b.id.as_str()).collect();
    let mut added: Vec<&Block> = after.blocks.iter().filter(|b| !had.contains_key(b.id.as_str())).collect();
    let mut removed: Vec<&Block> = before.blocks.iter().filter(|b| !has.contains(b.id.as_str())).collect();
    let mut out = Changes::default();
    removed.retain(|r| match added.iter().position(|a| same_block(r, a)) {
        Some(i) => {
            let a = added.remove(i);
            out.blocks_renamed.push(Renamed { from: block_ref(r), to: block_ref(a) });
            false
        }
        None => true,
    });
    out.blocks_added = added.into_iter().map(block_ref).collect();
    out.blocks_removed = removed.into_iter().map(block_ref).collect();
    out.kinds_changed = after
        .blocks
        .iter()
        .filter_map(|b| {
            had.get(b.id.as_str()).filter(|o| o.kind != b.kind).map(|o| KindChanged { id: b.id.clone(), from: o.kind, to: b.kind })
        })
        .collect();
    // a renamed block's links are the same under its new id
    let renamed: HashMap<String, String> = out.blocks_renamed.iter().map(|r| (r.from.id.clone(), r.to.id.clone())).collect();
    let now = |id: &str| renamed.get(id).cloned().unwrap_or_else(|| id.to_string());
    let were: HashSet<(String, String, LinkKind)> = before.links.iter().map(|l| (now(&l.from), now(&l.to), l.kind)).collect();
    let are: HashSet<(&str, &str, LinkKind)> = after.links.iter().map(|l| (l.from.as_str(), l.to.as_str(), l.kind)).collect();
    out.links_added = after.links.iter().filter(|l| !were.contains(&(l.from.clone(), l.to.clone(), l.kind))).cloned().collect();
    out.links_removed =
        before.links.iter().filter(|l| !are.contains(&(now(&l.from).as_str(), now(&l.to).as_str(), l.kind))).cloned().collect();
    out
}

/// Whether `after` is `before` under another id: of the same kind, and found in the same manifest
/// or holding the same folders. Services share their compose file: one is the other when it runs
/// the same image.
fn same_block(before: &Block, after: &Block) -> bool {
    if before.kind != after.kind {
        return false;
    }
    if before.kind == Kind::Service {
        return before.image.is_some() && before.image == after.image;
    }
    before.manifests.iter().any(|m| after.manifests.contains(m)) || (!before.roots.is_empty() && before.roots == after.roots)
}

fn block_ref(b: &Block) -> BlockRef {
    BlockRef { id: b.id.clone(), name: b.name.clone(), kind: b.kind, role: b.role }
}

/// What the commits of the branch at `root`'s HEAD, a git clone, changed to its map, along first
/// parents: those after `since` when the branch still has it, else from the first commit, as far
/// back as `max` maps go. `opts.name` names the block of the top folder, as for its map.
pub fn history(root: &Path, opts: &Options, h: &HistoryOptions) -> anyhow::Result<History> {
    let root = root.canonicalize()?;
    let git = Git(&root);
    let name = opts.name.clone().unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let since = h.since.as_deref().filter(|s| git.ok(&["merge-base", "--is-ancestor", s, "HEAD"]));
    let not_since = since.map(|s| format!("^{s}"));
    let range: Vec<&str> = std::iter::once("HEAD").chain(not_since.as_deref()).collect();

    // the commits that could change the map, the latest first, as many as there are maps to make
    let mut log = Log::new(&git, &range)?;
    let mut kept = Vec::new();
    let mut cut = false;
    while let Some(c) = log.next()? {
        if !c.may_change_map() {
            continue;
        }
        if kept.len() == h.max {
            cut = true;
            break;
        }
        kept.push(c);
    }
    drop(log);
    kept.reverse();

    // the map the first of them changes: since's, or the commit before them when the history is
    // cut; else none, the first commit's changes are all it holds
    let base = if cut { kept.first().and_then(|c| c.parents.first().cloned()) } else { since.map(str::to_string) };
    let mut blobs = Blobs::new(&git)?;
    let mut tree = match &base {
        Some(b) => Tree::at(&git, &mut blobs, &name, b)?,
        None => Tree::new(&name),
    };
    let mut before = match &base {
        Some(b) => tree.map(b),
        None => Some(empty()),
    };
    let mut chapters = Vec::new();
    for c in &kept {
        tree.apply(&mut blobs, &c.files)?;
        // an assay.toml that does not read: the commits until it does change nothing
        let Some(after) = tree.map(&c.sha) else { continue };
        match before.as_ref().map(|b| changes(b, &after)) {
            Some(made) if made.is_empty() => {}
            Some(made) if c.parents.len() > 1 => chapters.extend(merged(&git, &mut blobs, &name, c, made)?),
            Some(made) => chapters.push(chapter(c, None, c.at, made)),
            None => {}
        }
        before = Some(after);
    }
    let start = match base.filter(|_| cut) {
        Some(commit) => Some(Start { at: git.time(&commit)?, commit }),
        None => None,
    };
    Ok(History { since: since.filter(|_| !cut).map(str::to_string), start, chapters })
}

/// The chapters of what a merge brought (`net`): each change told by the commit of the merged
/// branch that made it first, and those none made by the merge itself.
fn merged(git: &Git, blobs: &mut Blobs, name: &str, merge: &Commit, net: Changes) -> anyhow::Result<Vec<Chapter>> {
    let alone = |net: Changes| -> anyhow::Result<Vec<Chapter>> { Ok(vec![chapter(merge, None, merge.at, net)]) };
    let (Some(first), Some(other)) = (merge.parents.first(), merge.parents.get(1)) else { return alone(net) };
    let not_first = format!("^{first}");
    let mut log = Log::new(git, &[other.as_str(), not_first.as_str()])?;
    let mut branch = Vec::new();
    while let Some(c) = log.next()? {
        if c.may_change_map() {
            if branch.len() == MAX_BRANCH_MAPS {
                return alone(net);
            }
            branch.push(c);
        }
    }
    drop(log);
    branch.reverse();
    let Some(fork) = branch.first().map(|c| c.parents.first().cloned()) else { return alone(net) };
    let mut tree = match &fork {
        Some(f) => Tree::at(git, blobs, name, f)?,
        None => Tree::new(name),
    };
    let mut before = match &fork {
        Some(f) => tree.map(f),
        None => Some(empty()),
    };
    let mut left = net;
    let mut out = Vec::new();
    for c in &branch {
        if left.is_empty() {
            break;
        }
        tree.apply(blobs, &c.files)?;
        let Some(after) = tree.map(&c.sha) else { continue };
        if let Some(b) = &before {
            let made = left.take_common(&changes(b, &after));
            if !made.is_empty() {
                out.push(chapter(c, Some(&merge.sha), merge.at, made));
            }
        }
        before = Some(after);
    }
    if !left.is_empty() {
        out.push(chapter(merge, None, merge.at, left));
    }
    Ok(out)
}

fn chapter(c: &Commit, merge: Option<&str>, at: i64, changes: Changes) -> Chapter {
    Chapter {
        commit: c.sha.clone(),
        merge: merge.map(str::to_string),
        author: c.author.clone(),
        email: c.email.clone(),
        at,
        title: c.title.clone(),
        changes,
    }
}

fn empty() -> RepoMap {
    RepoMap { schema: SCHEMA, commit: None, blocks: Vec::new(), links: Vec::new(), unassigned: Unassigned::default(), warnings: Vec::new() }
}

impl History {
    /// The chapters as text to read, oldest first: what each commit added (`+`), removed (`-`),
    /// renamed (`»`) or turned into another kind (`~`).
    pub fn story(&self) -> String {
        let mut out = String::new();
        if let Some(s) = &self.start {
            out.push_str(&format!(
                "From {} ({}): what came before is not read.\n\n",
                &s.commit[..s.commit.len().min(10)],
                crate::people::date(s.at)
            ));
        }
        for c in &self.chapters {
            out.push_str(&format!("{}  {}  {}: {}\n", crate::people::date(c.at), &c.commit[..c.commit.len().min(10)], c.author, c.title));
            let ch = &c.changes;
            let kind =
                |b: &BlockRef| format!("{}{}", crate::kind_name(b.kind), b.role.map(|r| format!(", {}", role_name(r))).unwrap_or_default());
            for b in &ch.blocks_added {
                out.push_str(&format!("  + {} ({})\n", b.id, kind(b)));
            }
            for b in &ch.blocks_removed {
                out.push_str(&format!("  - {} ({})\n", b.id, kind(b)));
            }
            for r in &ch.blocks_renamed {
                out.push_str(&format!("  » {} is now {}\n", r.from.id, r.to.id));
            }
            for k in &ch.kinds_changed {
                out.push_str(&format!("  ~ {}: {} -> {}\n", k.id, crate::kind_name(k.from), crate::kind_name(k.to)));
            }
            for (sign, links) in [('+', &ch.links_added), ('-', &ch.links_removed)] {
                out.push_str(&links_text(sign, links));
            }
            out.push('\n');
        }
        if self.chapters.is_empty() {
            out.push_str("No commit changed the map.\n");
        }
        out
    }
}

/// Links as lines; an image built from many blocks (a Dockerfile copying the whole repository) is
/// one line rather than one each.
fn links_text(sign: char, links: &[Link]) -> String {
    let mut builds: HashMap<&str, Vec<&str>> = HashMap::new();
    for l in links.iter().filter(|l| l.kind == LinkKind::Builds) {
        builds.entry(&l.from).or_default().push(&l.to);
    }
    let mut out = String::new();
    let mut said = HashSet::new();
    for l in links {
        let many = builds.get(l.from.as_str()).filter(|to| l.kind == LinkKind::Builds && to.len() >= 3);
        match many {
            Some(to) if said.insert(l.from.as_str()) => {
                out.push_str(&format!("  {sign} {} builds {} blocks: {}\n", l.from, to.len(), to.join(", ")))
            }
            Some(_) => {}
            None => {
                let guess = if l.certainty == crate::Certainty::Inferred { " (a guess)" } else { "" };
                out.push_str(&format!("  {sign} {} {} {}{guess}\n", l.from, link_verb(l.kind), l.to));
            }
        }
    }
    out
}

fn link_verb(k: LinkKind) -> &'static str {
    match k {
        LinkKind::Uses => "uses",
        LinkKind::Builds => "builds",
        LinkKind::Runs => "runs",
        LinkKind::DependsOn => "depends on",
        LinkKind::Connects => "connects to",
        LinkKind::Calls => "calls",
    }
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::Web => "web",
        Role::Api => "api",
        Role::Cli => "cli",
        Role::Library => "library",
        Role::Database => "database",
        Role::Cache => "cache",
        Role::Queue => "queue",
        Role::Search => "search",
        Role::Storage => "storage",
        Role::Mail => "mail",
    }
}

/// The files of a commit as assay reads them, carried from commit to commit.
struct Tree {
    files: BTreeSet<String>,
    snapshot: Snapshot,
    /// `files` changed since `snapshot.files` was made from them.
    stale: bool,
}

impl Tree {
    fn new(name: &str) -> Self {
        Self { files: BTreeSet::new(), snapshot: Snapshot { name: name.to_string(), ..Default::default() }, stale: false }
    }

    /// The files of `commit`, and the text of those assay reads.
    fn at(git: &Git, blobs: &mut Blobs, name: &str, commit: &str) -> anyhow::Result<Self> {
        let mut tree = Self::new(name);
        let out = git.out(&["ls-tree", "-r", "-z", "--full-tree", commit])?;
        for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            // <mode> <type> <object>\t<path>
            let Some(tab) = entry.iter().position(|b| *b == b'\t') else { continue };
            let meta = String::from_utf8_lossy(&entry[..tab]);
            let [mode, kind, object] = meta.split(' ').collect::<Vec<_>>()[..] else { continue };
            if kind != "blob" {
                continue;
            }
            let path = String::from_utf8_lossy(&entry[tab + 1..]).into_owned();
            if mode != SYMLINK
                && crate::reads(&path)
                && let Some(text) = blobs.text(object)?
            {
                tree.snapshot.texts.insert(path.clone(), text);
            }
            tree.files.insert(path);
        }
        tree.stale = true;
        Ok(tree)
    }

    /// A commit's changes to the files.
    fn apply(&mut self, blobs: &mut Blobs, changes: &[FileChange]) -> anyhow::Result<()> {
        for c in changes {
            // a submodule is no file of the repository's
            if c.status == b'D' || c.mode == GITLINK {
                self.stale |= self.files.remove(&c.path);
                self.snapshot.texts.remove(&c.path);
                continue;
            }
            self.stale |= self.files.insert(c.path.clone());
            let text = if c.mode != SYMLINK && crate::reads(&c.path) { blobs.text(&c.blob)? } else { None };
            match text {
                Some(t) => self.snapshot.texts.insert(c.path.clone(), t),
                None => self.snapshot.texts.remove(&c.path),
            };
        }
        Ok(())
    }

    /// The map of the files as they are, at `commit`; none when its assay.toml does not read.
    fn map(&mut self, commit: &str) -> Option<RepoMap> {
        if self.stale {
            self.snapshot.files = self.files.iter().cloned().collect();
            self.stale = false;
        }
        self.snapshot.commit = Some(commit.to_string());
        crate::map_snapshot(&self.snapshot).ok()
    }
}

/// A commit along first parents, with what it changed to the files from its first parent.
struct Commit {
    sha: String,
    parents: Vec<String>,
    author: String,
    email: String,
    at: i64,
    title: String,
    files: Vec<FileChange>,
}

struct FileChange {
    /// `A`, `M`, `D` or `T` (a file turned into a link, or back).
    status: u8,
    /// Its mode and blob after the commit.
    mode: String,
    blob: String,
    path: String,
}

impl Commit {
    /// Whether it could change the map: it adds or removes files (the folders a block holds, what
    /// tells some), or changes the text of one assay reads.
    fn may_change_map(&self) -> bool {
        self.files.iter().any(|f| f.status != b'M' || crate::reads(&f.path))
    }
}

/// `git log` along first parents, the latest first: each commit's head (`\x01`, then its fields,
/// each ended by `\x02`), then its changes from its first parent as `--raw -z` prints them: `:`,
/// the modes, the blobs and the status, a NUL, the path, a NUL.
struct Log {
    child: Child,
    out: BufReader<ChildStdout>,
}

const LOG_FORMAT: &str = "--format=%x01%H%x02%P%x02%aN%x02%aE%x02%ct%x02%s%x02";

impl Log {
    fn new(git: &Git, range: &[&str]) -> anyhow::Result<Self> {
        let mut child = git
            .cmd()
            .args([
                "log",
                "--first-parent",
                "--diff-merges=first-parent",
                "--root",
                "--raw",
                "--no-renames",
                "--no-abbrev",
                "-z",
                LOG_FORMAT,
            ])
            .args(range)
            .arg("--")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut out = BufReader::new(child.stdout.take().ok_or_else(|| anyhow::anyhow!("git log: no output"))?);
        // up to the first commit's mark
        out.read_until(1, &mut Vec::new())?;
        Ok(Self { child, out })
    }

    fn next(&mut self) -> anyhow::Result<Option<Commit>> {
        let mut buf = Vec::new();
        if self.out.read_until(1, &mut buf)? == 0 {
            anyhow::ensure!(self.child.wait()?.success(), "git log failed");
            return Ok(None);
        }
        if buf.last() == Some(&1) {
            buf.pop();
        }
        parse_commit(&buf).map(Some)
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        // read no further than needed
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_commit(chunk: &[u8]) -> anyhow::Result<Commit> {
    let mut fields = Vec::with_capacity(6);
    let mut rest = chunk;
    for _ in 0..6 {
        let end = rest.iter().position(|b| *b == 2).ok_or_else(|| anyhow::anyhow!("git log: a commit's head is cut short"))?;
        fields.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = &rest[end + 1..];
    }
    let mut files = Vec::new();
    let mut tokens = rest.split(|b| *b == 0);
    while let Some(t) = tokens.next() {
        let t = t.trim_ascii_start();
        let Some(meta) = t.strip_prefix(b":") else { continue };
        let Some(path) = tokens.next() else { break };
        let meta = String::from_utf8_lossy(meta);
        // old mode, new mode, old blob, new blob, status
        let [_, mode, _, blob, status] = meta.split(' ').collect::<Vec<_>>()[..] else { continue };
        files.push(FileChange {
            status: status.as_bytes().first().copied().unwrap_or(b'M'),
            mode: mode.to_string(),
            blob: blob.to_string(),
            path: String::from_utf8_lossy(path).into_owned(),
        });
    }
    let [sha, parents, author, email, at, title] = <[String; 6]>::try_from(fields).map_err(|_| anyhow::anyhow!("git log: six fields"))?;
    Ok(Commit {
        parents: parents.split_whitespace().map(str::to_string).collect(),
        sha,
        author,
        email,
        at: at.parse().unwrap_or(0),
        title,
        files,
    })
}

/// The text of blobs, by `git cat-file --batch`.
struct Blobs {
    child: Child,
    input: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
}

impl Blobs {
    fn new(git: &Git) -> anyhow::Result<Self> {
        let mut child =
            git.cmd().args(["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let input = child.stdin.take();
        let out = BufReader::new(child.stdout.take().ok_or_else(|| anyhow::anyhow!("git cat-file: no output"))?);
        Ok(Self { child, input, out })
    }

    /// A blob's text; none when it is not UTF-8 (a file assay could not read either), or not there.
    fn text(&mut self, blob: &str) -> anyhow::Result<Option<String>> {
        let input = self.input.as_mut().ok_or_else(|| anyhow::anyhow!("git cat-file: closed"))?;
        writeln!(input, "{blob}")?;
        input.flush()?;
        let mut head = String::new();
        self.out.read_line(&mut head)?;
        // `<blob> <type> <size>`, or `<blob> missing`
        let [_, _, size] = head.trim_end().split(' ').collect::<Vec<_>>()[..] else { return Ok(None) };
        let mut data = vec![0; size.parse::<usize>()? + 1];
        self.out.read_exact(&mut data)?;
        data.pop();
        Ok(String::from_utf8(data).ok())
    }
}

impl Drop for Blobs {
    fn drop(&mut self) {
        // its input closed, it ends
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

struct Git<'a>(&'a Path);

impl Git<'_> {
    fn cmd(&self) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(self.0);
        c
    }

    fn out(&self, args: &[&str]) -> anyhow::Result<Vec<u8>> {
        let out = self.cmd().args(args).output()?;
        anyhow::ensure!(out.status.success(), "git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        Ok(out.stdout)
    }

    fn ok(&self, args: &[&str]) -> bool {
        self.cmd().args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }

    /// A commit's commit time.
    fn time(&self, commit: &str) -> anyhow::Result<i64> {
        Ok(String::from_utf8_lossy(&self.out(&["log", "-1", "--format=%ct", commit])?).trim().parse()?)
    }
}
