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

/// The rules of a project; the default is what applies with no file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BotConfig {
    commands: BTreeMap<Kind, CommandRule>,
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
    Ok(BotConfig { commands })
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
    fn mistakes_are_named() {
        for (text, says) in [
            ("[commands.lable]\nallow = []", "no `/lable` command"),
            ("[commands.label]\nallow = [\"everyone\"]", "not `everyone`"),
            ("[commands.merge]\nallow = [\"anyone\"]", "own rights"),
            ("[commands.label]\nalow = []", "unknown field"),
            ("[labels]\nx = 1", "unknown field"),
            ("[commands", "invalid"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(says), "{text:?}: {e}");
        }
        // a command that runs as the commenter can still be turned off
        assert!(!parse("[commands.merge]\nenabled = false").unwrap().command(Kind::Merge).enabled);
    }
}
