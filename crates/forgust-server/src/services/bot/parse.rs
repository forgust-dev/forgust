//! Commands in a comment (ADR-0026): a line that starts with `/name`,
//! outside code blocks and quotes. Arguments are written as in GitLab's quick
//! actions (`@user`, `me`, `~label`, `~"two words"`, `%milestone`); bare
//! names work too.

/// At most this many commands of one comment are run.
pub const MAX_COMMANDS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Assign,
    Unassign,
    Label,
    Unlabel,
    Close,
    Reopen,
    Milestone,
    RemoveMilestone,
    Cc,
    Hold,
    Unhold,
    Retest,
    Merge,
    Rebase,
}

impl Kind {
    pub const ALL: [Kind; 14] = [
        Kind::Assign,
        Kind::Unassign,
        Kind::Label,
        Kind::Unlabel,
        Kind::Close,
        Kind::Reopen,
        Kind::Milestone,
        Kind::RemoveMilestone,
        Kind::Cc,
        Kind::Hold,
        Kind::Unhold,
        Kind::Retest,
        Kind::Merge,
        Kind::Rebase,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Assign => "assign",
            Kind::Unassign => "unassign",
            Kind::Label => "label",
            Kind::Unlabel => "unlabel",
            Kind::Close => "close",
            Kind::Reopen => "reopen",
            Kind::Milestone => "milestone",
            Kind::RemoveMilestone => "remove_milestone",
            Kind::Cc => "cc",
            Kind::Hold => "hold",
            Kind::Unhold => "unhold",
            Kind::Retest => "retest",
            Kind::Merge => "merge",
            Kind::Rebase => "rebase",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// How its arguments are written, for the composer's suggestions.
    pub fn arguments(self) -> &'static str {
        match self {
            Kind::Assign | Kind::Cc => "@user …",
            Kind::Unassign => "[@user …]",
            Kind::Label => "~label …",
            Kind::Unlabel => "[~label …]",
            Kind::Milestone => "%milestone",
            _ => "",
        }
    }

    /// What it does, in a few words.
    pub fn about(self) -> &'static str {
        match self {
            Kind::Assign => "Assign people (no name: yourself)",
            Kind::Unassign => "Remove assignees (no name: all of them)",
            Kind::Label => "Add labels",
            Kind::Unlabel => "Remove labels (no name: all of them)",
            Kind::Close => "Close",
            Kind::Reopen => "Reopen",
            Kind::Milestone => "Set the milestone",
            Kind::RemoveMilestone => "Remove the milestone",
            Kind::Cc => "Ask people to review",
            Kind::Hold => "Put on hold: it does not merge until /unhold",
            Kind::Unhold => "Take off hold",
            Kind::Retest => "Run the failed jobs of the latest pipeline again",
            Kind::Merge => "Merge when every check passes (or add to the merge train)",
            Kind::Rebase => "Rebase the source branch onto the target",
        }
    }

    /// Makes commits or pipelines in someone's name: run as the commenter,
    /// with their own rights, which `.forgust/bot.toml` cannot widen.
    pub fn as_commenter(self) -> bool {
        matches!(self, Kind::Retest | Kind::Merge | Kind::Rebase)
    }

    /// Makes sense on merge requests only.
    pub fn merge_request_only(self) -> bool {
        matches!(self, Kind::Cc | Kind::Hold | Kind::Unhold | Kind::Retest | Kind::Merge | Kind::Rebase)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub kind: Kind,
    /// Its arguments, sigils and quotes removed (`~"needs info"` → `needs info`).
    pub args: Vec<String>,
}

/// A line that looked like a command and is none: `/frobnicate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unknown(pub String);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub commands: Vec<Command>,
    pub unknown: Vec<Unknown>,
    /// Commands past [`MAX_COMMANDS`], left out.
    pub dropped: usize,
}

impl Parsed {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.unknown.is_empty()
    }
}

