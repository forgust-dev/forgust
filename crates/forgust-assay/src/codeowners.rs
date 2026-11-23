//! CODEOWNERS, as GitHub and GitLab read it: a path's owners are those of the last line
//! matching it; with GitLab's `[Sections]`, every section gives its own.

use crate::files::Patterns;

/// Where it is looked for, the first one found wins.
pub const LOCATIONS: &[&str] = &["CODEOWNERS", ".github/CODEOWNERS", ".gitlab/CODEOWNERS", "docs/CODEOWNERS"];

pub struct Codeowners {
    /// Per section: its patterns, and the owners of each (none: the line unsets them).
    sections: Vec<(Patterns, Vec<Vec<String>>)>,
}

impl Codeowners {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut raw: Vec<Vec<(String, Vec<String>)>> = vec![Vec::new()];
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // `[Section]`, `^[Optional section][2] @default-owner`
            if line.starts_with('[') || line.starts_with("^[") {
                raw.push(Vec::new());
                continue;
            }
            let mut tokens = line.split_whitespace();
            let Some(pattern) = tokens.next() else { continue };
            let owners = tokens.take_while(|t| !t.starts_with('#')).map(str::to_string).collect();
            raw.last_mut().expect("a section").push((pattern.to_string(), owners));
        }
        let sections = raw
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| {
                let (patterns, owners): (Vec<String>, Vec<Vec<String>>) = s.into_iter().unzip();
                Ok((Patterns::new(patterns)?, owners))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(Self { sections })
    }

    /// The owners of one file.
    pub fn owners_of(&self, path: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (patterns, owners) in &self.sections {
            if let Some(last) = patterns.matching(path).into_iter().max() {
                for o in &owners[last] {
                    if !out.contains(o) {
                        out.push(o.clone());
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_line_matching_wins_in_each_section() {
        let c = Codeowners::parse("* @all\n/crates/ @rust\n*.md @docs # docs\n\n[Web]\n/web/ @front\n/web/vendor/\n").unwrap();
        assert_eq!(c.owners_of("crates/a/src/lib.rs"), vec!["@rust"]);
        assert_eq!(c.owners_of("crates/a/README.md"), vec!["@docs"]);
        assert_eq!(c.owners_of("web/src/app.ts"), vec!["@all", "@front"]);
        assert_eq!(c.owners_of("web/vendor/x.js"), vec!["@all"], "a line with no owners unsets them");
    }
}
