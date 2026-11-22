//! Finding the blocks. Each detector reads the manifests of one kind and adds blocks, the
//! links it reads (to a block named by its path or its package name, which another
//! detector may find), and the CI jobs with the paths they run for.

mod cargo;
mod ci;
mod compose;
mod docker;
mod docs;
mod go;
mod migrations;
mod node;
mod python;

use std::collections::{HashMap, HashSet};

pub(crate) use docker::is_dockerfile;

use crate::model::{Certainty, Kind, LinkKind, Role};

/// A block as found, before its id is settled and assay.toml corrects it.
pub(crate) struct Draft {
    /// Unique among the drafts: what the links found point at.
    pub key: String,
    /// The id it asks for: its package's or service's name, made a slug.
    pub want_id: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: Kind,
    pub role: Option<Role>,
    pub roots: Vec<String>,
    pub manifests: Vec<String>,
    pub image: Option<String>,
    /// Its package's name, for the dependencies naming it (npm) or a build naming it (Cargo).
    pub package: Option<String>,
    /// A Cargo package's binaries, for a build naming one (`cargo build --bin x`).
    pub bins: Vec<String>,
}

impl Draft {
    pub fn new(key: String, name: &str, kind: Kind) -> Self {
        Self {
            key,
            want_id: slug(name),
            name: name.to_string(),
            description: None,
            kind,
            role: None,
            roots: Vec::new(),
            manifests: Vec::new(),
            image: None,
            package: None,
            bins: Vec::new(),
        }
    }
}

/// What a link found points at, resolved once every block is known and assay.toml has corrected
/// them: a block merged into another then stands for it, one ignored for none.
pub(crate) enum To {
    /// The draft of this key.
    Key(String),
    /// The npm package of this name, when it is one of the repository's.
    Npm(String),
    /// The block holding this path, or the packages under it when it is a folder of them.
    Within(String),
    /// The same, but of the Cargo packages only those a build names (`cargo build -p x`,
    /// `--bin y`): the others are copied for the workspace to resolve, or as what the one
    /// built uses.
    Built { path: String, names: Vec<String> },
    /// The draft of this key (an image), else what this folder holds (compose `build`: its
    /// Dockerfile, else its context).
    Image { image: String, context: String },
}

pub(crate) struct Pending {
    pub from: String,
    pub to: To,
    pub kind: LinkKind,
    pub certainty: Certainty,
    pub via: String,
}

/// A CI job, and the paths whose changes run it (`rules:changes`, a workflow's `paths`).
pub(crate) struct Job {
    pub name: String,
    pub paths: Vec<String>,
}

pub(crate) struct Ctx<'a> {
    /// The repository's name, for the block of its top folder.
    name: &'a str,
    pub files: &'a [String],
    /// The text of the files [`reads`] picks out.
    texts: &'a HashMap<String, String>,
    set: HashSet<&'a str>,
    pub drafts: Vec<Draft>,
    pub pending: Vec<Pending>,
    pub jobs: Vec<Job>,
    pub warnings: Vec<String>,
}