/// The commands of a comment, in order.
pub fn parse(body: &str) -> Parsed {
    let mut out = Parsed::default();
    // the fence a code block was opened with (its character and length)
    let mut fence: Option<(char, usize)> = None;
    for line in body.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let text = line.trim();
        if let Some((c, n)) = fence_of(text) {
            match fence {
                None => fence = Some((c, n)),
                // closed by as long a fence of the same character, alone on its line
                Some((open, len)) if c == open && n >= len && text.trim_start_matches(c).trim().is_empty() => fence = None,
                Some(_) => {}
            }
            continue;
        }
        // code (fenced, or indented by four), and paths: `/usr/bin` is no command
        if fence.is_some() || indent >= 4 || line.starts_with('\t') {
            continue;
        }
        let Some(rest) = text.strip_prefix('/') else { continue };
        let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if name.is_empty()
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !name.starts_with(|c: char| c.is_ascii_alphabetic())
        {
            continue;
        }
        match Kind::from_name(&name.to_ascii_lowercase()) {
            Some(kind) if out.commands.len() < MAX_COMMANDS => out.commands.push(Command { kind, args: arguments(args) }),
            Some(_) => out.dropped += 1,
            None if out.unknown.len() < MAX_COMMANDS => out.unknown.push(Unknown(name.to_string())),
            None => {}
        }
    }
    out
}

/// "```" or "~~~" (or longer) opening or closing a code block.
fn fence_of(text: &str) -> Option<(char, usize)> {
    let c = text.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = text.chars().take_while(|x| *x == c).count();
    (n >= 3).then_some((c, n))
}

/// Split on spaces and commas; `"..."` keeps its spaces; a leading `@`, `~` or
/// `%` (before the quote too: `~"a b"`) is dropped.
fn arguments(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || c == ',' {
            chars.next();
            continue;
        }
        if matches!(c, '@' | '~' | '%') {
            chars.next();
        }
        let mut arg = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                arg.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == ',' {
                    break;
                }
                arg.push(c);
                chars.next();
            }
        }
        let arg = arg.trim();
        if !arg.is_empty() {
            out.push(arg.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(kind: Kind, args: &[&str]) -> Command {
        Command { kind, args: args.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn gitlab_arguments() {
        let p = parse("Thanks!\n/assign @alice me\n/label ~bug ~\"needs info\", ui\n/milestone %\"1.0 beta\"\n/close");
        assert_eq!(
            p.commands,
            vec![
                cmd(Kind::Assign, &["alice", "me"]),
                cmd(Kind::Label, &["bug", "needs info", "ui"]),
                cmd(Kind::Milestone, &["1.0 beta"]),
                cmd(Kind::Close, &[]),
            ]
        );
        assert!(p.unknown.is_empty());
    }

    #[test]
    fn only_lines_that_start_with_a_command() {
        let p = parse("see /label in the docs\n  /HOLD\n> /close\n- /reopen\n/usr/bin/env\n/ merge\n/9lives");
        assert_eq!(p.commands, vec![cmd(Kind::Hold, &[])]);
        assert!(p.unknown.is_empty(), "paths and quotes are no commands: {:?}", p.unknown);
    }

    #[test]
    fn code_is_left_alone() {
        let p = parse("```sh\n/close\n```\n    /merge\n\t/merge\n~~~~\n/rebase\n```\n/retest\n~~~~\n/unhold");
        assert_eq!(p.commands, vec![cmd(Kind::Unhold, &[])]);
    }

    #[test]
    fn unknown_commands_are_reported_and_floods_cut() {
        let p = parse("/frobnicate now\n/lgtm");
        assert_eq!(p.unknown, vec![Unknown("frobnicate".into()), Unknown("lgtm".into())]);
        let p = parse(&"/hold\n".repeat(MAX_COMMANDS + 3));
        assert_eq!((p.commands.len(), p.dropped), (MAX_COMMANDS, 3));
    }

    #[test]
    fn every_command_has_its_name() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_name(k.name()), Some(k));
        }
    }
}
