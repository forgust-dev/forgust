//! `forgust_assay::history` over a small repository whose history is written here: a Cargo
//! workspace and its image, a web app added then reverted, a crate renamed, a tool rewritten in
//! another language, a branch merged with two commits of its own, and commits that cannot
//! change the map. Then the history read after a commit, and cut short.

use std::path::{Path, PathBuf};
use std::process::Command;

use forgust_assay::{Changes, Chapter, History, HistoryOptions, Kind, Options};

const ADA: (&str, &str) = ("Ada", "ada@example.com");
const BOB: (&str, &str) = ("Bob", "bob@example.com");

fn git(dir: &Path, args: &[&str], author: (&str, &str)) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", author.0)
        .env("GIT_AUTHOR_EMAIL", author.1)
        .env("GIT_COMMITTER_NAME", author.0)
        .env("GIT_COMMITTER_EMAIL", author.1)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

/// Writes `files`, removes `gone`, and commits it all as `author`: the new commit.
fn commit(dir: &Path, message: &str, files: &[(&str, &str)], gone: &[&str], author: (&str, &str)) -> String {
    write(dir, files);
    for g in gone {
        std::fs::remove_file(dir.join(g)).unwrap();
    }
    git(dir, &["add", "-A"], author);
    git(dir, &["commit", "-q", "--allow-empty", "-m", message], author);
    git(dir, &["rev-parse", "HEAD"], author)
}

fn ids(list: &[forgust_assay::BlockRef]) -> Vec<&str> {
    list.iter().map(|b| b.id.as_str()).collect()
}

fn links(list: &[forgust_assay::Link]) -> Vec<String> {
    list.iter().map(|l| format!("{} {:?} {}", l.from, l.kind, l.to)).collect()
}

fn titled<'a>(h: &'a History, title: &str) -> &'a Chapter {
    h.chapters
        .iter()
        .find(|c| c.title == title)
        .unwrap_or_else(|| panic!("no chapter {title:?} in {:?}", h.chapters.iter().map(|c| &c.title).collect::<Vec<_>>()))
}

struct Repo {
    dir: PathBuf,
    first: String,
    web: String,
    merge: String,
}

/// The repository, in a folder of its own for each test (`name`): they run at once.
fn repo(name: &str) -> Repo {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"], ADA);
    let first = commit(
        &dir,
        "an api on a core",
        &[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
            ("crates/api/Cargo.toml", "[package]\nname = \"api\"\n\n[dependencies]\ncore = { path = \"../core\" }\n"),
            ("crates/api/src/main.rs", "fn main() {}\n"),
            ("crates/core/Cargo.toml", "[package]\nname = \"core\"\n"),
            ("crates/core/src/lib.rs", "\n"),
            ("Dockerfile", "FROM rust\nCOPY crates crates\nRUN cargo build -p api\n"),
        ],
        &[],
        ADA,
    );
    // nothing assay reads, and no file added: not even mapped
    commit(&dir, "words", &[("crates/api/src/main.rs", "fn main() { println!(\"hi\"); }\n")], &[], ADA);
    // a file added, but an example's: the map stays as it was
    commit(
        &dir,
        "an example",
        &[("examples/demo/Cargo.toml", "[package]\nname = \"demo\"\n"), ("examples/demo/src/main.rs", "fn main() {}\n")],
        &[],
        ADA,
    );
    let web = commit(&dir, "a web app", &[("web/package.json", "{\"name\": \"web\", \"dependencies\": {\"react\": \"19\"}}\n")], &[], BOB);
    commit(
        &dir,
        "a tool",
        &[("tools/tool/package.json", "{\"name\": \"tool\", \"bin\": \"index.js\"}\n"), ("tools/tool/index.js", "\n")],
        &[],
        ADA,
    );
    commit(
        &dir,
        "rename core to base",
        &[
            ("crates/core/Cargo.toml", "[package]\nname = \"base\"\n"),
            ("crates/api/Cargo.toml", "[package]\nname = \"api\"\n\n[dependencies]\nbase = { path = \"../core\" }\n"),
        ],
        &[],
        ADA,
    );
    commit(
        &dir,
        "the tool in Rust",
        &[("tools/tool/Cargo.toml", "[package]\nname = \"tool\"\n"), ("tools/tool/src/main.rs", "fn main() {}\n")],
        &["tools/tool/package.json", "tools/tool/index.js"],
        ADA,
    );
    // a branch of two commits, merged after one more on main
    git(&dir, &["checkout", "-q", "-b", "worker"], BOB);
    commit(
        &dir,
        "a worker",
        &[("crates/worker/Cargo.toml", "[package]\nname = \"worker\"\n"), ("crates/worker/src/main.rs", "fn main() {}\n")],
        &[],
        BOB,
    );
    commit(
        &dir,
        "the worker uses base",
        &[("crates/worker/Cargo.toml", "[package]\nname = \"worker\"\n\n[dependencies]\nbase = { path = \"../core\" }\n")],
        &[],
        BOB,
    );
    git(&dir, &["checkout", "-q", "main"], ADA);
    commit(&dir, "a readme", &[("README.md", "# Shop\n")], &[], ADA);
    git(&dir, &["merge", "-q", "--no-ff", "worker", "-m", "Merge branch 'worker'"], ADA);
    let merge = git(&dir, &["rev-parse", "HEAD"], ADA);
    git(&dir, &["revert", "--no-edit", &web], BOB);
    Repo { dir, first, web, merge }
}

