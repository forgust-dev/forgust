use std::path::Path;
use std::process::Command;

use forgust_storage::*;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Build a small history in a work tree and push it into the stored repo.
fn seed(storage: &LocalStorage, repo: i64, work: &Path) -> (String, String) {
    git(work, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("README.md"), "# Hello\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    git(work, &["add", "."]);
    git(work, &["commit", "-q", "-m", "first"]);
    let first = git(work, &["rev-parse", "HEAD"]);
    std::fs::write(work.join("src/lib.rs"), "fn a() {}\nfn B() {}\nfn c() {}\n").unwrap();
    git(work, &["mv", "README.md", "README"]);
    git(work, &["add", "."]);
    git(work, &["commit", "-q", "-m", "second", "-m", "body text"]);
    let second = git(work, &["rev-parse", "HEAD"]);
    let path = storage.repo_path(repo);
    git(work, &["push", "-q", path.to_str().unwrap(), "main", "main:refs/heads/feature"]);
    git(work, &["tag", "-a", "v1", "-m", "release", &first]);
    git(work, &["push", "-q", path.to_str().unwrap(), "v1"]);
    (first, second)
}

async fn exercise(format: RefFormat) {
    let root = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let storage = LocalStorage::new(LocalStorageConfig::new(root.path()));
    let repo = 42;
    storage.create_repo(repo, CreateRepoOptions { default_branch: "main".into(), ref_format: format }).await.unwrap();
    assert!(storage.is_empty(repo).await.unwrap());
    assert_eq!(storage.ref_format(repo).await.unwrap(), format);
    let (first, second) = seed(&storage, repo, work.path());

    assert!(!storage.is_empty(repo).await.unwrap());
    assert_eq!(storage.default_branch(repo).await.unwrap().as_deref(), Some("main"));
    assert_eq!(storage.resolve(repo, "main").await.unwrap().as_deref(), Some(second.as_str()));
    assert_eq!(storage.resolve(repo, "v1").await.unwrap().as_deref(), Some(first.as_str()));
    assert_eq!(storage.resolve(repo, "nope").await.unwrap(), None);

    let tree = storage.tree(repo, "main", "").await.unwrap();
    let names: Vec<_> = tree.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(names, vec![("src", EntryKind::Tree), ("README", EntryKind::Blob)]);
    let sub = storage.tree(repo, &second, "src").await.unwrap();
    assert_eq!(sub[0].path, "src/lib.rs");

    let blob = storage.blob(repo, "main", "src/lib.rs", 1 << 20).await.unwrap();
    assert_eq!(blob.data, b"fn a() {}\nfn B() {}\nfn c() {}\n");
    let small = storage.blob(repo, "main", "src/lib.rs", 4).await.unwrap();
    assert!(small.truncated);
    assert_eq!(small.size, 30);
    assert!(matches!(storage.blob(repo, "main", "missing", 10).await, Err(StorageError::PathNotFound(_))));

    let log = storage.log(repo, "main", None, 0, 10).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].subject, "second");
    assert_eq!(log[0].body, "body text");
    assert_eq!(log[0].parents, vec![first.clone()]);
    let log_path = storage.log(repo, "main", Some("src/lib.rs"), 0, 10).await.unwrap();
    assert_eq!(log_path.len(), 2);

    let diff = storage.diff(repo, Some(&first), &second, DiffOptions::default()).await.unwrap();
    assert_eq!(diff.total_files, 2);
    let lib = diff.files.iter().find(|f| f.new_path == "src/lib.rs").unwrap();
    assert_eq!((lib.additions, lib.deletions), (2, 1));
    let renamed = diff.files.iter().find(|f| f.new_path == "README").unwrap();
    assert_eq!(renamed.status, "renamed");
    let root_diff = storage.diff(repo, None, &first, DiffOptions::default()).await.unwrap();
    assert_eq!(root_diff.total_files, 2);
    assert!(root_diff.files.iter().all(|f| f.status == "added"));

    let blame = storage.blame(repo, "main", "src/lib.rs").await.unwrap();
    let total: usize = blame.iter().map(|h| h.lines.len()).sum();
    assert_eq!(total, 3);
    assert_eq!(blame[0].commit.id, first);
    assert_eq!(blame[0].commit.subject, "first");
    assert_eq!(blame[1].commit.id, second);

    let refs = storage.refs(repo, "refs/heads/").await.unwrap();
    assert_eq!(refs.len(), 2);
    let tags = storage.refs(repo, "refs/tags/").await.unwrap();
    assert_eq!(tags[0].peeled.as_deref(), Some(first.as_str()));
    assert_eq!(tags[0].commit.as_ref().unwrap().subject, "first");

    assert!(storage.is_ancestor(repo, &first, &second).await.unwrap());
    assert!(!storage.is_ancestor(repo, &second, &first).await.unwrap());

    let mut archive = storage.archive(repo, "main", ArchiveFormat::TarGz, "proj-main").await.unwrap();
    let mut bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut archive.stdout, &mut bytes).await.unwrap();
    assert!(archive.wait().await.unwrap().success());
    assert_eq!(&bytes[..2], &[0x1f, 0x8b]);

    storage.maintenance(repo, true).await.unwrap();
    assert_eq!(storage.resolve(repo, "main").await.unwrap().as_deref(), Some(second.as_str()));

    let bundle = root.path().join("b.bundle");
    assert!(storage.bundle_create(repo, &bundle).await.unwrap());
    storage.restore_from_bundle(7, Some(&bundle), CreateRepoOptions { default_branch: "main".into(), ref_format: format }).await.unwrap();
    assert_eq!(storage.resolve(7, "main").await.unwrap().as_deref(), Some(second.as_str()));
    assert_eq!(storage.refs(7, "refs/").await.unwrap().len(), 3);

    storage.delete_repo(repo).await.unwrap();
    assert!(!storage.repo_exists(repo).await.unwrap());
}

#[tokio::test]
async fn files_backend_uses_gitoxide() {
    exercise(RefFormat::Files).await;
}

#[tokio::test]
async fn reftable_backend_uses_git_processes() {
    exercise(RefFormat::Reftable).await;
}

#[tokio::test]
async fn empty_repo_bundle_is_skipped() {
    let root = tempfile::tempdir().unwrap();
    let storage = LocalStorage::new(LocalStorageConfig::new(root.path()));
    storage.create_repo(1, CreateRepoOptions { default_branch: "main".into(), ref_format: RefFormat::Files }).await.unwrap();
    assert!(!storage.bundle_create(1, &root.path().join("x.bundle")).await.unwrap());
}
