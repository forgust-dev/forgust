//! Running one-shot git commands against a bare repo.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::StorageError;

pub fn base(git: &Path, repo: &Path) -> Command {
    let mut cmd = Command::new(git);
    cmd.arg("--git-dir")
        .arg(repo)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=false"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_QUARANTINE_PATH")
        .env("GIT_TERMINAL_PROMPT", "0")
        // refs/replace/* must not change what Forgust reads (a pushed one could swap the CI config of a protected branch)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", "/nonexistent")
        .kill_on_drop(true);
    cmd
}

static LIMIT: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// Git processes this server runs at once (one-shot commands, clones and
/// fetches, archives): more wait, so a burst of expensive reads cannot exhaust
/// the machine. Set once, by the first storage built in the process.
pub fn set_limit(n: usize) {
    let _ = LIMIT.set(Arc::new(Semaphore::new(n.max(1))));
}

fn limit() -> Arc<Semaphore> {
    LIMIT.get_or_init(|| Arc::new(Semaphore::new(default_limit()))).clone()
}

pub fn default_limit() -> usize {
    (std::thread::available_parallelism().map_or(4, |n| n.get()) * 4).max(16)
}

/// A slot for one git process, waiting at most `wait` for it.
pub async fn permit(wait: Duration) -> Result<OwnedSemaphorePermit, StorageError> {
    match tokio::time::timeout(wait, limit().acquire_owned()).await {
        Ok(Ok(p)) => Ok(p),
        _ => Err(StorageError::Busy),
    }
}

#[allow(dead_code)]
pub struct Output {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub truncated: bool,
}

/// Run a command, capturing at most `cap` bytes of stdout (the process is killed
/// beyond that), with a timeout.
pub async fn run(mut cmd: Command, cap: usize, timeout: Duration) -> Result<Output, StorageError> {
    let _slot = permit(timeout).await?;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let fut = async {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        let mut truncated = false;
        loop {
            let n = stdout.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            if buf.len() + n > cap {
                let take = cap - buf.len();
                buf.extend_from_slice(&chunk[..take]);
                truncated = true;
                let _ = child.start_kill();
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let mut err = Vec::new();
        let _ = stderr.read_to_end(&mut err).await;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, buf, truncated, err))
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok((status, stdout, truncated, err))) => {
            if !status.success() && !truncated {
                let msg = String::from_utf8_lossy(&err).trim().to_string();
                return Err(StorageError::Git(if msg.is_empty() { format!("exit {status}") } else { msg }));
            }
            Ok(Output { status, stdout, truncated })
        }
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(StorageError::Git("timeout".into())),
    }
}

/// Run a command fed `input` on stdin (written while stdout is read, so a
/// large exchange cannot deadlock), capturing at most `cap` bytes of stdout.
pub async fn run_input(mut cmd: Command, input: Vec<u8>, cap: usize, timeout: Duration) -> Result<Output, StorageError> {
    use tokio::io::AsyncWriteExt;
    let _slot = permit(timeout).await?;
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take().expect("piped");
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
        let _ = stdin.shutdown().await;
    });
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let fut = async {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        let mut truncated = false;
        loop {
            let n = stdout.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            if buf.len() + n > cap {
                truncated = true;
                let _ = child.start_kill();
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let mut err = Vec::new();
        let _ = stderr.read_to_end(&mut err).await;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, buf, truncated, err))
    };
    let result = tokio::time::timeout(timeout, fut).await;
    writer.abort();
    match result {
        Ok(Ok((status, stdout, truncated, err))) => {
            if truncated {
                return Err(StorageError::Invalid("too much output".into()));
            }
            if !status.success() {
                let msg = String::from_utf8_lossy(&err).trim().to_string();
                return Err(StorageError::Git(if msg.is_empty() { format!("exit {status}") } else { msg }));
            }
            Ok(Output { status, stdout, truncated })
        }
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(StorageError::Git("timeout".into())),
    }
}

/// Run a command and only report whether it exited 0 (stderr ignored).
pub async fn status(mut cmd: Command, timeout: Duration) -> Result<bool, StorageError> {
    let _slot = permit(timeout).await?;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match tokio::time::timeout(timeout, cmd.status()).await {
        Ok(s) => Ok(s?.success()),
        Err(_) => Err(StorageError::Git("timeout".into())),
    }
}

/// Run a command and return its output and exit code without treating a non-zero
/// exit as an error (for commands like `merge-tree` whose exit code is data).
pub async fn run_raw(mut cmd: Command, cap: usize, timeout: Duration) -> Result<(i32, Vec<u8>, String), StorageError> {
    let _slot = permit(timeout).await?;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let fut = async {
        let child = cmd.spawn()?;
        child.wait_with_output().await
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(out)) => {
            let mut stdout = out.stdout;
            stdout.truncate(cap);
            Ok((out.status.code().unwrap_or(-1), stdout, String::from_utf8_lossy(&out.stderr).trim().to_string()))
        }
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(StorageError::Git("timeout".into())),
    }
}
