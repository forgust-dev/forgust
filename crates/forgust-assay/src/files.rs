//! The files of a repository: those git tracks, or would (its ignore files apply); out of
//! git, those a walk of the folder finds. Without the paths left out: test data that
//! looks like real projects, others' code, and what assay.toml excludes.

use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Left out of every map, on top of assay.toml's `exclude`: test data and examples that look like
/// real projects, others' code (the modules Terraform downloads), and generated caches (sqlx's
/// queries).
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "**/node_modules/**",
    "**/vendor/**",
    "**/third_party/**",
    "**/fixtures/**",
    "**/__fixtures__/**",
    "**/testdata/**",
    "**/examples/**",
    "**/.terraform/**",
    "**/test-data/**",
    "/.sqlx/",
];

/// Whether [`DEFAULT_EXCLUDES`] leave `path` out.
pub fn excluded_by_default(path: &str) -> bool {
    static DEFAULTS: LazyLock<Patterns> = LazyLock::new(|| Patterns::new(DEFAULT_EXCLUDES).expect("the default patterns are valid"));
    DEFAULTS.matches(path)
}

/// Folders a walk out of git does not enter: build output and installed dependencies.
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".venv", "venv", "__pycache__", ".next", ".cache"];

/// The repository's files, relative to `root`, `/`-separated and sorted; and whether git
/// listed them (its history then tells who changed them).
pub fn list(root: &Path) -> anyhow::Result<(Vec<String>, bool)> {
    if let Some(files) = git_files(root) {
        return Ok((files, true));
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok((out, false))
}

fn git_files(root: &Path) -> Option<Vec<String>> {
    // a folder git ignores (a build's output) lists nothing: walked instead
    let ignored = Command::new("git").arg("-C").arg(root).args(["check-ignore", "-q", "."]).status().ok()?;
    if ignored.success() {
        return None;
    }
    let out =
        Command::new("git").arg("-C").arg(root).args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        // deleted from the work tree but not yet from the index, or a submodule
        .filter(|p| root.join(p).is_file())
        .collect();
    files.sort();
    files.dedup();
    Some(files)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            if !SKIP_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()) {
                walk(root, &entry.path(), out)?;
            }
        } else if ty.is_file() {
            out.push(entry.path().strip_prefix(root)?.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Paths matched as `.gitignore` patterns are: `/x` or `x/y` from the root only, `x`
/// anywhere, `x/` a folder; a folder matched leaves out all it holds.
pub struct Patterns(GlobSet);

impl Patterns {
    pub fn new<S: AsRef<str>>(patterns: impl IntoIterator<Item = S>) -> anyhow::Result<Self> {
        let mut set = GlobSetBuilder::new();
        for p in patterns {
            for g in gitignore_like(p.as_ref()) {
                set.add(GlobBuilder::new(&g).literal_separator(true).build()?);
            }
        }
        Ok(Self(set.build()?))
    }

    pub fn matches(&self, path: &str) -> bool {
        self.0.is_match(path)
    }

    /// The indices of the patterns given to `new` that match (each gave two globs).
    pub fn matching(&self, path: &str) -> Vec<usize> {
        let mut hits: Vec<usize> = self.0.matches(path).into_iter().map(|i| i / 2).collect();
        hits.dedup();
        hits
    }
}

/// A gitignore pattern as globs: the path itself, and what it holds when it is a folder.
fn gitignore_like(p: &str) -> [String; 2] {
    let p = p.trim();
    let anchored = p.starts_with('/') || p.trim_end_matches('/').contains('/');
    let body = p.trim_start_matches('/').trim_end_matches('/');
    let base = if anchored { body.to_string() } else { format!("**/{body}") };
    [base.clone(), format!("{base}/**")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_read_as_gitignore_does() {
        let p = Patterns::new(["bench", "/docs/", "web/*.md", "**/fixtures/**"]).unwrap();
        assert!(p.matches("bench/run.rs"));
        assert!(p.matches("crates/x/bench/a.rs"), "a bare name matches anywhere");
        assert!(p.matches("docs/a/b.md"));
        assert!(!p.matches("crates/docs/a.md"), "a leading slash anchors to the root");
        assert!(p.matches("web/README.md"));
        assert!(!p.matches("web/src/README.md"), "* stops at a slash");
        assert!(p.matches("tests/fixtures/app/Cargo.toml"));
        assert_eq!(p.matching("docs/x.md"), vec![1]);
    }
}
