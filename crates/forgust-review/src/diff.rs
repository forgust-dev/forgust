//! The changes to review: `git diff` in the job's clone, parsed with the
//! server's own parser (the line numbers the model sees are the ones the
//! server checks), cut into requests by file, never inside one.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};
use forgust_storage::{DiffFile, DiffLineKind};

use crate::report::Skipped;

/// Lines kept per file; a longer file is not reviewed.
const MAX_LINES_PER_FILE: usize = 3000;

/// `git diff base..head` in `dir`.
pub fn git_diff(dir: &Path, base: &str, head: &str) -> anyhow::Result<String> {
    for rev in [base, head] {
        if rev.is_empty() || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("not a commit id: `{rev}`");
        }
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["diff", "--no-color", "--no-ext-diff", "-M", "--unified=3", "--src-prefix=a/", "--dst-prefix=b/"])
        .arg(format!("{base}..{head}"))
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A file at `rev`, when it exists (the project's review instructions).
pub fn git_show(dir: &Path, rev: &str, path: &str, max: usize) -> Option<String> {
    if !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let out = Command::new("git").arg("-C").arg(dir).arg("show").arg(format!("{rev}:{path}")).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    Some(s)
}

/// One file as the model reads it.
#[derive(Debug, Clone)]
pub struct Patch {
    pub path: String,
    pub text: String,
}

/// Files that are never worth a model's time.
fn generated(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    const LOCKS: &[&str] = &[
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lockb",
        "go.sum",
        "poetry.lock",
        "uv.lock",
        "Pipfile.lock",
        "composer.lock",
        "Gemfile.lock",
        "flake.lock",
    ];
    LOCKS.contains(&name) || name.ends_with(".min.js") || name.ends_with(".min.css") || name.ends_with(".map") || name.ends_with(".snap")
}

/// The files to review, written for the model, and those left out.
pub fn prepare(diff_text: &str) -> (Vec<Patch>, Vec<Skipped>) {
    let mut patches = Vec::new();
    let mut skipped = Vec::new();
    for f in forgust_storage::parse_unified_diff(diff_text, MAX_LINES_PER_FILE) {
        let path = if f.new_path.is_empty() || f.status == "deleted" { f.old_path.clone() } else { f.new_path.clone() };
        let why = if f.binary {
            Some("binary")
        } else if f.status == "deleted" {
            Some("deleted")
        } else if generated(&path) {
            Some("generated or a lock file")
        } else if f.collapsed {
            Some("too many changed lines")
        } else if f.hunks.is_empty() {
            Some("no line changed")
        } else {
            None
        };
        match why {
            Some(reason) => skipped.push(Skipped { path, reason: reason.into() }),
            None => patches.push(Patch { text: render(&f, &path), path }),
        }
    }
    (patches, skipped)
}

/// A file's hunks, every line numbered: `L12 +` an added line 12 of the new
/// file, `L13  ` a kept one, `O7 -` a removed line 7 of the old file.
fn render(f: &DiffFile, path: &str) -> String {
    let mut s = format!("=== {path}");
    if f.status == "renamed" && f.old_path != f.new_path {
        s.push_str(&format!(" (renamed from {})", f.old_path));
    }
    if f.status == "added" {
        s.push_str(" (new file)");
    }
    s.push('\n');
    for h in &f.hunks {
        s.push_str(&h.header);
        s.push('\n');
        for l in &h.lines {
            match (l.kind, l.new, l.old) {
                (DiffLineKind::Add, Some(n), _) => s.push_str(&format!("L{n} + {}\n", l.text)),
                (DiffLineKind::Context, Some(n), _) => s.push_str(&format!("L{n}   {}\n", l.text)),
                (DiffLineKind::Del, _, Some(o)) => s.push_str(&format!("O{o} - {}\n", l.text)),
                _ => {}
            }
        }
    }
    s
}

/// Files per request, in order, each request at most `max_bytes` (a file
/// alone larger than that is left out), at most `max_requests` of them.
pub fn chunk(patches: Vec<Patch>, max_bytes: usize, max_requests: usize) -> (Vec<Vec<Patch>>, Vec<Skipped>) {
    let mut chunks: Vec<Vec<Patch>> = Vec::new();
    let mut skipped = Vec::new();
    let mut size = 0usize;
    for p in patches {
        if p.text.len() > max_bytes {
            skipped.push(Skipped { path: p.path, reason: "too large to review".into() });
            continue;
        }
        let fits = chunks.last().is_some() && size + p.text.len() <= max_bytes;
        if !fits {
            if chunks.len() == max_requests {
                skipped.push(Skipped { path: p.path, reason: "past the review's size limit".into() });
                continue;
            }
            chunks.push(Vec::new());
            size = 0;
        }
        size += p.text.len();
        chunks.last_mut().expect("pushed").push(p);
    }
    (chunks, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@ fn main() {
 fn main() {
-    let x = 1;
+    let x = 2;
 }
diff --git a/Cargo.lock b/Cargo.lock
index 1111111..2222222 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-a
+b
";

    #[test]
    fn numbered_lines_and_lock_files_left_out() {
        let (patches, skipped) = prepare(DIFF);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, "src/a.rs");
        let t = &patches[0].text;
        assert!(t.contains("O2 -     let x = 1;"), "{t}");
        assert!(t.contains("L2 +     let x = 2;"), "{t}");
        assert!(t.contains("L1   fn main() {"), "{t}");
        assert_eq!(skipped, vec![Skipped { path: "Cargo.lock".into(), reason: "generated or a lock file".into() }]);
    }

    #[test]
    fn chunks_by_file_and_nothing_cut() {
        let p = |n: &str, len: usize| Patch { path: n.into(), text: "x".repeat(len) };
        let (chunks, skipped) = chunk(vec![p("a", 60), p("b", 30), p("c", 50), p("huge", 500), p("d", 90), p("e", 10)], 100, 2);
        let names: Vec<Vec<&str>> = chunks.iter().map(|c| c.iter().map(|p| p.path.as_str()).collect()).collect();
        // "d" needs a third request; "e" still fits in the second
        assert_eq!(names, vec![vec!["a", "b"], vec!["c", "e"]]);
        let left: Vec<&str> = skipped.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(left, vec!["huge", "d"]);
    }

    #[test]
    fn refuses_what_is_not_a_commit() {
        assert!(git_diff(Path::new("."), "HEAD~1", "abc").is_err());
    }
}
