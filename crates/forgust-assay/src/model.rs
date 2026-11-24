//! What `assay map` gives: the blocks of a repository, the links between them and the
//! people who know them, as the JSON the CLI prints (see docs/smart-view.md).

use serde::{Deserialize, Serialize};

/// The version of this format. It goes up when a field changes meaning or goes away;
/// a new optional field keeps it.
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMap {
    pub schema: u32,
    /// The commit mapped, in a git repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub blocks: Vec<Block>,
    pub links: Vec<Link>,
    /// The files in no block: mostly a repository's top-level ones (README, LICENSE…).
    pub unassigned: Unassigned,
    /// What could not be read (a manifest that does not parse), and was left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What a block is, by what tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A Cargo package.
    Rust,
    /// An npm package.
    Node,
    /// A Python project (pyproject.toml, setup.py, setup.cfg).
    Python,
    /// A Go module.
    Go,
    /// An image a Dockerfile builds.
    Docker,
    /// A service of a compose file.
    Service,
    /// A Helm chart.
    Helm,
    /// Kubernetes manifests: a Kustomize folder with its bases and overlays, or a folder of
    /// manifests.
    Kubernetes,
    /// Ansible: a role, a collection, or the playbooks around roles.
    Ansible,
    /// A Terraform (or OpenTofu) module.
    Terraform,
    /// A Pulumi project.
    Pulumi,
    /// A database's schema, as its migrations.
    Migrations,
    /// The CI: its configuration files.
    Ci,
    /// The documentation folder.
    Docs,
    /// A block assay.toml declares and no manifest gives.
    Other,
}

/// What a block is for, when its manifest suggests it: a guess, which assay.toml corrects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Web,
    Api,
    Cli,
    Library,
    Database,
    Cache,
    Queue,
    Search,
    Storage,
    Mail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Found in a manifest (assay.toml may have corrected it).
    Detected,
    /// Declared by assay.toml only.
    Configured,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    /// Stable while the package, the service or the path it comes from keeps its name.
    pub id: String,
    pub name: String,
    /// What it is for, in a line: its manifest's (a package's `description`, an image's
    /// `org.opencontainers.image.description` label) or assay.toml's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// The folders (or files) whose files are the block's: a file belongs to the block
    /// with the deepest root that holds it. None for a service, which only a compose
    /// file declares.
    pub roots: Vec<String>,
    /// Where the block was found: its Cargo.toml, the compose files naming the service…
    pub manifests: Vec<String>,
    /// The block whose root holds this one's: a crate for its migrations, an app for a tool
    /// inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// For a service: the image it runs, as the compose file names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub files: usize,
    /// By number of files, the most first.
    pub languages: Vec<Language>,
    /// The CI jobs that build or test it: from their `rules:changes` or `paths` filters, or
    /// from assay.toml.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<String>,
    /// The people declared for it: CODEOWNERS, and assay.toml's referents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<String>,
    /// The people who changed it lately, the most active first: people to ask, not a ranking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<Person>,
    /// How many of them made at least a fifth of its recent changes: 1, one person knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_factor: Option<usize>,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Language {
    pub name: String,
    pub files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// Their commits touching the block within the history read (a year by default).
    pub commits: usize,
    /// The day of their latest one, `YYYY-MM-DD` (UTC).
    pub last: String,
}

/// How two blocks are linked: the arrow goes from the one that needs to the one needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// A package depends on another of the repository.
    Uses,
    /// An image is built from a block's files.
    Builds,
    /// A service runs an image built in the repository; so does what deploys it (a chart,
    /// manifests, a playbook, infrastructure code), by the image's name: a guess.
    Runs,
    /// A service starts after another (compose `depends_on`).
    DependsOn,
    /// A service reaches another: its environment names it.
    Connects,
    /// Declared in assay.toml: what no file tells (a web app calling an API).
    Calls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Certainty {
    /// A manifest or assay.toml says so.
    Declared,
    /// A guess, from a name found where it is used: to show apart, and to confirm.
    Inferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub kind: LinkKind,
    pub certainty: Certainty,
    /// The file that tells it.
    pub via: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Unassigned {
    pub files: usize,
    /// A few of them, to see what no block covers.
    pub sample: Vec<String>,
}

impl RepoMap {
    /// The block a file belongs to (its path from the repository's root): the one with the
    /// deepest root holding it.
    pub fn block_of(&self, path: &str) -> Option<&Block> {
        let mut best: Option<(&Block, usize)> = None;
        for b in &self.blocks {
            for r in &b.roots {
                if holds(r, path) && best.is_none_or(|(_, d)| depth(r) > d) {
                    best = Some((b, depth(r)));
                }
            }
        }
        best.map(|(b, _)| b)
    }
}

/// Whether `root` (a folder or a file, "." for the whole repository) holds `path`.
pub(crate) fn holds(root: &str, path: &str) -> bool {
    root == "." || path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// How deep a root is: the whole repository's is the shallowest.
pub(crate) fn depth(root: &str) -> usize {
    if root == "." { 0 } else { root.len() }
}
