//! In-process storage backend: bare repos on local disk.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use lru::LruCache;
use sha2::{Digest, Sha256};

use crate::catfile::{CatFilePool, parse_raw_tree};
use crate::diff::{Diff, DiffOptions, parse_numstat_z, parse_unified_diff};
use crate::types::*;
use crate::{Result, Storage, StorageError, cli};

const SHORT: Duration = Duration::from_secs(30);
const LONG: Duration = Duration::from_secs(600);
/// How long a clone, fetch or archive waits for a slot under the process limit.
const SERVICE_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct LocalStorageConfig {
    pub root: PathBuf,
    pub git_bin: PathBuf,
    /// Max number of `git cat-file` processes kept alive.
    pub catfile_pool_size: usize,
    pub catfile_idle: Duration,
    /// Max open gitoxide repo handles.
    pub gix_cache_size: usize,
    /// Max git processes at once (0: four per core, at least 16). Pushes are not
    /// counted: their hooks run git commands of their own.
    pub max_processes: usize,
}

impl LocalStorageConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalStorageConfig {
            root: root.into(),
            git_bin: PathBuf::from("git"),
            catfile_pool_size: 64,
            catfile_idle: Duration::from_secs(120),
            gix_cache_size: 256,
            max_processes: 0,
        }
    }
}

pub struct LocalStorage {
    cfg: LocalStorageConfig,
    catfile: CatFilePool,
    gix: Mutex<LruCache<i64, Option<gix::ThreadSafeRepository>>>,
}

fn kind_from_mode(mode: u32) -> EntryKind {
    match mode {
        0o040000 => EntryKind::Tree,
        0o120000 => EntryKind::Symlink,
        0o160000 => EntryKind::Submodule,
        0o100755 => EntryKind::Executable,
        _ => EntryKind::Blob,
    }
}

static REPLAY_REBASES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Rebases done with `git replay` (the rest used the per-commit fallback).
pub fn replay_rebases() -> u64 {
    REPLAY_REBASES.load(std::sync::atomic::Ordering::Relaxed)
}

pub const REBASE_PREFIX: &str = "refs/heads/__forgust_rebase/";

static REBASE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct CommitMeta {
    author: Identity,
    author_date: String,
    message: String,
}

fn check_ref_name(name: &str) -> Result<()> {
    let ok = name.starts_with("refs/")
        && !name.contains("..")
        && !name.ends_with('/')
        && !name.ends_with(".lock")
        && !name.bytes().any(|b| b <= b' ' || b == 0x7f || b"~^:?*[\\".contains(&b));
    if ok { Ok(()) } else { Err(StorageError::Invalid(format!("bad ref name {name}"))) }
}

fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

