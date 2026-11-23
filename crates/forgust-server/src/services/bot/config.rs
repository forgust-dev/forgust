//! `.forgust/bot.toml` (ADR-0026): a project's rules for the bot, read
//! from the target branch of a merge request, the default branch for an issue.
//!
//! ```toml
//! # who may run a command, beyond those who could do the same from the page
//! [commands.label]
//! allow = ["author", "code_owners", "reporter"]
//!
//! [commands.close]
//! enabled = false
//!
//! # labels the bot puts on merge requests (it makes those the project lacks)
//! [labels.paths]            # a changed file matches a pattern (as in CODEOWNERS)
//! docs = ["*.md", "/docs/"]
//! "area/web" = ["/web/"]
//!
//! [labels.size]             # size/XS … size/XL, by lines added and removed
//! enabled = true
//! xs = 10                   # fewer than 10 lines: size/XS (these are the defaults)
//! s = 30
//! m = 100
//! l = 500
//!
//! [labels.title]            # the title's conventional-commit type
//! feat = "feature"
//! fix = "bug"
//! ```
//!
//! `allow` takes `anyone` (who can comment), `author` (of the issue or merge
//! request), `code_owners` (of a file the merge request changes) and roles
//! (`guest` … `owner`: that role and up). `/merge`, `/rebase` and `/retest`
//! take no `allow`: they run as the commenter, with their own rights.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::parse::Kind;

pub const PATH: &str = ".forgust/bot.toml";
/// A larger file is refused, not read in part.
pub const MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allow {
    Anyone,
    Author,
    CodeOwners,
    /// This role and up.
    Role(i16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRule {
    pub enabled: bool,
    pub allow: Vec<Allow>,
}

impl Default for CommandRule {
    fn default() -> Self {
        CommandRule { enabled: true, allow: Vec::new() }
    }
}

/// `size/…` by changed lines: fewer than `xs` is XS, …, `l` and more is XL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizes {
    pub xs: u32,
    pub s: u32,
    pub m: u32,
    pub l: u32,
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes { xs: 10, s: 30, m: 100, l: 500 }
    }
}

impl Sizes {
    pub fn label(&self, lines: u32) -> &'static str {
        match lines {
            n if n < self.xs => "size/XS",
            n if n < self.s => "size/S",
            n if n < self.m => "size/M",
            n if n < self.l => "size/L",
            _ => "size/XL",
        }
    }
}

/// The labels the bot puts on merge requests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelRules {
    /// Label → the patterns (CODEOWNERS') a changed file has to match.
    pub paths: BTreeMap<String, Vec<String>>,
    pub size: Option<Sizes>,
    /// Conventional-commit type of the title (`feat`) → label.
    pub title: BTreeMap<String, String>,
}

impl LabelRules {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.size.is_none() && self.title.is_empty()
    }
}

/// The rules of a project; the default is what applies with no file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BotConfig {
    commands: BTreeMap<Kind, CommandRule>,
    pub labels: LabelRules,
}

