//! Git storage service (ADR-0002, ADR-0012).
//!
//! The [`Storage`] trait is the boundary every caller uses. v1 ships one in-process
//! implementation, [`LocalStorage`]: git processes for writes and transfer, a pool of
//! long-lived `git cat-file --batch-command` processes, and gitoxide for hot reads on
//! repos it can open. Callers never see which backend served a request.

mod catfile;
mod cli;
mod diff;
mod local;
mod types;

pub use diff::{Diff, DiffFile, DiffHunk, DiffLine, DiffLineKind, DiffOptions, FileStat, map_old_line_to_new, parse_unified_diff};
pub use local::{LocalStorage, LocalStorageConfig, REBASE_PREFIX, replay_rebases};
pub use types::*;

use async_trait::async_trait;
use std::path::Path;

pub type Result<T, E = StorageError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("repository {0} not found")]
    RepoNotFound(i64),
    #[error("repository {0} already exists")]
    RepoExists(i64),
    #[error("revision not found: {0}")]
    RevisionNotFound(String),
    #[error("path not found: {0}")]
    PathNotFound(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("git command failed: {0}")]
    Git(String),
    #[error("merge conflict in {0:?}")]
    Conflict(Vec<String>),
    #[error("ref {0} changed concurrently")]
    RefChanged(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("too many git processes are running; try again")]
    Busy,
}