fn sort_entries(entries: &mut [TreeEntry]) {
    entries.sort_by(|a, b| {
        let at = a.kind == EntryKind::Tree;
        let bt = b.kind == EntryKind::Tree;
        bt.cmp(&at).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

const LOG_FORMAT: &str = "--format=%H%x00%P%x00%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%s%x00%b%x1e";

fn parse_log(out: &[u8]) -> Vec<CommitInfo> {
    let text = String::from_utf8_lossy(out);
    text.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            if rec.is_empty() {
                return None;
            }
            let f: Vec<&str> = rec.split('\0').collect();
            if f.len() < 10 {
                return None;
            }
            Some(CommitInfo {
                id: f[0].to_string(),
                parents: f[1].split(' ').filter(|s| !s.is_empty()).map(str::to_string).collect(),
                author: Person { name: f[2].into(), email: f[3].into(), time: f[4].parse().unwrap_or(0), username: None },
                committer: Person { name: f[5].into(), email: f[6].into(), time: f[7].parse().unwrap_or(0), username: None },
                subject: f[8].to_string(),
                body: f[9].trim_end().to_string(),
            })
        })
        .collect()
}

impl LocalStorage {
    pub fn new(cfg: LocalStorageConfig) -> Self {
        cli::set_limit(if cfg.max_processes == 0 { cli::default_limit() } else { cfg.max_processes });
        let catfile = CatFilePool::new(cfg.git_bin.clone(), cfg.catfile_pool_size, cfg.catfile_idle);
        let cap = NonZeroUsize::new(cfg.gix_cache_size.max(1)).unwrap();
        LocalStorage { cfg, catfile, gix: Mutex::new(LruCache::new(cap)) }
    }

    pub fn config(&self) -> &LocalStorageConfig {
        &self.cfg
    }

    /// `@repos/ab/cd/<sha256(id)>.git` — never derived from names (ADR-0009).
    pub fn repo_path(&self, repo: i64) -> PathBuf {
        let h = hex::encode(Sha256::digest(repo.to_string().as_bytes()));
        self.cfg.root.join("@repos").join(&h[0..2]).join(&h[2..4]).join(format!("{h}.git"))
    }

    fn git(&self, repo: i64) -> tokio::process::Command {
        cli::base(&self.cfg.git_bin, &self.repo_path(repo))
    }

    /// git for `repo` that also sees a push's quarantined objects. The
    /// quarantine must be one of the repo's own `objects/incoming-*` dirs.
    async fn git_q(&self, repo: i64, quarantine: Option<&Path>) -> Result<tokio::process::Command> {
        let path = self.ensure(repo).await?;
        let mut c = self.git(repo);
        if let Some(q) = quarantine {
            // git's own: <repo>/objects/tmp_objdir-incoming-XXXXXX
            let objects = tokio::fs::canonicalize(path.join("objects")).await?;
            let q = tokio::fs::canonicalize(q).await.map_err(|_| StorageError::Invalid("bad quarantine directory".into()))?;
            let ok = q.parent() == Some(objects.as_path())
                && q.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("tmp_objdir-incoming-") || n.starts_with("incoming-"));
            if !ok {
                return Err(StorageError::Invalid("bad quarantine directory".into()));
            }
            c.env("GIT_OBJECT_DIRECTORY", q).env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &objects);
        }
        Ok(c)
    }

    async fn ensure(&self, repo: i64) -> Result<PathBuf> {
        let p = self.repo_path(repo);
        if tokio::fs::try_exists(p.join("HEAD")).await? { Ok(p) } else { Err(StorageError::RepoNotFound(repo)) }
    }

    /// gitoxide handle, or None when gitoxide cannot open the repo (reftable,
    /// SHA-256): the caller then uses the cat-file backend.
    fn gix_repo(&self, repo: i64, path: &Path) -> Option<gix::Repository> {
        let mut cache = self.gix.lock().unwrap();
        if let Some(r) = cache.get(&repo) {
            return r.as_ref().map(|r| r.to_thread_local());
        }
        // gitoxide cannot read reftable or SHA-256 repos yet (crate-status, 2026-11-06)
        let config = std::fs::read_to_string(path.join("config")).unwrap_or_default().to_lowercase();
        let unsupported = config.contains("refstorage = reftable") || config.contains("objectformat = sha256");
        let opened = if unsupported {
            None
        } else {
            gix::open_opts(path, gix::open::Options::isolated().config_overrides(["core.useReplaceRefs=false"])).ok().map(|r| r.into_sync())
        };
        if opened.is_none() {
            tracing::debug!(repo, "gitoxide cannot open repo, using git processes");
        }
        let res = opened.as_ref().map(|r| r.to_thread_local());
        cache.put(repo, opened);
        res
    }

    fn forget(&self, repo: i64) {
        self.gix.lock().unwrap().pop(&repo);
        self.catfile.forget(&self.repo_path(repo));
    }

    async fn configure(&self, repo: i64) -> Result<()> {
        for (k, v) in [
            ("uploadpack.allowFilter", "true"),
            ("uploadpack.allowAnySHA1InWant", "true"),
            ("receive.advertisePushOptions", "true"),
            ("core.logAllRefUpdates", "false"),
            ("gc.auto", "0"),
            ("forgust.repoid", &repo.to_string()),
        ] {
            let mut c = self.git(repo);
            c.args(["config", k, v]);
            cli::run(c, 1024, SHORT).await?;
        }
        Ok(())
    }

    async fn tree_catfile(&self, path: &Path, commit: &str, dir: &str) -> Result<Vec<TreeEntry>> {
        let spec = if dir.is_empty() { format!("{commit}^{{tree}}") } else { format!("{commit}:{dir}") };
        let res = self
            .catfile
            .with(path, |cf| {
                let spec = spec.clone();
                Box::pin(async move { cf.contents(&spec, 64 << 20).await })
            })
            .await?;
        let Some((h, data)) = res else { return Err(StorageError::PathNotFound(dir.into())) };
        if h.kind != "tree" {
            return Err(StorageError::Invalid(format!("{dir} is not a directory")));
        }
        let mut entries: Vec<TreeEntry> = parse_raw_tree(&data)
            .into_iter()
            .map(|(mode, name, id)| TreeEntry { path: join_path(dir, &name), name, kind: kind_from_mode(mode), id })
            .collect();
        sort_entries(&mut entries);
        Ok(entries)
    }

    fn tree_gix(r: &gix::Repository, commit: &str, dir: &str) -> Result<Vec<TreeEntry>> {
        let id = r.rev_parse_single(format!("{commit}^{{tree}}").as_str()).map_err(|_| StorageError::RevisionNotFound(commit.into()))?;
        let root = r.find_object(id).map_err(|e| StorageError::Git(e.to_string()))?.into_tree();
        let tree = if dir.is_empty() {
            root
        } else {
            let entry = root
                .lookup_entry_by_path(dir)
                .map_err(|e| StorageError::Git(e.to_string()))?
                .ok_or_else(|| StorageError::PathNotFound(dir.into()))?;
            if !entry.mode().is_tree() {
                return Err(StorageError::Invalid(format!("{dir} is not a directory")));
            }
            entry.object().map_err(|e| StorageError::Git(e.to_string()))?.into_tree()
        };
        let decoded = tree.decode().map_err(|e| StorageError::Git(e.to_string()))?;
        let mut entries: Vec<TreeEntry> = decoded
            .entries
            .iter()
            .map(|e| {
                let name = e.filename.to_string();
                TreeEntry { path: join_path(dir, &name), name, kind: kind_from_mode(e.mode.value() as u32), id: e.oid.to_string() }
            })
            .collect();
        sort_entries(&mut entries);
        Ok(entries)
    }

    fn blob_gix(r: &gix::Repository, commit: &str, path: &str, max: usize) -> Result<Blob> {
        let id = r.rev_parse_single(format!("{commit}^{{tree}}").as_str()).map_err(|_| StorageError::RevisionNotFound(commit.into()))?;
        let root = r.find_object(id).map_err(|e| StorageError::Git(e.to_string()))?.into_tree();
        let entry = root
            .lookup_entry_by_path(path)
            .map_err(|e| StorageError::Git(e.to_string()))?
            .ok_or_else(|| StorageError::PathNotFound(path.into()))?;
        let kind = kind_from_mode(entry.mode().value() as u32);
        if matches!(kind, EntryKind::Tree | EntryKind::Submodule) {
            return Err(StorageError::Invalid(format!("{path} is not a file")));
        }
        let obj = entry.object().map_err(|e| StorageError::Git(e.to_string()))?;
        let size = obj.data.len() as u64;
        let truncated = obj.data.len() > max;
        let data = obj.data[..obj.data.len().min(max)].to_vec();
        Ok(Blob { id: entry.object_id().to_string(), size, kind, data, truncated })
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit_tree_dated(
        &self,
        repo: i64,
        tree: &str,
        parents: &[String],
        message: &str,
        author: &Identity,
        author_date: Option<&str>,
        committer: &Identity,
    ) -> Result<String> {
        self.ensure(repo).await?;
        if !is_valid_oid(tree) || parents.iter().any(|p| !is_valid_oid(p)) {
            return Err(StorageError::Invalid("bad object id".into()));
        }
        let mut c = self.git(repo);
        c.env("GIT_AUTHOR_NAME", &author.name)
            .env("GIT_AUTHOR_EMAIL", &author.email)
            .env("GIT_COMMITTER_NAME", &committer.name)
            .env("GIT_COMMITTER_EMAIL", &committer.email);
        if let Some(d) = author_date {
            c.env("GIT_AUTHOR_DATE", d);
        }
        c.args(["commit-tree", tree]);
        for p in parents {
            c.arg("-p").arg(p);
        }
        c.arg("-m").arg(message);
        let out = cli::run(c, 1024, SHORT).await?;
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !is_valid_oid(&oid) {
            return Err(StorageError::Git(format!("commit-tree returned {oid:?}")));
        }
        Ok(oid)
    }

    async fn commit_meta(&self, repo: i64, commit: &str) -> Result<CommitMeta> {
        let mut c = self.git(repo);
        c.args(["log", "-1", "--format=%an%x00%ae%x00%ad%x00%B", "--date=raw", commit, "--"]);
        let out = cli::run(c, 1 << 20, SHORT).await?;
        let text = String::from_utf8_lossy(&out.stdout);
        let f: Vec<&str> = text.splitn(4, '\0').collect();
        if f.len() < 4 {
            return Err(StorageError::RevisionNotFound(commit.into()));
        }
        Ok(CommitMeta {
            author: Identity { name: f[0].into(), email: f[1].into() },
            author_date: f[2].into(),
            message: f[3].trim_end().to_string(),
        })
    }

    async fn resolve_catfile(&self, path: &Path, rev: &str) -> Result<Option<String>> {
        let spec = format!("{rev}^{{commit}}");
        let h = self
            .catfile
            .with(path, |cf| {
                let spec = spec.clone();
                Box::pin(async move { cf.info(&spec).await })
            })
            .await?;
        Ok(h.map(|h| h.oid))
    }
}

#[async_trait]
impl Storage for LocalStorage {
    async fn create_repo(&self, repo: i64, opts: CreateRepoOptions) -> Result<()> {
        let path = self.repo_path(repo);
        if tokio::fs::try_exists(&path).await? {
            return Err(StorageError::RepoExists(repo));
        }
        tokio::fs::create_dir_all(path.parent().unwrap()).await?;
        let branch = if opts.default_branch.is_empty() { "main".to_string() } else { opts.default_branch };
        if !validate_rev(&branch) {
            return Err(StorageError::Invalid("bad branch name".into()));
        }
        let mut c = tokio::process::Command::new(&self.cfg.git_bin);
        c.args(["init", "--bare", "--quiet"])
            .arg(format!("--initial-branch={branch}"))
            .arg(format!("--ref-format={}", opts.ref_format.as_str()))
            .arg(&path)
            .env_remove("GIT_DIR")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", "/nonexistent")
            .kill_on_drop(true);
        cli::run(c, 4096, SHORT).await?;
        self.configure(repo).await?;
        self.forget(repo);
        Ok(())
    }

