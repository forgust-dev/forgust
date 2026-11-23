//! What a Smart View job leaves behind (`artifacts:reports:smart_view`), and the forge
//! reads: the map of the commit the job cloned, and what the forge needs beside it to show it
//! (which of its paths are files, who opted out). `assay report` writes it.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{CONFIG_FILE, Config, Options, RepoMap};

/// The file the job writes, uploaded as its report.
pub const FILE: &str = "assay-report.json";
pub const VERSION: u32 = 1;
/// Largest report the forge reads.
pub const MAX_BYTES: usize = 16 << 20;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    /// The commit mapped: the clone's HEAD.
    pub commit: String,
    /// How many days of history told who knows each block.
    pub days: u32,
    /// Whether the repository has an assay.toml.
    pub config_file: bool,
    /// The map; none when assay.toml could not be read (`error` says why).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<RepoMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The blocks' roots and manifests that are files: the others are folders.
    #[serde(default)]
    pub files: Vec<String>,
    /// The addresses (lowercased) of who opted out (assay.toml's `[people] exclude`): those
    /// named, and every other address of the same people in the history read.
    #[serde(default)]
    pub excluded: Vec<String>,
    /// The patterns of the files left out of every block (the defaults, then assay.toml's
    /// `exclude`), for [`crate::Patterns`]: a path they match is in no block, whatever root
    /// holds it.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// The report of the repository at `root`, a git clone. Fails only when the folder cannot be
/// read or is at no commit: an assay.toml that does not parse is the report's `error`.
pub fn report(root: &Path, opts: &Options) -> anyhow::Result<Report> {
    let (snapshot, days) = crate::snapshot(root, opts)?;
    let commit = snapshot.commit.clone().ok_or_else(|| anyhow::anyhow!("{}: not at a commit of a git repository", root.display()))?;
    let mut out =
        Report { version: VERSION, commit, days, config_file: snapshot.files.iter().any(|f| f == CONFIG_FILE), ..Default::default() };
    match crate::map_snapshot(&snapshot) {
        Ok(map) => {
            let listed: HashSet<&str> = snapshot.files.iter().map(String::as_str).collect();
            let mut files: Vec<String> = map
                .blocks
                .iter()
                .flat_map(|b| b.roots.iter().chain(&b.manifests))
                .filter(|p| listed.contains(p.as_str()))
                .cloned()
                .collect();
            files.sort();
            files.dedup();
            out.files = files;
            // it parsed: the map was made with it
            let config = snapshot.texts.get(CONFIG_FILE).and_then(|t| Config::parse(t).ok()).unwrap_or_default();
            out.excluded = crate::people::excluded_emails(snapshot.history.as_deref().unwrap_or_default(), &config.people.exclude);
            out.exclude = crate::files::DEFAULT_EXCLUDES.iter().map(|p| p.to_string()).chain(config.exclude).collect();
            out.map = Some(map);
        }
        Err(e) => out.error = Some(e.to_string()),
    }
    Ok(out)
}