/// The storage boundary. All methods are keyed by repo ID (ADR-0009).
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    async fn create_repo(&self, repo: i64, opts: CreateRepoOptions) -> Result<()>;
    async fn delete_repo(&self, repo: i64) -> Result<()>;
    async fn repo_exists(&self, repo: i64) -> Result<bool>;
    async fn set_default_branch(&self, repo: i64, branch: &str) -> Result<()>;
    async fn default_branch(&self, repo: i64) -> Result<Option<String>>;
    async fn is_empty(&self, repo: i64) -> Result<bool>;

    /// Spawn `git upload-pack` / `git receive-pack` and hand back its pipes.
    async fn spawn_service(&self, repo: i64, service: GitService, opts: ServiceOptions) -> Result<ServiceProcess>;

    async fn resolve(&self, repo: i64, rev: &str) -> Result<Option<String>>;
    async fn refs(&self, repo: i64, prefix: &str) -> Result<Vec<RefInfo>>;
    async fn tree(&self, repo: i64, commit: &str, path: &str) -> Result<Vec<TreeEntry>>;
    /// Every file path at `commit` (at most `max`); the bool says it was cut short.
    async fn list_paths(&self, repo: i64, commit: &str, max: usize) -> Result<(Vec<String>, bool)>;
    /// Every blob at `commit` with its size (at most `max`); the bool says it was cut short.
    async fn tree_sizes(&self, repo: i64, commit: &str, max: usize) -> Result<(Vec<(String, u64)>, bool)>;
    /// Bytes the repository's objects take on disk (loose and packed).
    async fn disk_size(&self, repo: i64) -> Result<u64>;
    async fn blob(&self, repo: i64, commit: &str, path: &str, max_bytes: usize) -> Result<Blob>;
    async fn commit(&self, repo: i64, rev: &str) -> Result<Option<CommitInfo>>;
    async fn log(&self, repo: i64, rev: &str, path: Option<&str>, skip: usize, limit: usize) -> Result<Vec<CommitInfo>>;
    /// Commits reachable from `rev`.
    async fn commit_count(&self, repo: i64, rev: &str) -> Result<u64>;
    /// Commits reachable from `to` but not from `from` (`from..to`), newest first.
    async fn log_between(&self, repo: i64, from: &str, to: &str, limit: usize) -> Result<Vec<CommitInfo>>;
    async fn diff(&self, repo: i64, from: Option<&str>, to: &str, opts: DiffOptions) -> Result<Diff>;
    async fn blame(&self, repo: i64, commit: &str, path: &str) -> Result<Vec<BlameHunk>>;
    async fn archive(&self, repo: i64, commit: &str, format: ArchiveFormat, prefix: &str) -> Result<ServiceProcess>;
    async fn is_ancestor(&self, repo: i64, ancestor: &str, descendant: &str) -> Result<bool>;
    async fn merge_base(&self, repo: i64, a: &str, b: &str) -> Result<Option<String>>;

    async fn maintenance(&self, repo: i64, full: bool) -> Result<()>;
    /// Write a bundle of all refs; returns false for an empty repo (nothing to bundle).
    async fn bundle_create(&self, repo: i64, dest: &Path) -> Result<bool>;
    async fn restore_from_bundle(&self, repo: i64, bundle: Option<&Path>, opts: CreateRepoOptions) -> Result<()>;
    async fn ref_format(&self, repo: i64) -> Result<RefFormat>;

    /// Set `name` to `new`. With `expected_old`, only if the ref currently has that
    /// value (`ZERO_OID` = must not exist): compare-and-swap.
    async fn update_ref(&self, repo: i64, name: &str, new: &str, expected_old: Option<&str>) -> Result<()>;
    async fn delete_ref(&self, repo: i64, name: &str, expected_old: Option<&str>) -> Result<()>;
    /// Set many refs in one transaction (no compare-and-swap): imports.
    async fn update_refs(&self, repo: i64, updates: &[(String, String)]) -> Result<()>;
    /// In-memory merge of `theirs` into `ours` (no working copy).
    async fn merge_tree(&self, repo: i64, ours: &str, theirs: &str, merge_base: Option<&str>) -> Result<MergeTreeResult>;
    /// Write an annotated tag object `name` pointing at commit `target`; returns its id
    /// (the ref is set separately, with `update_ref`).
    async fn tag_object(&self, repo: i64, name: &str, target: &str, message: &str, tagger: &Identity) -> Result<String>;
    async fn commit_tree(
        &self,
        repo: i64,
        tree: &str,
        parents: &[String],
        message: &str,
        author: &Identity,
        committer: &Identity,
    ) -> Result<String>;
    /// The tree of `base` (a commit) with `changes` applied: a file's new
    /// content, or `None` to delete it. Keeps each existing file's mode (new
    /// files are 100644); refuses paths that are directories, symlinks or
    /// submodules. No working copy is involved.
    async fn tree_with_changes(&self, repo: i64, base: &str, changes: &[FileChange]) -> Result<String>;
    /// Commits in `from..to`, oldest first.
    async fn rev_list(&self, repo: i64, from: &str, to: &str, merges_only: bool) -> Result<Vec<String>>;
    /// Replay the commits of `upstream..tip` onto `onto`; returns the new tip.
    /// Uses `git replay`, falling back to one `merge-tree` per commit. Refuses
    /// ranges with merge commits.
    async fn rebase(&self, repo: i64, onto: &str, upstream: &str, tip: &str, committer: &Identity) -> Result<String>;
    /// Fetch `refspecs` from another server over http(s) (imports). `config` is
    /// passed as `GIT_CONFIG_*` environment variables (credentials, pinned
    /// addresses), never on the command line.
    async fn fetch(&self, repo: i64, url: &str, refspecs: &[String], config: &[(String, String)]) -> Result<()>;
    /// Blob ids and sizes of small blobs reachable from any ref (LFS pointer scan):
    /// at most `max_size` bytes each.
    async fn small_blobs(&self, repo: i64, max_size: u64) -> Result<Vec<String>>;
    /// Raw content of a blob by id.
    async fn blob_by_id(&self, repo: i64, oid: &str, max_bytes: usize) -> Result<Vec<u8>>;

    // A push being received: its objects are still in git's quarantine
    // directory (`GIT_QUARANTINE_PATH` of the pre-receive hook, under the
    // repo's `objects/`: `tmp_objdir-incoming-*`), seen with the repo's own objects. None: no quarantine.

    /// Commits reachable from `tips` and from no ref yet: what a push adds,
    /// newest first, at most `limit`.
    async fn new_commits(&self, repo: i64, quarantine: Option<&Path>, tips: &[String], limit: usize) -> Result<Vec<String>>;
    /// Raw commit objects (headers and message, as hashed), in the order asked.
    async fn raw_commits(&self, repo: i64, quarantine: Option<&Path>, shas: &[String]) -> Result<Vec<(String, Vec<u8>)>>;
    /// Blobs a push adds (reachable from `tips`, from no ref yet): (path, size),
    /// at most `limit`.
    async fn new_blobs(&self, repo: i64, quarantine: Option<&Path>, tips: &[String], limit: usize) -> Result<Vec<(String, u64)>>;
    /// Paths that differ between two commits.
    async fn changed_paths(&self, repo: i64, quarantine: Option<&Path>, old: &str, new: &str) -> Result<Vec<String>>;

    /// Drop idle helper processes.
    fn reap_idle(&self) {}
    /// Live `git cat-file` helper processes (for metrics).
    fn catfile_processes(&self) -> usize {
        0
    }
}