impl<'a> Ctx<'a> {
    fn new(name: &'a str, files: &'a [String], texts: &'a HashMap<String, String>) -> Self {
        Self {
            name,
            files,
            texts,
            set: files.iter().map(String::as_str).collect(),
            drafts: Vec::new(),
            pending: Vec::new(),
            jobs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// The files named `name`, wherever they are.
    pub fn named(&self, name: &str) -> Vec<&'a str> {
        self.files.iter().map(String::as_str).filter(|f| file_name(f) == name).collect()
    }

    pub fn exists(&self, path: &str) -> bool {
        self.set.contains(path)
    }

    /// A folder's name; the repository's own for its top.
    pub fn folder_name(&self, dir: &str) -> String {
        if dir == "." {
            if self.name.is_empty() { "repository".to_string() } else { self.name.to_string() }
        } else {
            file_name(dir).to_string()
        }
    }

    /// Whether some file is at `path` or under it.
    pub fn any_at(&self, path: &str) -> bool {
        path == "." || self.exists(path) || self.files.iter().any(|f| crate::model::holds(path, f))
    }

    /// A file's text; one that could not be read is a warning.
    pub fn read(&mut self, path: &str) -> Option<String> {
        debug_assert!(reads(path), "{path} is read, but `reads` does not pick it out: a forge would not hand it over");
        match self.texts.get(path) {
            Some(s) => Some(s.clone()),
            None => {
                self.warn(path, "could not be read");
                None
            }
        }
    }

    pub fn warn(&mut self, path: &str, e: impl std::fmt::Display) {
        self.warnings.push(format!("{path}: {e}"));
    }

    /// Adds a draft; one already found under its key (a service in two compose files)
    /// takes its manifests instead.
    pub fn add(&mut self, d: Draft) {
        if let Some(have) = self.drafts.iter_mut().find(|x| x.key == d.key) {
            for m in d.manifests {
                if !have.manifests.contains(&m) {
                    have.manifests.push(m);
                }
            }
            have.image = have.image.take().or(d.image);
            have.role = have.role.or(d.role);
        } else {
            self.drafts.push(d);
        }
    }

    pub fn link(&mut self, from: &str, to: To, kind: LinkKind, certainty: Certainty, via: &str) {
        self.pending.push(Pending { from: from.to_string(), to, kind, certainty, via: via.to_string() });
    }
}

/// Whether a detector reads this file's text: a manifest, a Dockerfile, a compose file, a CI
/// configuration.
pub(crate) fn reads(path: &str) -> bool {
    let name = file_name(path);
    matches!(name, "Cargo.toml" | "package.json" | "pyproject.toml" | "go.mod")
        || docker::is_dockerfile(name)
        || compose::is_compose(name)
        || ci::reads(path)
}

/// Every detector, over the files of a repository (`name`) and the text of those [`reads`]
/// picks out.
pub(crate) fn detect<'a>(name: &'a str, files: &'a [String], texts: &'a HashMap<String, String>) -> Ctx<'a> {
    let mut cx = Ctx::new(name, files, texts);
    cargo::detect(&mut cx);
    node::detect(&mut cx);
    python::detect(&mut cx);
    go::detect(&mut cx);
    migrations::detect(&mut cx);
    docker::detect(&mut cx);
    compose::detect(&mut cx);
    ci::detect(&mut cx);
    docs::detect(&mut cx);
    cx
}

/// The folder of a path: "." at the top.
pub(crate) fn dir_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((d, _)) => d.to_string(),
        None => ".".to_string(),
    }
}

pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `rel` from the folder `dir`, `..` and `.` resolved: "." for the top.
pub(crate) fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if dir == "." || dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    if parts.is_empty() { ".".to_string() } else { parts.join("/") }
}

/// A manifest's description, unless it says nothing.
pub(crate) fn described(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// A name as an id: lower case letters and digits, dashes between words.
pub(crate) fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() { "block".to_string() } else { out.to_string() }
}

/// The part of a path pattern before its first wildcard, as a folder: "" when it starts
/// with one (`**/*.rs`) or names a variable.
pub(crate) fn literal_prefix(pattern: &str) -> String {
    if pattern.contains('$') {
        return String::new();
    }
    let cut = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    let head = &pattern[..cut];
    // `crates/a/**` and `crates/a/` are the folder; `crates/a*` its parent
    let head = if cut < pattern.len() && !head.ends_with('/') { head.rsplit_once('/').map(|(d, _)| d).unwrap_or("") } else { head };
    head.trim_start_matches("./").trim_start_matches('/').trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_names() {
        assert_eq!(join("crates/a", "../b"), "crates/b");
        assert_eq!(join(".", "./web"), "web");
        assert_eq!(join("web", ".."), ".");
        assert_eq!(dir_of("Cargo.toml"), ".");
        assert_eq!(slug("@forgust/Web UI"), "forgust-web-ui");
        assert_eq!(literal_prefix("crates/forgust-server/**/*"), "crates/forgust-server");
        assert_eq!(literal_prefix("web/src/*.ts"), "web/src");
        assert_eq!(literal_prefix("web/app*"), "web");
        assert_eq!(literal_prefix("**/*.rs"), "");
        assert_eq!(literal_prefix("Dockerfile"), "Dockerfile");
    }
}
