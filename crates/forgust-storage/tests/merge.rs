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

fn bot() -> Identity {
    Identity { name: "Forgust".into(), email: "forgust@localhost".into() }
}

/// main: base + 2 commits; topic: 20 commits on its own file; clash: edits the same line as main.
async fn setup() -> (tempfile::TempDir, tempfile::TempDir, LocalStorage) {
    let root = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let st = LocalStorage::new(LocalStorageConfig::new(root.path()));
    st.create_repo(1, CreateRepoOptions { default_branch: "main".into(), ref_format: RefFormat::Files }).await.unwrap();
    let w = work.path();
    git(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("shared.txt"), "one\ntwo\nthree\n").unwrap();
    git(w, &["add", "."]);
    git(w, &["commit", "-q", "-m", "base"]);
    git(w, &["checkout", "-q", "-b", "topic"]);
    for i in 0..20 {
        std::fs::write(w.join("topic.txt"), format!("{i}\n").repeat(i + 1)).unwrap();
        git(w, &["add", "."]);
        git(w, &["commit", "-q", "-m", &format!("topic {i}")]);
    }
    git(w, &["checkout", "-q", "-b", "clash", "main"]);
    std::fs::write(w.join("shared.txt"), "one\nTWO (clash)\nthree\n").unwrap();
    git(w, &["commit", "-q", "-am", "clash"]);
    git(w, &["checkout", "-q", "main"]);
    std::fs::write(w.join("shared.txt"), "one\nTWO (main)\nthree\n").unwrap();
    git(w, &["commit", "-q", "-am", "main 1"]);
    std::fs::write(w.join("main.txt"), "m\n").unwrap();
    git(w, &["add", "."]);
    git(w, &["commit", "-q", "-m", "main 2"]);
    git(w, &["push", "-q", st.repo_path(1).to_str().unwrap(), "main", "topic", "clash"]);
    (root, work, st)
}

#[tokio::test]
async fn merge_tree_clean_and_conflicting() {
    let (_r, _w, st) = setup().await;
    let clean = st.merge_tree(1, "main", "topic", None).await.unwrap();
    assert!(clean.clean());
    let conflict = st.merge_tree(1, "main", "clash", None).await.unwrap();
    assert_eq!(conflict.conflicts, vec!["shared.txt".to_string()]);
}

#[tokio::test]
async fn merge_commit_and_compare_and_swap() {
    let (_r, _w, st) = setup().await;
    let main = st.resolve(1, "main").await.unwrap().unwrap();
    let topic = st.resolve(1, "topic").await.unwrap().unwrap();
    let m = st.merge_tree(1, &main, &topic, None).await.unwrap();
    let author = Identity { name: "Olivia".into(), email: "o@example.com".into() };
    let c = st.commit_tree(1, &m.tree, &[main.clone(), topic.clone()], "Merge branch 'topic'", &author, &bot()).await.unwrap();
    let info = st.commit(1, &c).await.unwrap().unwrap();
    assert_eq!(info.parents, vec![main.clone(), topic.clone()]);
    assert_eq!(info.author.name, "Olivia");
    assert_eq!(info.committer.name, "Forgust");

    // CAS: fails when the branch moved, succeeds with the right expectation
    assert!(matches!(st.update_ref(1, "refs/heads/main", &c, Some(&topic)).await, Err(StorageError::RefChanged(_))));
    st.update_ref(1, "refs/heads/main", &c, Some(&main)).await.unwrap();
    assert_eq!(st.resolve(1, "main").await.unwrap().unwrap(), c);
    // "must not exist"
    st.update_ref(1, "refs/keep-around/x", &c, Some(ZERO_OID)).await.unwrap();
    assert!(st.update_ref(1, "refs/keep-around/x", &c, Some(ZERO_OID)).await.is_err());
    assert!(st.update_ref(1, "refs/heads/../x", &c, None).await.is_err());
}

#[tokio::test]
async fn rebase_twenty_commits_without_a_working_copy() {
    let (_r, _w, st) = setup().await;
    let main = st.resolve(1, "main").await.unwrap().unwrap();
    let base = st.merge_base(1, "main", "topic").await.unwrap().unwrap();
    let replays = replay_rebases();
    let new_tip = st.rebase(1, &main, &base, "refs/heads/topic", &bot()).await.unwrap();
    assert_eq!(replay_rebases(), replays + 1, "git replay should do the work");
    let commits = st.rev_list(1, &main, &new_tip, false).await.unwrap();
    assert_eq!(commits.len(), 20);
    assert!(st.is_ancestor(1, &main, &new_tip).await.unwrap());
    let tip = st.commit(1, &new_tip).await.unwrap().unwrap();
    assert_eq!(tip.subject, "topic 19");
    assert_eq!(tip.author.name, "Ada");
    // the rebased tree has both sides
    assert!(st.blob(1, &new_tip, "main.txt", 100).await.is_ok());
    assert!(st.blob(1, &new_tip, "topic.txt", 100).await.is_ok());
    // nothing left behind
    assert!(st.refs(1, "refs/forgust/").await.unwrap().is_empty());
    assert!(st.refs(1, REBASE_PREFIX).await.unwrap().is_empty());
}

#[tokio::test]
async fn rebase_reports_conflicts() {
    let (_r, _w, st) = setup().await;
    let main = st.resolve(1, "main").await.unwrap().unwrap();
    let base = st.merge_base(1, "main", "clash").await.unwrap().unwrap();
    match st.rebase(1, &main, &base, "refs/heads/clash", &bot()).await {
        Err(StorageError::Conflict(paths)) => assert_eq!(paths, vec!["shared.txt".to_string()]),
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn tree_with_changes_edits_without_a_working_copy() {
    use forgust_storage::FileChange;
    let (_r, _w, st) = setup().await;
    let main = st.resolve(1, "main").await.unwrap().unwrap();
    let files = st.list_paths(1, &main, 1000).await.unwrap().0;
    let existing = files.first().unwrap().clone();
    let tree = st
        .tree_with_changes(
            1,
            &main,
            &[
                FileChange { path: existing.clone(), content: Some(b"rewritten\n".to_vec()) },
                FileChange { path: "new/dir/file.txt".into(), content: Some(b"new\n".to_vec()) },
            ],
        )
        .await
        .unwrap();
    let c = st.commit_tree(1, &tree, std::slice::from_ref(&main), "edit", &bot(), &bot()).await.unwrap();
    assert_eq!(st.blob(1, &c, &existing, 1 << 20).await.unwrap().data, b"rewritten\n");
    assert_eq!(st.blob(1, &c, "new/dir/file.txt", 1 << 20).await.unwrap().data, b"new\n");
    // a deletion, and the parent is untouched
    let tree = st.tree_with_changes(1, &c, &[FileChange { path: "new/dir/file.txt".into(), content: None }]).await.unwrap();
    let c2 = st.commit_tree(1, &tree, std::slice::from_ref(&c), "rm", &bot(), &bot()).await.unwrap();
    assert!(!st.list_paths(1, &c2, 1000).await.unwrap().0.contains(&"new/dir/file.txt".to_string()));
    assert!(st.list_paths(1, &c, 1000).await.unwrap().0.contains(&"new/dir/file.txt".to_string()));
    // a directory is not a file
    assert!(st.tree_with_changes(1, &c, &[FileChange { path: "new".into(), content: Some(b"x".to_vec()) }]).await.is_err());
    assert!(st.tree_with_changes(1, &c, &[FileChange { path: "../x".into(), content: Some(b"x".to_vec()) }]).await.is_err());
}
