//! The rules between blocks that assay.toml sets (`[[rules]]`: these blocks must not need those),
//! checked on the map: each link that breaks one is a [`Violation`].

use globset::{Glob, GlobMatcher};

use crate::config::{FILE, RuleConfig};
use crate::model::{Block, Certainty, Kind, Link, Role, Violation};

/// What a rule names blocks by: an id or a glob of ids (`acme-*`), `role:<role>`, `kind:<kind>`.
enum Selector {
    Ids(GlobMatcher),
    Role(Role),
    Kind(Kind),
}

impl Selector {
    fn parse(s: &str) -> Result<Self, String> {
        if let Some(r) = s.strip_prefix("role:") {
            return named("role", r).map(Selector::Role);
        }
        if let Some(k) = s.strip_prefix("kind:") {
            return named("kind", k).map(Selector::Kind);
        }
        Glob::new(s).map(|g| Selector::Ids(g.compile_matcher())).map_err(|e| e.to_string())
    }

    fn matches(&self, b: &Block) -> bool {
        match self {
            Selector::Ids(g) => g.is_match(&b.id),
            Selector::Role(r) => b.role == Some(*r),
            Selector::Kind(k) => b.kind == *k,
        }
    }
}

/// A role's or a kind's name, as assay.toml and the JSON give it.
fn named<T: serde::de::DeserializeOwned>(what: &str, v: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(v.to_string())).map_err(|_| format!("no {what} `{v}`"))
}

/// The links that break a rule, in the order of the rules then of the links; a selector that
/// cannot be read is a warning, and its rule is not checked.
pub(crate) fn check(rules: &[RuleConfig], blocks: &[Block], links: &[Link], warnings: &mut Vec<String>) -> Vec<Violation> {
    let mut out = Vec::new();
    for (i, rule) in rules.iter().enumerate() {
        let parse = |list: &[String]| list.iter().map(|s| Selector::parse(s)).collect::<Result<Vec<_>, _>>();
        let (from, deny) = match (parse(&rule.from), parse(&rule.deny)) {
            (Ok(f), Ok(d)) => (f, d),
            (Err(e), _) | (_, Err(e)) => {
                warnings.push(format!("{FILE}: rule {}: {e}", i + 1));
                continue;
            }
        };
        let is = |id: &str, sel: &[Selector]| blocks.iter().find(|b| b.id == id).is_some_and(|b| sel.iter().any(|s| s.matches(b)));
        for l in links.iter().filter(|l| rule.guesses || l.certainty == Certainty::Declared) {
            if is(&l.from, &from) && is(&l.to, &deny) {
                out.push(Violation {
                    rule: i + 1,
                    why: rule.why.clone(),
                    from: l.from.clone(),
                    to: l.to.clone(),
                    kind: l.kind,
                    certainty: l.certainty,
                    via: l.via.clone(),
                });
            }
        }
    }
    out
}
