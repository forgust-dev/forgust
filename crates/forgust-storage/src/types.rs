use serde::Serialize;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};

pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
pub const ZERO_OID: &str = "0000000000000000000000000000000000000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RefFormat {
    #[default]
    Files,
    Reftable,
}

impl RefFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefFormat::Files => "files",
            RefFormat::Reftable => "reftable",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "files" => Some(RefFormat::Files),
            "reftable" => Some(RefFormat::Reftable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CreateRepoOptions {
    pub default_branch: String,
    pub ref_format: RefFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitService {
    UploadPack,
    ReceivePack,
    UploadArchive,
}

impl GitService {
    pub fn name(&self) -> &'static str {
        match self {
            GitService::UploadPack => "git-upload-pack",
            GitService::ReceivePack => "git-receive-pack",
            GitService::UploadArchive => "git-upload-archive",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "git-upload-pack" => Some(GitService::UploadPack),
            "git-receive-pack" => Some(GitService::ReceivePack),
            "git-upload-archive" => Some(GitService::UploadArchive),
            _ => None,
        }
    }
    pub(crate) fn subcommand(&self) -> &'static str {
        &self.name()[4..]
    }
}

#[derive(Debug, Clone, Default)]
pub struct ServiceOptions {
    /// HTTP smart protocol: `--stateless-rpc`.
    pub stateless_rpc: bool,
    /// HTTP `info/refs`: `--advertise-refs`.
    pub advertise_refs: bool,
    /// Value of the `Git-Protocol` header / `GIT_PROTOCOL` SSH env (for v2).
    pub git_protocol: Option<String>,
    /// Extra environment, passed through to hooks.
    pub env: Vec<(String, String)>,
    /// `core.hooksPath` for this invocation.
    pub hooks_path: Option<std::path::PathBuf>,
}

/// A running git process. Write the request to `stdin`, read the response from
/// `stdout`, then `wait()` for its exit status.
pub struct ServiceProcess {
    pub stdin: Option<Pin<Box<dyn AsyncWrite + Send + Unpin>>>,
    pub stdout: Pin<Box<dyn AsyncRead + Send + Unpin>>,
    pub stderr: Option<Pin<Box<dyn AsyncRead + Send + Unpin>>>,
    pub child: tokio::process::Child,
    /// The process's slot under the git process limit, freed with it.
    pub slot: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl ServiceProcess {
    pub async fn wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        drop(self.stdin.take());
        self.child.wait().await
    }
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct RefInfo {
    pub name: String,
    pub target: String,
    /// For annotated tags: the commit the tag points to.
    pub peeled: Option<String>,
    pub commit: Option<CommitSummary>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CommitSummary {
    pub id: String,
    pub subject: String,
    pub author_name: String,
    pub committed_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Tree,
    Blob,
    Executable,
    Symlink,
    Submodule,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct TreeEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Blob {
    pub id: String,
    pub size: u64,
    pub kind: EntryKind,
    /// Up to `max_bytes` of content.
    #[serde(skip)]
    pub data: Vec<u8>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct Person {
    pub name: String,
    pub email: String,
    pub time: i64,
    /// The account with this (verified) email, filled in by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CommitInfo {
    pub id: String,
    pub parents: Vec<String>,
    pub author: Person,
    pub committer: Person,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct BlameHunk {
    pub commit: CommitSummary,
    /// First line of the hunk in the final file (1-based).
    pub start_line: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    TarGz,
    Zip,
    Tar,
}

impl ArchiveFormat {
    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "tar.gz" | "tgz" => Some(ArchiveFormat::TarGz),
            "zip" => Some(ArchiveFormat::Zip),
            "tar" => Some(ArchiveFormat::Tar),
            _ => None,
        }
    }
    pub fn ext(&self) -> &'static str {
        match self {
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Tar => "tar",
        }
    }
    pub fn content_type(&self) -> &'static str {
        match self {
            ArchiveFormat::TarGz => "application/gzip",
            ArchiveFormat::Zip => "application/zip",
            ArchiveFormat::Tar => "application/x-tar",
        }
    }
}

/// One file of a commit made by the server (suggestions, web edits).
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    /// New content; `None` deletes the file.
    pub content: Option<Vec<u8>>,
}

pub fn is_valid_oid(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Reject revisions that git could read as options or that contain odd characters.
pub fn validate_rev(rev: &str) -> bool {
    !rev.is_empty()
        && !rev.starts_with('-')
        && rev.len() < 1024
        && !rev.contains("..")
        // `^{/regex}` searches the whole history for a message
        && !rev.contains("^{/")
        && !rev.bytes().any(|b| b < 0x20 || b == 0x7f || b == b' ' || b == b':' || b == b'\\')
}

/// Reject paths that escape the tree or carry control characters.
pub fn validate_path(path: &str) -> bool {
    path.len() < 4096
        && !path.starts_with('/')
        && !path.starts_with('-')
        && !path.split('/').any(|seg| seg == ".." || seg == ".")
        && !path.bytes().any(|b| b < 0x20 || b == 0x7f)
}

/// Who a server-made commit is attributed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeTreeResult {
    /// The merged tree (written to the object store even when it has conflicts).
    pub tree: String,
    /// Paths with conflicts; empty when the merge is clean.
    pub conflicts: Vec<String>,
}

impl MergeTreeResult {
    pub fn clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}