impl BotConfig {
    pub fn command(&self, kind: Kind) -> CommandRule {
        self.commands.get(&kind).cloned().unwrap_or_default()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    commands: BTreeMap<String, RawRule>,
    #[serde(default)]
    labels: RawLabels,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawLabels {
    #[serde(default)]
    paths: BTreeMap<String, Vec<String>>,
    size: Option<RawSizes>,
    #[serde(default)]
    title: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSizes {
    enabled: Option<bool>,
    xs: Option<u32>,
    s: Option<u32>,
    m: Option<u32>,
    l: Option<u32>,
}

/// At most this many labels by path, and patterns for one label.
const MAX_PATH_RULES: usize = 100;

/// A label the bot can make: what the labels' own page accepts.
fn label_name(table: &str, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 255 || name.contains(',') {
        return Err(format!("`[labels.{table}]`: `{name}` cannot be a label (1-255 characters, no comma)"));
    }
    Ok(name.to_string())
}

fn labels(raw: RawLabels) -> Result<LabelRules, String> {
    if raw.paths.len() > MAX_PATH_RULES {
        return Err(format!("`[labels.paths]`: at most {MAX_PATH_RULES} labels"));
    }
    let mut paths = BTreeMap::new();
    for (name, patterns) in raw.paths {
        let name = label_name("paths", &name)?;
        if patterns.is_empty() || patterns.len() > MAX_PATH_RULES {
            return Err(format!("`[labels.paths]`: `{name}` takes 1 to {MAX_PATH_RULES} patterns"));
        }
        if let Some(bad) = patterns.iter().find(|p| crate::services::codeowners::matchers(p).is_empty()) {
            return Err(format!("`[labels.paths]`: `{bad}` is not a pattern"));
        }
        paths.insert(name, patterns);
    }
    let size = match raw.size {
        Some(r) if r.enabled.unwrap_or(true) => {
            let d = Sizes::default();
            let z = Sizes { xs: r.xs.unwrap_or(d.xs), s: r.s.unwrap_or(d.s), m: r.m.unwrap_or(d.m), l: r.l.unwrap_or(d.l) };
            if !(z.xs < z.s && z.s < z.m && z.m < z.l) {
                return Err("`[labels.size]`: the sizes must grow (xs < s < m < l)".into());
            }
            Some(z)
        }
        _ => None,
    };
    let mut title = BTreeMap::new();
    for (kind, name) in raw.title {
        if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(format!("`[labels.title]`: `{kind}` is not a type (letters only, as in `feat: …`)"));
        }
        title.insert(kind.to_ascii_lowercase(), label_name("title", &name)?);
    }
    Ok(LabelRules { paths, size, title })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    enabled: Option<bool>,
    #[serde(default)]
    allow: Vec<String>,
}

/// The file's rules, or what is wrong with it, in words for its authors.
pub fn parse(text: &str) -> Result<BotConfig, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| e.message().to_string())?;
    let mut commands = BTreeMap::new();
    for (name, rule) in raw.commands {
        let kind = Kind::from_name(&name).ok_or_else(|| format!("`[commands.{name}]`: there is no `/{name}` command"))?;
        if kind.as_commenter() && !rule.allow.is_empty() {
            return Err(format!("`[commands.{name}]`: `/{name}` runs with the commenter's own rights, `allow` cannot widen it"));
        }
        let mut allow = Vec::new();
        for a in &rule.allow {
            allow.push(match a.as_str() {
                "anyone" => Allow::Anyone,
                "author" => Allow::Author,
                "code_owners" => Allow::CodeOwners,
                role => Allow::Role(forgust_db::access::parse(role).filter(|l| *l > 0).ok_or_else(|| {
                    format!("`[commands.{name}]`: `allow` takes anyone, author, code_owners or a role (guest, reporter, developer, maintainer, owner), not `{role}`")
                })?),
            });
        }
        commands.insert(kind, CommandRule { enabled: rule.enabled.unwrap_or(true), allow });
    }
    Ok(BotConfig { commands, labels: labels(raw.labels)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_per_command() {
        let c =
            parse("[commands.label]\nallow = [\"author\", \"code_owners\", \"reporter\"]\n[commands.close]\nenabled = false\n").unwrap();
        assert_eq!(
            c.command(Kind::Label),
            CommandRule { enabled: true, allow: vec![Allow::Author, Allow::CodeOwners, Allow::Role(forgust_db::access::REPORTER)] }
        );
        assert!(!c.command(Kind::Close).enabled);
        assert_eq!(c.command(Kind::Assign), CommandRule::default());
        assert_eq!(parse("").unwrap(), BotConfig::default());
    }

    #[test]
    fn label_rules() {
        let c = parse(
            "[labels.paths]\ndocs = [\"*.md\", \"/docs/\"]\n[labels.size]\nl = 400\n[labels.title]\nFeat = \"feature\"\nfix = \"bug\"\n",
        )
        .unwrap();
        assert_eq!(c.labels.paths["docs"], vec!["*.md", "/docs/"]);
        assert_eq!(c.labels.size, Some(Sizes { xs: 10, s: 30, m: 100, l: 400 }));
        assert_eq!(c.labels.title["feat"], "feature");
        let z = Sizes::default();
        assert_eq!(
            [0, 9, 10, 29, 99, 100, 499, 500].map(|n| z.label(n)),
            ["size/XS", "size/XS", "size/S", "size/S", "size/M", "size/L", "size/L", "size/XL"]
        );
        // no file, or a table that says no: no labels
        assert!(parse("").unwrap().labels.is_empty());
        assert!(parse("[labels.size]\nenabled = false").unwrap().labels.is_empty());
        assert_eq!(parse("[labels.size]").unwrap().labels.size, Some(z));
    }

    #[test]
    fn mistakes_are_named() {
        for (text, says) in [
            ("[commands.lable]\nallow = []", "no `/lable` command"),
            ("[commands.label]\nallow = [\"everyone\"]", "not `everyone`"),
            ("[commands.merge]\nallow = [\"anyone\"]", "own rights"),
            ("[commands.label]\nalow = []", "unknown field"),
            ("[labels]\nx = 1", "unknown field"),
            ("[labels.paths]\ndocs = []", "takes 1 to"),
            ("[labels.paths]\n\"a,b\" = [\"*.md\"]", "cannot be a label"),
            ("[labels.paths]\ndocs = [\"/\"]", "is not a pattern"),
            ("[labels.size]\nxs = 50", "must grow"),
            ("[labels.title]\n\"feat!\" = \"x\"", "is not a type"),
            ("[commands", "invalid"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(says), "{text:?}: {e}");
        }
        // a command that runs as the commenter can still be turned off
        assert!(!parse("[commands.merge]\nenabled = false").unwrap().command(Kind::Merge).enabled);
    }
}
