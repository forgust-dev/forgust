//! assay.toml, at the top of the repository: what the detection gets wrong, corrected, and
//! what no manifest tells, declared. Blocks are named by the ids `assay map` prints.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;

use crate::detect::join;
use crate::model::{Block, Certainty, Kind, Link, LinkKind, Role, Source};
use crate::push_link;

pub const FILE: &str = "assay.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The format's version; this assay reads 1.
    pub version: Option<u32>,
    /// Paths to leave out, as `.gitignore` patterns (on top of fixtures, vendored code…).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// By id: a detected block corrected, or a block of its own (it then needs `paths`).
    #[serde(default)]
    pub blocks: BTreeMap<String, BlockConfig>,
    /// What no file tells (a web app calling an API), or a link found that is wrong.
    #[serde(default)]
    pub links: Vec<LinkConfig>,
    #[serde(default)]
    pub people: PeopleConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockConfig {
    pub name: Option<String>,
    /// What it is for, in a line: instead of its manifest's, or for a block that has none (a
    /// compose service, a Go module).
    pub description: Option<String>,
    pub kind: Option<Kind>,
    pub role: Option<Role>,
    /// Its roots, instead of those detected.
    pub paths: Option<Vec<String>>,
    /// Not a block (an example, a helper's package.json): its files go to the block around it.
    #[serde(default)]
    pub ignore: bool,
    /// Part of another block, which takes its roots, its manifests and its links.
    pub merge_into: Option<String>,
    /// People to ask about it, on top of those its history gives: `@user` or an address.
    #[serde(default)]
    pub referents: Vec<String>,
    /// Its CI jobs, by name or glob (`web-*`), on top of those found from their paths.
    #[serde(default)]
    pub jobs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfig {
    pub from: String,
    pub to: String,
    /// `calls` when added; when removing, any kind unless given. A guess found and declared
    /// with its kind (`connects`) is confirmed.
    pub kind: Option<LinkKind>,
    /// The link found is wrong: taken away rather than added.
    #[serde(default)]
    pub remove: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeopleConfig {
    /// Never listed, by one of their names or addresses (all of theirs are then left out):
    /// people who opted out, bots.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// How many days of history tell who knows a block (a year by default).
    pub days: Option<u32>,
}

/// The blocks assay.toml took away: each merged into another (its id), or ignored (none).
#[derive(Debug, Default)]
pub(crate) struct Gone(HashMap<String, Option<String>>);

impl Gone {
    /// What stands for block `id` now: itself, the block it was merged into, or none when it
    /// was ignored.
    pub fn follow(&self, id: &str) -> Option<String> {
        let mut id = id;
        // merges never loop (one into itself is skipped): a step per block gone, at most
        for _ in 0..=self.0.len() {
            match self.0.get(id) {
                None => return Some(id.to_string()),
                Some(None) => return None,
                Some(Some(into)) => id = into,
            }
        }
        None
    }

    /// Each block merged into another, with the one holding it in the end.
    pub fn merged(&self) -> impl Iterator<Item = (&str, String)> {
        self.0.keys().filter_map(|id| Some((id.as_str(), self.follow(id)?)))
    }
}

impl Config {
    /// The repository's assay.toml; none is an empty one.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = root.join(FILE);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let c: Config = toml::from_str(text).map_err(|e| anyhow::anyhow!("{FILE}: {}", e.message()))?;
        if let Some(v) = c.version
            && v != 1
        {
            anyhow::bail!("{FILE}: version {v} is unknown to this assay, which reads version 1");
        }
        Ok(c)
    }

    /// The corrections of the blocks detected, and the blocks it declares; before the links
    /// found are resolved, which then follow what it took away.
    pub(crate) fn apply_blocks(&self, blocks: &mut Vec<Block>, warnings: &mut Vec<String>) -> Gone {
        // the blocks it declares first: a detected one may merge into them
        for (id, c) in &self.blocks {
            if c.paths.is_some() && !c.ignore && c.merge_into.is_none() && !blocks.iter().any(|b| &b.id == id) {
                blocks.push(Block {
                    id: id.clone(),
                    name: id.clone(),
                    description: None,
                    kind: Kind::Other,
                    role: None,
                    roots: Vec::new(),
                    manifests: vec![FILE.to_string()],
                    parent: None,
                    image: None,
                    files: 0,
                    languages: Vec::new(),
                    jobs: Vec::new(),
                    owners: Vec::new(),
                    people: Vec::new(),
                    bus_factor: None,
                    source: Source::Configured,
                });
            }
        }
        let mut gone = Gone::default();
        for (id, c) in &self.blocks {
            let Some(i) = blocks.iter().position(|b| &b.id == id) else {
                warnings.push(format!("{FILE}: no block `{id}` was found (did its id change?), and it has no `paths` to be one"));
                continue;
            };
            if c.ignore {
                blocks.remove(i);
                gone.0.insert(id.clone(), None);
                continue;
            }
            if let Some(into) = &c.merge_into {
                merge(blocks, i, into, &mut gone, warnings);
                continue;
            }
            let b = &mut blocks[i];
            if let Some(n) = &c.name {
                b.name = n.clone();
            }
            if let Some(d) = &c.description {
                b.description = Some(d.clone());
            }
            if let Some(k) = c.kind {
                b.kind = k;
            }
            if let Some(r) = c.role {
                b.role = Some(r);
            }
            if let Some(p) = &c.paths {
                b.roots = p.iter().map(|p| join(".", p)).collect();
            }
        }
        gone
    }

    /// The links it declares, and the links found it takes away.
    pub(crate) fn apply_links(&self, blocks: &[Block], links: &mut Vec<Link>, warnings: &mut Vec<String>) {
        for l in &self.links {
            if l.remove {
                let before = links.len();
                links.retain(|x| !(x.from == l.from && x.to == l.to && l.kind.is_none_or(|k| k == x.kind)));
                if links.len() == before {
                    warnings.push(format!("{FILE}: no link {} → {} was found to remove", l.from, l.to));
                }
            } else if [&l.from, &l.to].iter().all(|id| blocks.iter().any(|b| &&b.id == id)) {
                let kind = l.kind.unwrap_or(LinkKind::Calls);
                push_link(
                    links,
                    Link { from: l.from.clone(), to: l.to.clone(), kind, certainty: Certainty::Declared, via: FILE.to_string() },
                );
            } else {
                warnings.push(format!("{FILE}: the link {} → {} names a block that was not found", l.from, l.to));
            }
        }
    }
}

/// Block `i` merged into `into`: its roots and manifests become `into`'s, and so will the links
/// found to it or from it.
fn merge(blocks: &mut Vec<Block>, i: usize, into: &str, gone: &mut Gone, warnings: &mut Vec<String>) {
    let Some(target) = gone.follow(into) else {
        warnings.push(format!("{FILE}: `{}` merges into `{into}`, which it ignores", blocks[i].id));
        return;
    };
    let Some(j) = blocks.iter().position(|b| b.id == target) else {
        warnings.push(format!("{FILE}: `{}` merges into `{into}`, which was not found", blocks[i].id));
        return;
    };
    if i == j {
        return;
    }
    let from = blocks.remove(i);
    let to = &mut blocks[if j > i { j - 1 } else { j }];
    for r in from.roots {
        if !to.roots.contains(&r) {
            to.roots.push(r);
        }
    }
    for m in from.manifests {
        if !to.manifests.contains(&m) {
            to.manifests.push(m);
        }
    }
    gone.0.insert(from.id, Some(target));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_format() {
        let c = Config::parse(
            r#"
version = 1
exclude = ["bench/"]

[blocks.forgust-server]
name = "API"
role = "api"
referents = ["@alice"]
jobs = ["rust-*"]

[blocks.forgust-diff]
merge_into = "forgust-server"

[blocks.site]
kind = "docs"
paths = ["site/"]

[[links]]
from = "forgust-web"
to = "forgust-server"

[[links]]
from = "image"
to = "forgust-bench"
remove = true

[people]
exclude = ["bot@example.com"]
days = 180
"#,
        )
        .unwrap();
        assert_eq!(c.blocks["forgust-server"].role, Some(Role::Api));
        assert_eq!(c.links[0].kind, None, "a link added calls, unless told");
        assert!(c.links[1].remove);
        assert_eq!(c.people.days, Some(180));
        assert!(Config::parse("[blocks.x]\nnmae = \"typo\"\n").is_err(), "an unknown key is an error, not ignored");
        assert!(Config::parse("version = 2\n").is_err());
    }

    #[test]
    fn follows_merges_to_their_end() {
        let mut gone = Gone::default();
        gone.0.insert("a".into(), Some("b".into()));
        gone.0.insert("b".into(), Some("c".into()));
        gone.0.insert("x".into(), None);
        assert_eq!(gone.follow("a").as_deref(), Some("c"));
        assert_eq!(gone.follow("c").as_deref(), Some("c"), "a block still there stands for itself");
        assert_eq!(gone.follow("x"), None, "ignored");
        let mut merged: Vec<(&str, String)> = gone.merged().collect();
        merged.sort();
        assert_eq!(merged, [("a", "c".to_string()), ("b", "c".to_string())]);
    }
}