/// Each commit that changed the map, with what it changed: blocks and links come and go, a
/// renamed crate keeps its links, a tool rewritten changes kind, and a merge's changes are told
/// by the commits of its branch.
#[test]
fn tells_what_each_commit_changed() {
    let r = repo("history-told");
    let h = forgust_assay::history(&r.dir, &Options::default(), &HistoryOptions::default()).unwrap();
    assert_eq!(h.since, None);
    assert_eq!(h.start, None, "from the first commit");
    let titles: Vec<&str> = h.chapters.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "an api on a core",
            "a web app",
            "a tool",
            "rename core to base",
            "the tool in Rust",
            "a worker",
            "the worker uses base",
            "Revert \"a web app\""
        ],
        "no chapter for what changed nothing: words, an example, a readme"
    );

    let first = &h.chapters[0];
    assert_eq!(first.commit, r.first);
    assert_eq!((first.author.as_str(), first.email.as_str()), ADA);
    assert_eq!(ids(&first.changes.blocks_added), ["api", "core", "image"]);
    assert_eq!(links(&first.changes.links_added), ["api Uses core", "image Builds api"]);

    let web = titled(&h, "a web app");
    assert_eq!(web.commit, r.web);
    assert_eq!((web.author.as_str(), ids(&web.changes.blocks_added)), ("Bob", vec!["web"]));

    // the crate keeps its manifest under another name: renamed, its link with it
    let renamed = &titled(&h, "rename core to base").changes;
    assert_eq!(renamed.blocks_renamed.iter().map(|r| (r.from.id.as_str(), r.to.id.as_str())).collect::<Vec<_>>(), [("core", "base")]);
    assert!(renamed.blocks_added.is_empty() && renamed.blocks_removed.is_empty());
    assert!(renamed.links_added.is_empty() && renamed.links_removed.is_empty(), "api uses base as it used core: {renamed:?}");

    let rewritten = &titled(&h, "the tool in Rust").changes;
    assert_eq!(
        rewritten.kinds_changed.iter().map(|k| (k.id.as_str(), k.from, k.to)).collect::<Vec<_>>(),
        [("tool", Kind::Node, Kind::Rust)]
    );
    assert!(rewritten.blocks_added.is_empty() && rewritten.blocks_removed.is_empty());

    // the merge brought them, made by the commits of its branch: theirs, at the merge's time
    let worker = titled(&h, "a worker");
    assert_eq!((worker.merge.as_deref(), worker.author.as_str()), (Some(r.merge.as_str()), "Bob"));
    assert_eq!(ids(&worker.changes.blocks_added), ["worker"]);
    let uses = titled(&h, "the worker uses base");
    assert_eq!(uses.merge.as_deref(), Some(r.merge.as_str()));
    assert_eq!(links(&uses.changes.links_added), ["worker Uses base"]);
    assert_eq!(worker.at, uses.at, "both reached main with the merge");

    // the revert undoes the web app, and nothing else does
    let revert = titled(&h, "Revert \"a web app\"");
    assert_eq!(ids(&revert.changes.blocks_removed), ["web"]);
    assert!(revert.changes.undoes(&web.changes));
    assert!(!web.changes.undoes(&titled(&h, "a tool").changes));
    assert!(!Changes::default().undoes(&Changes::default()), "nothing undoes nothing");

    let story = h.story();
    assert!(story.contains("  » core is now base\n"), "{story}");
    assert!(story.contains("  ~ tool: node -> rust\n"), "{story}");
    assert!(story.contains("  + worker uses base\n"), "{story}");
}

/// After `since`, only what came after it; with too few maps to go back to the first commit, the
/// history starts from the map as it was before the commits mapped.
#[test]
fn reads_after_a_commit_and_cuts_short() {
    let r = repo("history-after");
    let after = |since: &str, max| {
        forgust_assay::history(&r.dir, &Options::default(), &HistoryOptions { since: Some(since.to_string()), max }).unwrap()
    };

    let h = after(&r.merge, 100);
    assert_eq!(h.since.as_deref(), Some(r.merge.as_str()));
    assert_eq!(h.chapters.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["Revert \"a web app\""]);

    // a commit the branch does not have: the whole history
    let h = after("0000000000000000000000000000000000000000", 100);
    assert_eq!((h.since, h.chapters.len()), (None, 8));

    // two maps: the revert and the merge's; what came before them is the map at its first parent
    let h = forgust_assay::history(&r.dir, &Options::default(), &HistoryOptions { since: None, max: 2 }).unwrap();
    let start = h.start.expect("cut short");
    assert_eq!(start.commit, git(&r.dir, &["rev-parse", &format!("{}^1", r.merge)], ADA));
    assert_eq!(
        h.chapters.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
        ["a worker", "the worker uses base", "Revert \"a web app\""]
    );

    // cut short after `since`: the history read replaces the forge's
    let h = after(&r.first, 2);
    assert!(h.since.is_none() && h.start.is_some());
}