    async fn delete_repo(&self, repo: i64) -> Result<()> {
        let path = self.ensure(repo).await?;
        self.forget(repo);
        let trash = path.with_extension(format!("deleted-{}", std::process::id()));
        tokio::fs::rename(&path, &trash).await?;
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(trash)).await.map_err(|e| StorageError::Git(e.to_string()))??;
        Ok(())
    }

    async fn repo_exists(&self, repo: i64) -> Result<bool> {
        Ok(tokio::fs::try_exists(self.repo_path(repo).join("HEAD")).await?)
    }

    async fn set_default_branch(&self, repo: i64, branch: &str) -> Result<()> {
        self.ensure(repo).await?;
        if !validate_rev(branch) {
            return Err(StorageError::Invalid("bad branch name".into()));
        }
        let mut c = self.git(repo);
        c.args(["symbolic-ref", "HEAD"]).arg(format!("refs/heads/{branch}"));
        cli::run(c, 1024, SHORT).await?;
        Ok(())
    }

    async fn default_branch(&self, repo: i64) -> Result<Option<String>> {
        self.ensure(repo).await?;
        let mut c = self.git(repo);
        c.args(["symbolic-ref", "--short", "HEAD"]);
        match cli::run(c, 4096, SHORT).await {
            Ok(o) => Ok(Some(String::from_utf8_lossy(&o.stdout).trim().to_string())),
            Err(StorageError::Git(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn is_empty(&self, repo: i64) -> Result<bool> {
        self.ensure(repo).await?;
        let mut c = self.git(repo);
        c.args(["for-each-ref", "--count=1", "--format=%(refname)"]);
        Ok(cli::run(c, 4096, SHORT).await?.stdout.is_empty())
    }

    async fn spawn_service(&self, repo: i64, service: GitService, opts: ServiceOptions) -> Result<ServiceProcess> {
        let path = self.ensure(repo).await?;
        let mut c = cli::base(&self.cfg.git_bin, &path);
        if let Some(h) = &opts.hooks_path {
            c.arg("-c").arg(format!("core.hooksPath={}", h.display()));
        }
        // internal refs never reach clients (they would bloat every advertisement)
        c.args([
            "-c",
            "transfer.hideRefs=refs/keep-around",
            "-c",
            "transfer.hideRefs=refs/forgust",
            "-c",
            "transfer.hideRefs=refs/pipelines",
        ]);
        // CI jobs fetch their pipeline's commit by id (`+<sha>:refs/pipelines/<id>`),
        // the tip of a hidden ref: allow wants of hidden ref tips (cheap, unlike
        // allowReachableSHA1InWant, which walks history)
        c.args(["-c", "uploadpack.allowTipSHA1InWant=true"]);
        c.arg("-c").arg(format!("transfer.hideRefs={}", REBASE_PREFIX.trim_end_matches('/')));
        c.arg(service.subcommand());
        if opts.stateless_rpc {
            c.arg("--stateless-rpc");
        }
        if opts.advertise_refs {
            c.arg("--advertise-refs");
        }
        c.arg(&path);
        if let Some(p) = &opts.git_protocol {
            c.env("GIT_PROTOCOL", p);
        }
        for (k, v) in &opts.env {
            c.env(k, v);
        }
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let slot = match service {
            GitService::ReceivePack => None,
            _ => Some(cli::permit(SERVICE_WAIT).await?),
        };
        let mut child = c.spawn()?;
        let stdin = child.stdin.take().map(|s| Box::pin(s) as _);
        let stdout = Box::pin(child.stdout.take().expect("piped"));
        let stderr = child.stderr.take().map(|s| Box::pin(s) as _);
        Ok(ServiceProcess { stdin, stdout, stderr, child, slot })
    }

    async fn resolve(&self, repo: i64, rev: &str) -> Result<Option<String>> {
        let path = self.ensure(repo).await?;
        if !validate_rev(rev) {
            return Err(StorageError::Invalid(format!("bad revision {rev}")));
        }
        if let Some(r) = self.gix_repo(repo, &path) {
            let spec = format!("{rev}^{{commit}}");
            let rev = rev.to_string();
            return tokio::task::spawn_blocking(move || match r.rev_parse_single(spec.as_str()) {
                Ok(id) => Ok(Some(id.to_string())),
                Err(_) => {
                    let _ = rev;
                    Ok(None)
                }
            })
            .await
            .map_err(|e| StorageError::Git(e.to_string()))?;
        }
        self.resolve_catfile(&path, rev).await
    }

    async fn refs(&self, repo: i64, prefix: &str) -> Result<Vec<RefInfo>> {
        self.ensure(repo).await?;
        if !prefix.starts_with("refs/") || prefix.contains("..") {
            return Err(StorageError::Invalid("bad ref prefix".into()));
        }
        let mut c = self.git(repo);
        c.args([
            "for-each-ref",
            "--sort=-creatordate",
            "--format=%(refname)%00%(objectname)%00%(*objectname)%00%(creatordate:unix)%00%(*committerdate:unix)%00%(subject)%00%(*subject)%00%(authorname)%00%(*authorname)%00%(objecttype)",
            prefix,
        ]);
        let out = cli::run(c, 256 << 20, LONG).await?;
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\0').collect();
                if f.len() < 10 {
                    return None;
                }
                let peeled = (!f[2].is_empty()).then(|| f[2].to_string());
                let is_tag_obj = f[9] == "tag";
                let commit_id = peeled.clone().unwrap_or_else(|| f[1].to_string());
                let commit = (f[9] == "commit" || is_tag_obj).then(|| CommitSummary {
                    id: commit_id,
                    subject: if is_tag_obj { f[6] } else { f[5] }.to_string(),
                    author_name: if is_tag_obj { f[8] } else { f[7] }.to_string(),
                    committed_at: if is_tag_obj { f[4] } else { f[3] }.parse().unwrap_or(0),
                });
                Some(RefInfo { name: f[0].to_string(), target: f[1].to_string(), peeled, commit })
            })
            .collect())
    }

    async fn tree(&self, repo: i64, commit: &str, dir: &str) -> Result<Vec<TreeEntry>> {
        let path = self.ensure(repo).await?;
        if !validate_rev(commit) || !validate_path(dir) {
            return Err(StorageError::Invalid("bad revision or path".into()));
        }
        let dir = dir.trim_end_matches('/').to_string();
        if let Some(r) = self.gix_repo(repo, &path) {
            let commit = commit.to_string();
            return tokio::task::spawn_blocking(move || Self::tree_gix(&r, &commit, &dir))
                .await
                .map_err(|e| StorageError::Git(e.to_string()))?;
        }
        self.tree_catfile(&path, commit, &dir).await
    }

    async fn list_paths(&self, repo: i64, commit: &str, max: usize) -> Result<(Vec<String>, bool)> {
        self.ensure(repo).await?;
        if !validate_rev(commit) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        let mut c = self.git(repo);
        c.args(["ls-tree", "-r", "-z", "--name-only", commit]);
        let out = cli::run(c, 256 << 20, LONG).await?;
        let mut paths: Vec<String> =
            out.stdout.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
        let truncated = paths.len() > max || out.truncated;
        paths.truncate(max);
        Ok((paths, truncated))
    }

    async fn tree_sizes(&self, repo: i64, commit: &str, max: usize) -> Result<(Vec<(String, u64)>, bool)> {
        self.ensure(repo).await?;
        if !validate_rev(commit) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        let mut c = self.git(repo);
        c.args(["ls-tree", "-r", "-l", "-z", commit]);
        let out = cli::run(c, 512 << 20, LONG).await?;
        let mut files = Vec::new();
        // `<mode> SP <type> SP <oid> SP+ <size> TAB <path> NUL`
        for rec in out.stdout.split(|&b| b == 0).filter(|r| !r.is_empty()) {
            let Some(tab) = rec.iter().position(|&b| b == b'\t') else { continue };
            let meta = String::from_utf8_lossy(&rec[..tab]);
            let mut f = meta.split_whitespace();
            if f.nth(1) != Some("blob") {
                continue;
            }
            let Some(size) = f.nth(1).and_then(|s| s.parse::<u64>().ok()) else { continue };
            files.push((String::from_utf8_lossy(&rec[tab + 1..]).into_owned(), size));
        }
        let truncated = files.len() > max || out.truncated;
        files.truncate(max);
        Ok((files, truncated))
    }

    async fn disk_size(&self, repo: i64) -> Result<u64> {
        self.ensure(repo).await?;
        let mut c = self.git(repo);
        c.args(["count-objects", "-v"]);
        let out = cli::run(c, 1 << 16, LONG).await?;
        // `size` (loose) and `size-pack`, in KiB
        let kib: u64 = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("size: ").or_else(|| l.strip_prefix("size-pack: ")))
            .filter_map(|v| v.trim().parse::<u64>().ok())
            .sum();
        Ok(kib * 1024)
    }

    async fn blob(&self, repo: i64, commit: &str, file: &str, max_bytes: usize) -> Result<Blob> {
        let path = self.ensure(repo).await?;
        if !validate_rev(commit) || !validate_path(file) || file.is_empty() {
            return Err(StorageError::Invalid("bad revision or path".into()));
        }
        if let Some(r) = self.gix_repo(repo, &path) {
            let (commit, file) = (commit.to_string(), file.to_string());
            return tokio::task::spawn_blocking(move || Self::blob_gix(&r, &commit, &file, max_bytes))
                .await
                .map_err(|e| StorageError::Git(e.to_string()))?;
        }
        // cat-file backend: the entry mode comes from the parent tree listing
        let (parent, name) = match file.rsplit_once('/') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (String::new(), file.to_string()),
        };
        let entries = self.tree_catfile(&path, commit, &parent).await?;
        let entry = entries.into_iter().find(|e| e.name == name).ok_or_else(|| StorageError::PathNotFound(file.into()))?;
        if matches!(entry.kind, EntryKind::Tree | EntryKind::Submodule) {
            return Err(StorageError::Invalid(format!("{file} is not a file")));
        }
        let oid = entry.id.clone();
        let res = self
            .catfile
            .with(&path, |cf| {
                let oid = oid.clone();
                Box::pin(async move { cf.contents(&oid, max_bytes).await })
            })
            .await?;
        let (h, data) = res.ok_or_else(|| StorageError::PathNotFound(file.into()))?;
        Ok(Blob { id: h.oid, size: h.size, kind: entry.kind, truncated: (h.size as usize) > data.len(), data })
    }

    async fn commit(&self, repo: i64, rev: &str) -> Result<Option<CommitInfo>> {
        Ok(self.log(repo, rev, None, 0, 1).await.ok().and_then(|mut v| v.pop()))
    }

    async fn log(&self, repo: i64, rev: &str, path: Option<&str>, skip: usize, limit: usize) -> Result<Vec<CommitInfo>> {
        self.ensure(repo).await?;
        if !validate_rev(rev) || path.is_some_and(|p| !validate_path(p)) {
            return Err(StorageError::Invalid("bad revision or path".into()));
        }
        let mut c = self.git(repo);
        c.arg("log").arg(LOG_FORMAT).arg(format!("--skip={skip}")).arg(format!("--max-count={}", limit.min(1000))).arg(rev).arg("--");
        if let Some(p) = path.filter(|p| !p.is_empty()) {
            c.env("GIT_LITERAL_PATHSPECS", "1").arg(p);
        }
        let out = cli::run(c, 64 << 20, LONG).await.map_err(|e| match e {
            StorageError::Git(m) if m.contains("unknown revision") || m.contains("bad revision") => {
                StorageError::RevisionNotFound(rev.into())
            }
            other => other,
        })?;
        Ok(parse_log(&out.stdout))
    }

    async fn commit_count(&self, repo: i64, rev: &str) -> Result<u64> {
        self.ensure(repo).await?;
        if !validate_rev(rev) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        let mut c = self.git(repo);
        c.args(["rev-list", "--count", rev, "--"]);
        let out = cli::run(c, 1 << 10, LONG).await?;
        String::from_utf8_lossy(&out.stdout).trim().parse().map_err(|_| StorageError::Git("rev-list --count: not a number".into()))
    }

    async fn log_between(&self, repo: i64, from: &str, to: &str, limit: usize) -> Result<Vec<CommitInfo>> {
        self.ensure(repo).await?;
        if !validate_rev(from) || !validate_rev(to) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        let mut c = self.git(repo);
        c.arg("log").arg(LOG_FORMAT).arg(format!("--max-count={}", limit.min(1000))).arg(format!("{from}..{to}")).arg("--");
        let out = cli::run(c, 64 << 20, LONG).await?;
        Ok(parse_log(&out.stdout))
    }

    async fn diff(&self, repo: i64, from: Option<&str>, to: &str, opts: DiffOptions) -> Result<Diff> {
        self.ensure(repo).await?;
        let from = from.unwrap_or(EMPTY_TREE);
        if !validate_rev(from) || !validate_rev(to) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        // 1. the full list of changed files
        let mut c = self.git(repo);
        c.args(["diff", "--numstat", "-z", "-M", "--no-ext-diff", "--no-textconv", from, to]);
        let out = cli::run(c, 64 << 20, LONG).await?;
        let mut summary = parse_numstat_z(&out.stdout);
        if let Some(only) = &opts.paths {
            summary.retain(|f| only.contains(&f.new_path) || only.contains(&f.old_path));
        }
        let additions = summary.iter().filter_map(|f| f.additions).sum();
        let deletions = summary.iter().filter_map(|f| f.deletions).sum();
        let page: Vec<_> = summary.iter().skip(opts.offset).take(opts.limit.max(1)).collect();

        // 2. the patch for this page of files only
        let mut files = Vec::new();
        if !page.is_empty() {
            let mut c = self.git(repo);
            c.args([
                "diff",
                "-p",
                "-M",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                from,
                to,
                "--",
            ]);
            c.env("GIT_LITERAL_PATHSPECS", "1");
            for f in &page {
                c.arg(&f.new_path);
                if f.old_path != f.new_path {
                    c.arg(&f.old_path);
                }
            }
            let out = cli::run(c, 128 << 20, LONG).await?;
            let text = String::from_utf8_lossy(&out.stdout);
            let mut parsed = parse_unified_diff(&text, opts.max_lines_per_file);
            // keep the numstat order
            let mut by_path: HashMap<String, crate::DiffFile> =
                parsed.drain(..).map(|f| (format!("{}\0{}", f.old_path, f.new_path), f)).collect();
            for f in &page {
                let key = |o: &str, n: &str| format!("{o}\0{n}");
                let found = by_path
                    .remove(&key(&f.old_path, &f.new_path))
                    .or_else(|| by_path.remove(&key("", &f.new_path)))
                    .or_else(|| by_path.remove(&key(&f.old_path, "")));
                if let Some(mut df) = found {
                    if f.additions.is_none() {
                        df.binary = true;
                    }
                    files.push(df);
                }
            }
        }
        Ok(Diff { from: from.into(), to: to.into(), total_files: summary.len(), additions, deletions, offset: opts.offset, files, summary })
    }

    async fn blame(&self, repo: i64, commit: &str, file: &str) -> Result<Vec<BlameHunk>> {
        self.ensure(repo).await?;
        if !validate_rev(commit) || !validate_path(file) || file.is_empty() {
            return Err(StorageError::Invalid("bad revision or path".into()));
        }
        let mut c = self.git(repo);
        c.args(["blame", "--porcelain", commit, "--", file]);
        let out = cli::run(c, 256 << 20, LONG).await?;
        let text = String::from_utf8_lossy(&out.stdout);

        let mut commits: HashMap<String, CommitSummary> = HashMap::new();
        let mut hunks: Vec<BlameHunk> = Vec::new();
        let mut cur_sha = String::new();
        let mut cur_final = 0usize;
        let mut new_group = false;
        for line in text.lines() {
            if let Some(content) = line.strip_prefix('\t') {
                let summary = commits.get(&cur_sha).cloned().unwrap_or(CommitSummary {
                    id: cur_sha.clone(),
                    subject: String::new(),
                    author_name: String::new(),
                    committed_at: 0,
                });
                match hunks.last_mut() {
                    Some(h) if !new_group && h.commit.id == cur_sha => h.lines.push(content.to_string()),
                    _ => hunks.push(BlameHunk { commit: summary, start_line: cur_final, lines: vec![content.to_string()] }),
                }
                new_group = false;
                continue;
            }
            let mut parts = line.split(' ');
            let first = parts.next().unwrap_or("");
            if first.len() == 40 && first.bytes().all(|b| b.is_ascii_hexdigit()) {
                cur_sha = first.to_string();
                let _orig = parts.next();
                cur_final = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                // a 4th field marks the start of a new group in the porcelain output
                new_group = parts.next().is_some();
                commits.entry(cur_sha.clone()).or_insert_with(|| CommitSummary {
                    id: cur_sha.clone(),
                    subject: String::new(),
                    author_name: String::new(),
                    committed_at: 0,
                });
                continue;
            }
            let rest = line.split_once(' ').map(|x| x.1).unwrap_or("");
            if let Some(cs) = commits.get_mut(&cur_sha) {
                match first {
                    "author" => cs.author_name = rest.to_string(),
                    "author-time" => cs.committed_at = rest.parse().unwrap_or(0),
                    "summary" => cs.subject = rest.to_string(),
                    _ => {}
                }
            }
        }
        // fill commit info on hunks created before all headers were seen
        for h in &mut hunks {
            if let Some(cs) = commits.get(&h.commit.id) {
                h.commit = cs.clone();
            }
        }
        Ok(hunks)
    }

    async fn archive(&self, repo: i64, commit: &str, format: ArchiveFormat, prefix: &str) -> Result<ServiceProcess> {
        self.ensure(repo).await?;
        if !validate_rev(commit) || prefix.contains("..") || prefix.contains('/') {
            return Err(StorageError::Invalid("bad revision or prefix".into()));
        }
        let mut c = self.git(repo);
        let fmt = match format {
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Tar => "tar",
        };
        c.arg("archive").arg(format!("--format={fmt}")).arg(format!("--prefix={prefix}/")).arg(commit);
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let slot = Some(cli::permit(SERVICE_WAIT).await?);
        let mut child = c.spawn()?;
        let stdout = Box::pin(child.stdout.take().expect("piped"));
        let stderr = child.stderr.take().map(|s| Box::pin(s) as _);
        Ok(ServiceProcess { stdin: None, stdout, stderr, child, slot })
    }

    async fn is_ancestor(&self, repo: i64, ancestor: &str, descendant: &str) -> Result<bool> {
        self.ensure(repo).await?;
        if !validate_rev(ancestor) || !validate_rev(descendant) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        let mut c = self.git(repo);
        c.args(["merge-base", "--is-ancestor", ancestor, descendant]);
        cli::status(c, LONG).await
    }

    async fn merge_base(&self, repo: i64, a: &str, b: &str) -> Result<Option<String>> {
        self.ensure(repo).await?;
        if !validate_rev(a) || !validate_rev(b) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        let mut c = self.git(repo);
        c.args(["merge-base", a, b]);
        match cli::run(c, 1024, LONG).await {
            Ok(o) => Ok(Some(String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())),
            Err(StorageError::Git(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn maintenance(&self, repo: i64, full: bool) -> Result<()> {
        self.ensure(repo).await?;
        if self.is_empty(repo).await? {
            return Ok(());
        }
        let format = self.ref_format(repo).await?;
        if format == RefFormat::Files {
            let mut c = self.git(repo);
            c.args(["pack-refs", "--all", "--prune"]);
            cli::run(c, 4096, LONG).await?;
        }
        let mut c = self.git(repo);
        if full {
            c.args(["repack", "-a", "-d", "-b", "--write-midx", "--quiet"]);
        } else {
            c.args(["repack", "-d", "-l", "--write-midx", "--quiet"]);
        }
        cli::run(c, 4096, Duration::from_secs(3600)).await?;
        let mut c = self.git(repo);
        c.args(["commit-graph", "write", "--reachable", "--changed-paths", "--split", "--no-progress"]);
        cli::run(c, 4096, Duration::from_secs(3600)).await?;
        self.forget(repo);
        Ok(())
    }

    async fn bundle_create(&self, repo: i64, dest: &Path) -> Result<bool> {
        self.ensure(repo).await?;
        if self.is_empty(repo).await? {
            return Ok(false);
        }
        let mut c = self.git(repo);
        c.args(["bundle", "create", "--quiet"]).arg(dest).arg("--all");
        cli::run(c, 4096, Duration::from_secs(6 * 3600)).await?;
        Ok(true)
    }

    async fn restore_from_bundle(&self, repo: i64, bundle: Option<&Path>, opts: CreateRepoOptions) -> Result<()> {
        let path = self.repo_path(repo);
        if tokio::fs::try_exists(&path).await? {
            return Err(StorageError::RepoExists(repo));
        }
        let Some(bundle) = bundle else { return self.create_repo(repo, opts).await };
        tokio::fs::create_dir_all(path.parent().unwrap()).await?;
        let mut c = tokio::process::Command::new(&self.cfg.git_bin);
        c.args(["clone", "--mirror", "--quiet"])
            .arg(format!("--ref-format={}", opts.ref_format.as_str()))
            .arg(bundle)
            .arg(&path)
            .env_remove("GIT_DIR")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", "/nonexistent")
            .kill_on_drop(true);
        cli::run(c, 4096, Duration::from_secs(6 * 3600)).await?;
        let mut c = self.git(repo);
        c.args(["remote", "remove", "origin"]);
        cli::run(c, 4096, SHORT).await?;
        self.configure(repo).await?;
        if !opts.default_branch.is_empty() {
            self.set_default_branch(repo, &opts.default_branch).await?;
        }
        self.forget(repo);
        Ok(())
    }

    async fn ref_format(&self, repo: i64) -> Result<RefFormat> {
        self.ensure(repo).await?;
        let mut c = self.git(repo);
        c.args(["rev-parse", "--show-ref-format"]);
        let out = cli::run(c, 1024, SHORT).await?;
        Ok(RefFormat::parse(String::from_utf8_lossy(&out.stdout).trim()).unwrap_or_default())
    }

    async fn update_ref(&self, repo: i64, name: &str, new: &str, expected_old: Option<&str>) -> Result<()> {
        self.ensure(repo).await?;
        check_ref_name(name)?;
        if !is_valid_oid(new) || expected_old.is_some_and(|o| !is_valid_oid(o)) {
            return Err(StorageError::Invalid("bad object id".into()));
        }
        let mut c = self.git(repo);
        c.args(["update-ref", "-m", "forgust", name, new]);
        if let Some(old) = expected_old {
            c.arg(old);
        }
        let (code, _, err) = cli::run_raw(c, 4096, SHORT).await?;
        match code {
            0 => Ok(()),
            _ if expected_old.is_some()
                && (err.contains("but expected") || err.contains("cannot lock ref") || err.contains("reference already exists")) =>
            {
                Err(StorageError::RefChanged(name.into()))
            }
            _ => Err(StorageError::Git(err)),
        }
    }

    async fn update_refs(&self, repo: i64, updates: &[(String, String)]) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        if updates.is_empty() {
            return Ok(());
        }
        self.ensure(repo).await?;
        // one update per ref (git refuses duplicates in a transaction); the last wins
        let unique: std::collections::BTreeMap<&str, &str> = updates.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        let mut input = String::new();
        for (name, new) in unique {
            check_ref_name(name)?;
            if !is_valid_oid(new) {
                return Err(StorageError::Invalid("bad object id".into()));
            }
            input.push_str(&format!("update {name} {new}\n"));
        }
        let mut c = self.git(repo);
        c.args(["update-ref", "-m", "forgust", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = c.spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| StorageError::Git("no stdin".into()))?;
        stdin.write_all(input.as_bytes()).await?;
        drop(stdin);
        let out =
            tokio::time::timeout(LONG, child.wait_with_output()).await.map_err(|_| StorageError::Git("update-ref timed out".into()))??;
        if !out.status.success() {
            return Err(StorageError::Git(String::from_utf8_lossy(&out.stderr).into_owned()));
        }
        Ok(())
    }

    async fn delete_ref(&self, repo: i64, name: &str, expected_old: Option<&str>) -> Result<()> {
        self.ensure(repo).await?;
        check_ref_name(name)?;
        let mut c = self.git(repo);
        c.args(["update-ref", "-d", name]);
        if let Some(old) = expected_old {
            c.arg(old);
        }
        let (code, _, err) = cli::run_raw(c, 4096, SHORT).await?;
        if code == 0 {
            Ok(())
        } else if expected_old.is_some() {
            Err(StorageError::RefChanged(name.into()))
        } else {
            Err(StorageError::Git(err))
        }
    }

    async fn merge_tree(&self, repo: i64, ours: &str, theirs: &str, merge_base: Option<&str>) -> Result<MergeTreeResult> {
        self.ensure(repo).await?;
        if !validate_rev(ours) || !validate_rev(theirs) || merge_base.is_some_and(|b| !validate_rev(b)) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        let mut c = self.git(repo);
        c.args(["merge-tree", "--write-tree", "--name-only", "--no-messages"]);
        if let Some(b) = merge_base {
            c.arg(format!("--merge-base={b}"));
        }
        c.args([ours, theirs]);
        let (code, out, err) = cli::run_raw(c, 16 << 20, LONG).await?;
        let text = String::from_utf8_lossy(&out);
        let mut lines = text.lines();
        let tree = lines.next().unwrap_or("").trim().to_string();
        match code {
            0 if is_valid_oid(&tree) => Ok(MergeTreeResult { tree, conflicts: vec![] }),
            1 if is_valid_oid(&tree) => {
                let mut conflicts: Vec<String> = lines.take_while(|l| !l.is_empty()).map(str::to_string).collect();
                conflicts.dedup();
                Ok(MergeTreeResult { tree, conflicts })
            }
            _ => Err(StorageError::Git(if err.is_empty() { format!("merge-tree exited {code}") } else { err })),
        }
    }

    async fn tree_with_changes(&self, repo: i64, base: &str, changes: &[FileChange]) -> Result<String> {
        use tokio::io::AsyncWriteExt;
        self.ensure(repo).await?;
        if changes.is_empty() || changes.len() > 1000 {
            return Err(StorageError::Invalid("1-1000 changes are needed".into()));
        }
        for ch in changes {
            if !validate_path(&ch.path) {
                return Err(StorageError::Invalid(format!("bad path {}", ch.path)));
            }
        }
        let base_oid =
            self.resolve(repo, &format!("{base}^{{commit}}")).await?.ok_or_else(|| StorageError::RevisionNotFound(base.into()))?;
        // a private index file: concurrent edits never share one
        let index = self.repo_path(repo).join(format!(
            "forgust-index-{}-{}",
            std::process::id(),
            REBASE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // update-index wants a work tree even with --cacheinfo (it never reads
        // or writes it then): an empty private directory
        let work = index.with_extension("wt");
        let result = async {
            tokio::fs::create_dir_all(&work).await?;
            let mut c = self.git(repo);
            c.env("GIT_INDEX_FILE", &index).args(["read-tree", &base_oid]);
            cli::run(c, 4096, LONG).await?;
            // every path's current mode in one listing
            let mut ls = self.git(repo);
            ls.args(["ls-tree", "-z", &base_oid, "--"]).args(changes.iter().map(|c| c.path.as_str()));
            let out = cli::run(ls, 16 << 20, LONG).await?;
            let modes: std::collections::HashMap<String, String> = String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter_map(|line| {
                    let (meta, path) = line.split_once('\t')?;
                    Some((path.to_string(), meta.split_whitespace().next()?.to_string()))
                })
                .collect();
            // then one `update-index --index-info` for all of them
            let mut info: Vec<u8> = Vec::new();
            for ch in changes {
                let existing = modes.get(&ch.path);
                if existing.is_some_and(|m| !matches!(m.as_str(), "100644" | "100755")) {
                    return Err(StorageError::Invalid(format!("{} is not a regular file", ch.path)));
                }
                match &ch.content {
                    Some(bytes) => {
                        let mut h = self.git(repo);
                        h.args(["hash-object", "-w", "--stdin"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
                        let mut child = h.spawn()?;
                        let mut stdin = child.stdin.take().ok_or_else(|| StorageError::Git("no stdin".into()))?;
                        stdin.write_all(bytes).await?;
                        drop(stdin);
                        let out = tokio::time::timeout(LONG, child.wait_with_output())
                            .await
                            .map_err(|_| StorageError::Git("hash-object timed out".into()))??;
                        let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if !out.status.success() || !is_valid_oid(&oid) {
                            return Err(StorageError::Git(String::from_utf8_lossy(&out.stderr).into_owned()));
                        }
                        let mode = existing.map_or("100644", |m| m.as_str());
                        info.extend_from_slice(format!("{mode} {oid}\t{}\0", ch.path).as_bytes());
                    }
                    // mode 0: the entry goes
                    None => info.extend_from_slice(format!("0 {}\t{}\0", "0".repeat(base_oid.len()), ch.path).as_bytes()),
                }
            }
            let mut u = self.git(repo);
            u.env("GIT_INDEX_FILE", &index).env("GIT_WORK_TREE", &work).args(["update-index", "-z", "--index-info"]);
            cli::run_input(u, info, 4096, LONG).await?;
            let mut w = self.git(repo);
            w.env("GIT_INDEX_FILE", &index).arg("write-tree");
            let out = cli::run(w, 1024, LONG).await?;
            let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !is_valid_oid(&tree) {
                return Err(StorageError::Git(format!("write-tree returned {tree:?}")));
            }
            Ok(tree)
        }
        .await;
        let _ = tokio::fs::remove_file(&index).await;
        let _ = tokio::fs::remove_dir_all(&work).await;
        result
    }

    async fn commit_tree(
        &self,
        repo: i64,
        tree: &str,
        parents: &[String],
        message: &str,
        author: &Identity,
        committer: &Identity,
    ) -> Result<String> {
        self.commit_tree_dated(repo, tree, parents, message, author, None, committer).await
    }

    async fn tag_object(&self, repo: i64, name: &str, target: &str, message: &str, tagger: &Identity) -> Result<String> {
        use tokio::io::AsyncWriteExt;
        self.ensure(repo).await?;
        check_ref_name(&format!("refs/tags/{name}"))?;
        if !is_valid_oid(target) {
            return Err(StorageError::Invalid("bad object id".into()));
        }
        let clean = |s: &str| s.chars().filter(|c| !matches!(c, '<' | '>' | '\n' | '\0')).collect::<String>();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut body = format!(
            "object {target}\ntype commit\ntag {name}\ntagger {} <{}> {now} +0000\n\n{}",
            clean(&tagger.name).trim(),
            clean(&tagger.email).trim(),
            message.replace('\0', "")
        );
        if !body.ends_with('\n') {
            body.push('\n');
        }
        // mktag checks the object (its target exists and is a commit) before writing it
        let mut c = self.git(repo);
        c.args(["mktag"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = c.spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| StorageError::Git("no stdin".into()))?;
        stdin.write_all(body.as_bytes()).await?;
        drop(stdin);
        let out =
            tokio::time::timeout(SHORT, child.wait_with_output()).await.map_err(|_| StorageError::Git("mktag timed out".into()))??;
        if !out.status.success() {
            return Err(StorageError::Git(String::from_utf8_lossy(&out.stderr).into_owned()));
        }
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !is_valid_oid(&oid) {
            return Err(StorageError::Git(format!("mktag returned {oid:?}")));
        }
        Ok(oid)
    }

    async fn rev_list(&self, repo: i64, from: &str, to: &str, merges_only: bool) -> Result<Vec<String>> {
        self.ensure(repo).await?;
        if !validate_rev(from) || !validate_rev(to) {
            return Err(StorageError::Invalid("bad revision".into()));
        }
        let mut c = self.git(repo);
        c.args(["rev-list", "--reverse", "--topo-order"]);
        if merges_only {
            c.arg("--merges");
        }
        c.arg(format!("{from}..{to}")).arg("--");
        let out = cli::run(c, 64 << 20, LONG).await?;
        Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
    }

    async fn fetch(&self, repo: i64, url: &str, refspecs: &[String], config: &[(String, String)]) -> Result<()> {
        self.ensure(repo).await?;
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(StorageError::Invalid("only http(s) remotes can be fetched".into()));
        }
        if refspecs.iter().any(|r| r.starts_with('-') || r.contains("..")) {
            return Err(StorageError::Invalid("bad refspec".into()));
        }
        let mut c = self.git(repo);
        // no file://, ext:: or ssh: an import URL must not reach local files or run commands
        // and no redirects: the caller pins the checked host, a redirect would leave it for any address
        c.args(["-c", "protocol.allow=never", "-c", "protocol.http.allow=always", "-c", "protocol.https.allow=always"])
            .args(["-c", "credential.helper=", "-c", "http.followRedirects=false"])
            .args(["fetch", "--quiet", "--no-write-fetch-head", "--no-auto-gc", "--"])
            .arg(url)
            .args(refspecs)
            .env("GIT_CONFIG_COUNT", config.len().to_string());
        for (i, (k, v)) in config.iter().enumerate() {
            c.env(format!("GIT_CONFIG_KEY_{i}"), k).env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
        let r = cli::run(c, 64 * 1024, Duration::from_secs(6 * 3600)).await;
        self.forget(repo);
        r.map(|_| ())
    }

    async fn small_blobs(&self, repo: i64, max_size: u64) -> Result<Vec<String>> {
        self.ensure(repo).await?;
        let mut c = self.git(repo);
        c.args(["cat-file", "--batch-all-objects", "--batch-check=%(objecttype) %(objectname) %(objectsize)"]);
        let out = cli::run(c, 512 * 1024 * 1024, LONG).await?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut it = l.split(' ');
                match (it.next(), it.next(), it.next().and_then(|s| s.parse::<u64>().ok())) {
                    (Some("blob"), Some(oid), Some(size)) if size <= max_size => Some(oid.to_string()),
                    _ => None,
                }
            })
            .collect())
    }

    async fn blob_by_id(&self, repo: i64, oid: &str, max_bytes: usize) -> Result<Vec<u8>> {
        self.ensure(repo).await?;
        if !is_valid_oid(oid) {
            return Err(StorageError::Invalid("bad object id".into()));
        }
        let mut c = self.git(repo);
        c.args(["cat-file", "blob", oid]);
        Ok(cli::run(c, max_bytes, SHORT).await?.stdout)
    }

    async fn new_commits(&self, repo: i64, quarantine: Option<&Path>, tips: &[String], limit: usize) -> Result<Vec<String>> {
        if tips.is_empty() {
            return Ok(Vec::new());
        }
        if tips.iter().any(|t| !is_valid_oid(t)) {
            return Err(StorageError::Invalid("bad object id".into()));
        }
        let mut c = self.git_q(repo, quarantine).await?;
        c.args(["rev-list", &format!("--max-count={limit}")]).args(tips).args(["--not", "--all"]);
        let out = cli::run(c, 64 * 1024 * 1024, LONG).await?;
        Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
    }

    async fn raw_commits(&self, repo: i64, quarantine: Option<&Path>, shas: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
        if shas.is_empty() {
            return Ok(Vec::new());
        }
        if shas.iter().any(|t| !is_valid_oid(t)) {
            return Err(StorageError::Invalid("bad object id".into()));
        }
        let mut c = self.git_q(repo, quarantine).await?;
        c.args(["cat-file", "--batch"]);
        let input = shas.iter().map(|s| format!("{s}\n")).collect::<String>().into_bytes();
        let out = cli::run_input(c, input, 512 * 1024 * 1024, LONG).await?.stdout;
        // "<oid> <type> <size>\n<content>\n" per object ("<oid> missing\n" when absent)
        let mut res = Vec::new();
        let mut at = 0;
        while at < out.len() {
            let nl = out[at..]
                .iter()
                .position(|b| *b == b'\n')
                .map(|i| at + i)
                .ok_or_else(|| StorageError::Git("truncated cat-file output".into()))?;
            let header = String::from_utf8_lossy(&out[at..nl]).to_string();
            let mut it = header.split(' ');
            let (oid, kind, size) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().and_then(|s| s.parse::<usize>().ok()));
            at = nl + 1;
            let Some(size) = size else { continue };
            let body = out.get(at..at + size).ok_or_else(|| StorageError::Git("truncated cat-file output".into()))?.to_vec();
            at += size + 1;
            if kind == "commit" {
                res.push((oid.to_string(), body));
            }
        }
        Ok(res)
    }

    async fn new_blobs(&self, repo: i64, quarantine: Option<&Path>, tips: &[String], limit: usize) -> Result<Vec<(String, u64)>> {
        if tips.is_empty() {
            return Ok(Vec::new());
        }
        if tips.iter().any(|t| !is_valid_oid(t)) {
            return Err(StorageError::Invalid("bad object id".into()));
        }
        let mut c = self.git_q(repo, quarantine).await?;
        c.args(["rev-list", "--objects"]).args(tips).args(["--not", "--all"]);
        let listed = cli::run(c, 256 * 1024 * 1024, LONG).await?.stdout;
        let mut paths = std::collections::HashMap::new();
        let mut input = String::new();
        for line in String::from_utf8_lossy(&listed).lines() {
            let (oid, path) = line.split_once(' ').unwrap_or((line, ""));
            if path.is_empty() {
                continue;
            }
            if paths.insert(oid.to_string(), path.to_string()).is_none() {
                input.push_str(oid);
                input.push('\n');
            }
            if paths.len() >= limit {
                break;
            }
        }
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut c = self.git_q(repo, quarantine).await?;
        c.args(["cat-file", "--batch-check=%(objecttype) %(objectname) %(objectsize)"]);
        let out = cli::run_input(c, input.into_bytes(), 64 * 1024 * 1024, LONG).await?.stdout;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|l| {
                let mut it = l.split(' ');
                match (it.next(), it.next(), it.next().and_then(|s| s.parse::<u64>().ok())) {
                    (Some("blob"), Some(oid), Some(size)) => paths.get(oid).map(|p| (p.clone(), size)),
                    _ => None,
                }
            })
            .collect())
    }

    async fn changed_paths(&self, repo: i64, quarantine: Option<&Path>, old: &str, new: &str) -> Result<Vec<String>> {
        if !is_valid_oid(old) || !is_valid_oid(new) {
            return Err(StorageError::Invalid("bad object id".into()));
        }
        let mut c = self.git_q(repo, quarantine).await?;
        c.args(["diff", "--no-renames", "--name-only", "-z", old, new]);
        let out = cli::run(c, 64 * 1024 * 1024, LONG).await?.stdout;
        Ok(out.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect())
    }

    async fn rebase(&self, repo: i64, onto: &str, upstream: &str, tip: &str, committer: &Identity) -> Result<String> {
        self.ensure(repo).await?;
        if !self.rev_list(repo, upstream, tip, true).await?.is_empty() {
            return Err(StorageError::Invalid("the commits to rebase include merge commits; merge instead of rebasing".into()));
        }
        let commits = self.rev_list(repo, upstream, tip, false).await?;
        if commits.is_empty() {
            return self.resolve(repo, onto).await?.ok_or_else(|| StorageError::RevisionNotFound(onto.into()));
        }
        // fast path: `git replay` (experimental in git; we pin the version). It needs
        // a branch (refs/heads/) as the tip and prints the update instead of applying
        // it; the temporary branch is hidden from clients and refused on push.
        let tmp = format!(
            "{REBASE_PREFIX}{}-{}",
            commits.last().unwrap(),
            std::process::id() as u64 * 1_000_000 + REBASE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let tip_oid = self.resolve(repo, tip).await?.ok_or_else(|| StorageError::RevisionNotFound(tip.into()))?;
        self.update_ref(repo, &tmp, &tip_oid, None).await?;
        let mut c = self.git(repo);
        c.env("GIT_COMMITTER_NAME", &committer.name).env("GIT_COMMITTER_EMAIL", &committer.email);
        c.args(["replay", "--onto", onto]).arg(format!("{upstream}..{tmp}"));
        let replayed = cli::run_raw(c, 1 << 20, LONG).await;
        // git < 2.52 prints `update <ref> <new> <old>`; newer git moves the
        // ref itself and prints nothing (`--ref-action=update` by default)
        let mut new_tip = None;
        if let Ok((0, out, _)) = &replayed {
            let text = String::from_utf8_lossy(out);
            new_tip = text
                .lines()
                .find_map(|l| l.strip_prefix("update ").and_then(|r| r.split_whitespace().nth(1)))
                .filter(|o| is_valid_oid(o))
                .map(str::to_string);
            if new_tip.is_none() {
                new_tip = self.resolve(repo, &tmp).await.ok().flatten().filter(|o| *o != tip_oid);
            }
        }
        let _ = self.delete_ref(repo, &tmp, None).await;
        if let Some(new) = new_tip {
            tracing::debug!(repo, "rebased with git replay");
            REPLAY_REBASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(new);
        }
        // on conflicts (or no replay), the per-commit path below reports the paths
        tracing::debug!(repo, "git replay unavailable or failed; rebasing commit by commit");
        let mut current = self.resolve(repo, onto).await?.ok_or_else(|| StorageError::RevisionNotFound(onto.into()))?;
        for commit in commits {
            let meta = self.commit_meta(repo, &commit).await?;
            let parent = format!("{commit}^");
            let merged = self.merge_tree(repo, &current, &commit, Some(&parent)).await?;
            if !merged.clean() {
                return Err(StorageError::Conflict(merged.conflicts));
            }
            current = self
                .commit_tree_dated(repo, &merged.tree, &[current], &meta.message, &meta.author, Some(&meta.author_date), committer)
                .await?;
        }
        Ok(current)
    }

    fn reap_idle(&self) {
        self.catfile.reap_idle();
    }

    fn catfile_processes(&self) -> usize {
        self.catfile.len()
    }
}
