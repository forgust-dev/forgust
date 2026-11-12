//! Pool of long-lived `git cat-file --batch-command` processes, one per repo
//! (ADR-0002). Bounded by an LRU with an idle timeout so a host with many repos
//! never runs a process per repo.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lru::LruCache;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub struct ObjectHeader {
    pub oid: String,
    pub kind: String,
    pub size: u64,
}

pub struct CatFile {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    last_used: Instant,
}

impl CatFile {
    async fn spawn(git: &Path, repo: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(git)
            .arg("--git-dir")
            .arg(repo)
            .args(["cat-file", "--batch-command"])
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        Ok(CatFile { _child: child, stdin, stdout, last_used: Instant::now() })
    }

    async fn header(&mut self, spec: &str) -> std::io::Result<Option<ObjectHeader>> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line).await? == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "cat-file exited"));
        }
        let line = line.trim_end();
        if line.ends_with(" missing") || line.ends_with(" ambiguous") {
            return Ok(None);
        }
        let mut parts = line.splitn(3, ' ');
        let (Some(oid), Some(kind), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(std::io::Error::other(format!("bad cat-file header for {spec}: {line}")));
        };
        let size = size.parse().map_err(|_| std::io::Error::other("bad size"))?;
        Ok(Some(ObjectHeader { oid: oid.to_string(), kind: kind.to_string(), size }))
    }

    pub async fn info(&mut self, spec: &str) -> std::io::Result<Option<ObjectHeader>> {
        self.last_used = Instant::now();
        self.stdin.write_all(format!("info {spec}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        self.header(spec).await
    }

    /// Read an object; at most `max` bytes are kept, the rest is drained.
    pub async fn contents(&mut self, spec: &str, max: usize) -> std::io::Result<Option<(ObjectHeader, Vec<u8>)>> {
        self.last_used = Instant::now();
        self.stdin.write_all(format!("contents {spec}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        let Some(h) = self.header(spec).await? else { return Ok(None) };
        let keep = (h.size as usize).min(max);
        let mut data = vec![0u8; keep];
        self.stdout.read_exact(&mut data).await?;
        let mut rest = h.size - keep as u64;
        let mut sink = [0u8; 64 * 1024];
        while rest > 0 {
            let n = (rest as usize).min(sink.len());
            self.stdout.read_exact(&mut sink[..n]).await?;
            rest -= n as u64;
        }
        let mut nl = [0u8; 1];
        self.stdout.read_exact(&mut nl).await?;
        Ok(Some((h, data)))
    }
}

pub struct CatFilePool {
    git: PathBuf,
    idle: Duration,
    procs: Mutex<LruCache<PathBuf, Arc<tokio::sync::Mutex<Option<CatFile>>>>>,
}

impl CatFilePool {
    pub fn new(git: PathBuf, capacity: usize, idle: Duration) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap();
        CatFilePool { git, idle, procs: Mutex::new(LruCache::new(cap)) }
    }

    fn slot(&self, repo: &Path) -> Arc<tokio::sync::Mutex<Option<CatFile>>> {
        let mut procs = self.procs.lock().unwrap();
        if let Some(s) = procs.get(repo) {
            return s.clone();
        }
        let s = Arc::new(tokio::sync::Mutex::new(None));
        procs.push(repo.to_path_buf(), s.clone());
        s
    }

    pub fn forget(&self, repo: &Path) {
        self.procs.lock().unwrap().pop(repo);
    }

    /// Drop processes that have been idle longer than the idle timeout.
    pub fn reap_idle(&self) {
        let mut procs = self.procs.lock().unwrap();
        let idle: Vec<PathBuf> = procs
            .iter()
            .filter_map(|(k, v)| match v.try_lock() {
                Ok(g) => match g.as_ref() {
                    Some(cf) if cf.last_used.elapsed() > self.idle => Some(k.clone()),
                    None => Some(k.clone()),
                    _ => None,
                },
                Err(_) => None,
            })
            .collect();
        for k in idle {
            procs.pop(&k);
        }
    }

    pub fn len(&self) -> usize {
        self.procs.lock().unwrap().len()
    }

    /// Run `f` against the repo's process, spawning (or respawning once) as needed.
    pub async fn with<T>(
        &self,
        repo: &Path,
        f: impl for<'a> Fn(&'a mut CatFile) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<T>> + Send + 'a>>,
    ) -> std::io::Result<T> {
        let slot = self.slot(repo);
        let mut guard = slot.lock().await;
        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(CatFile::spawn(&self.git, repo).await?);
            }
            match f(guard.as_mut().unwrap()).await {
                Ok(v) => return Ok(v),
                Err(e) if attempt == 0 => {
                    tracing::debug!(error = %e, "cat-file process failed, respawning");
                    *guard = None;
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }
}

/// Parse a raw (binary) tree object into (mode, name, oid hex).
pub fn parse_raw_tree(data: &[u8]) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let Some(sp) = data[i..].iter().position(|&b| b == b' ') else { break };
        let mode = std::str::from_utf8(&data[i..i + sp]).ok().and_then(|m| u32::from_str_radix(m, 8).ok()).unwrap_or(0);
        i += sp + 1;
        let Some(nul) = data[i..].iter().position(|&b| b == 0) else { break };
        let name = String::from_utf8_lossy(&data[i..i + nul]).into_owned();
        i += nul + 1;
        if i + 20 > data.len() {
            break;
        }
        let oid = hex::encode(&data[i..i + 20]);
        i += 20;
        out.push((mode, name, oid));
    }
    out
}
