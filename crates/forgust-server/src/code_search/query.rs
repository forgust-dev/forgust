//! What a code search asks for: the trigrams a file must contain to be worth
//! reading (`Need`), and the matcher that then checks its lines.
//!
//! The index stores the trigrams of each file's lower-cased text, cut inside
//! lines. A plain text query needs all of its own trigrams. For a regular
//! expression we walk its syntax tree: a literal needs its trigrams, a
//! sequence needs what every part needs, an alternation what one branch
//! needs, and anything else (classes, optional parts) needs nothing.

use std::collections::BTreeSet;

use regex_syntax::hir::{Hir, HirKind};

/// Shortest text the index can filter on.
pub const MIN_CHARS: usize = 3;
/// Longest query text.
pub const MAX_CHARS: usize = 500;
/// Compiled size of a regular expression.
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// Trigrams asked of the index for one literal: more would not filter better.
const MAX_GRAMS: usize = 24;

/// Trigrams a file must contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Need {
    /// Nothing known: every file may match.
    Any,
    /// All of these trigrams.
    Grams(Vec<String>),
    All(Vec<Need>),
    OneOf(Vec<Need>),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QueryError {
    #[error("search for at least {MIN_CHARS} characters")]
    TooShort,
    #[error("the search text is too long (at most {MAX_CHARS} characters)")]
    TooLong,
    #[error("the search text cannot contain a line break")]
    LineBreak,
    #[error("not a valid regular expression: {0}")]
    BadRegex(String),
    #[error("the regular expression must contain at least {MIN_CHARS} characters of plain text that every match has")]
    NoLiteral,
}

/// The text as the index sees it.
pub fn fold(text: &str) -> String {
    text.to_lowercase()
}

/// The distinct trigrams of already folded text, cut inside lines.
pub fn grams(folded: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    for line in folded.split(['\n', '\r']) {
        // byte offsets of the last three characters seen
        let (mut a, mut b) = (usize::MAX, usize::MAX);
        for (i, c) in line.char_indices() {
            if a != usize::MAX {
                out.insert(&line[a..i + c.len_utf8()]);
            }
            a = b;
            b = i;
        }
    }
    out
}

fn literal_need(text: &str) -> Need {
    let folded = fold(text);
    let all = grams(&folded);
    if all.is_empty() {
        return Need::Any;
    }
    // spread the ones we keep over the whole literal
    let step = all.len().div_ceil(MAX_GRAMS);
    Need::Grams(all.into_iter().step_by(step).map(str::to_string).collect())
}

fn hir_need(h: &Hir) -> Need {
    match h.kind() {
        HirKind::Literal(l) => std::str::from_utf8(&l.0).map(literal_need).unwrap_or(Need::Any),
        HirKind::Capture(c) => hir_need(&c.sub),
        HirKind::Repetition(r) if r.min >= 1 => hir_need(&r.sub),
        HirKind::Concat(parts) => {
            let needs: Vec<Need> = parts.iter().map(hir_need).filter(|n| *n != Need::Any).collect();
            match needs.len() {
                0 => Need::Any,
                1 => needs.into_iter().next().unwrap(),
                _ => Need::All(needs),
            }
        }
        HirKind::Alternation(branches) => {
            let needs: Vec<Need> = branches.iter().map(hir_need).collect();
            if needs.contains(&Need::Any) { Need::Any } else { Need::OneOf(needs) }
        }
        _ => Need::Any,
    }
}

/// Checks the lines of a candidate file.
#[derive(Debug, Clone)]
pub struct Matcher(regex::Regex);

impl Matcher {
    /// Byte ranges of the matches in one line (without its line break).
    pub fn find(&self, line: &str) -> Vec<(usize, usize)> {
        self.0.find_iter(line).filter(|m| !m.is_empty()).map(|m| (m.start(), m.end())).collect()
    }
}

/// Turn the search text into what to ask the index and how to check a line.
pub fn plan(text: &str, is_regex: bool, case_sensitive: bool) -> Result<(Need, Matcher), QueryError> {
    if text.chars().count() > MAX_CHARS {
        return Err(QueryError::TooLong);
    }
    if text.contains(['\n', '\r']) {
        return Err(QueryError::LineBreak);
    }
    let (pattern, need) = if is_regex {
        // parsed without the case switch, so literals stay literals: the index
        // is folded anyway, and an inline `(?i)` only makes us filter less
        let hir = regex_syntax::ParserBuilder::new().build().parse(text).map_err(|e| QueryError::BadRegex(e.to_string()))?;
        let need = hir_need(&hir);
        if need == Need::Any {
            return Err(QueryError::NoLiteral);
        }
        (text.to_string(), need)
    } else {
        if text.chars().count() < MIN_CHARS {
            return Err(QueryError::TooShort);
        }
        (regex::escape(text), literal_need(text))
    };
    let regex = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => QueryError::BadRegex("it is too large".into()),
            e => QueryError::BadRegex(e.to_string()),
        })?;
    Ok((need, Matcher(regex)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(items: &[&str]) -> Need {
        Need::Grams(items.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn trigrams_stay_inside_lines_and_are_distinct() {
        let got: Vec<&str> = grams("abcd\nab\nxyz abc").into_iter().collect();
        assert_eq!(got, vec![" ab", "abc", "bcd", "xyz", "yz ", "z a"]);
        assert!(grams("ab").is_empty());
        // characters, not bytes
        assert_eq!(grams("héé!").into_iter().collect::<Vec<_>>(), vec!["héé", "éé!"]);
    }

    #[test]
    fn plain_text_needs_its_folded_trigrams() {
        let (need, m) = plan("Fn Main", false, false).unwrap();
        assert_eq!(need, g(&[" ma", "ain", "fn ", "mai", "n m"]));
        assert_eq!(m.find("pub fn main() { FN MAIN }"), vec![(4, 11), (16, 23)]);
        let (_, exact) = plan("Fn Main", false, true).unwrap();
        assert!(exact.find("pub fn main()").is_empty());
        // regex characters are plain text here
        let (_, dots) = plan("a.b(", false, false).unwrap();
        assert_eq!(dots.find("xa.b( aXb("), vec![(1, 5)]);
    }

    #[test]
    fn long_text_asks_for_a_bounded_number_of_trigrams() {
        let text: String = ('a'..='z').cycle().take(400).collect::<String>() + "0123456789";
        let (Need::Grams(grams), _) = plan(&text, false, false).unwrap() else { panic!() };
        assert!(grams.len() <= MAX_GRAMS, "{}", grams.len());
    }

    #[test]
    fn refused_queries() {
        assert_eq!(plan("ab", false, false).unwrap_err(), QueryError::TooShort);
        assert_eq!(plan("a\nbc", false, false).unwrap_err(), QueryError::LineBreak);
        assert_eq!(plan(&"x".repeat(MAX_CHARS + 1), false, false).unwrap_err(), QueryError::TooLong);
        assert_eq!(plan(r"\w+", true, false).unwrap_err(), QueryError::NoLiteral);
        assert_eq!(plan("ab.*cd", true, false).unwrap_err(), QueryError::NoLiteral);
        assert_eq!(plan("(foo)?bar|x", true, false).unwrap_err(), QueryError::NoLiteral);
        assert!(matches!(plan("foo(", true, false).unwrap_err(), QueryError::BadRegex(_)));
        assert!(matches!(plan(r"foo\d{1000}{1000}", true, false).unwrap_err(), QueryError::BadRegex(_)));
    }

    #[test]
    fn a_regex_needs_the_trigrams_of_its_required_literals() {
        assert_eq!(plan(r"Foo\d+", true, false).unwrap().0, g(&["foo"]));
        assert_eq!(plan(r"\w+Error\b", true, false).unwrap().0, g(&["err", "ror", "rro"]));
        assert_eq!(plan(r"foo.*(bar)+", true, false).unwrap().0, Need::All(vec![g(&["foo"]), g(&["bar"])]));
        assert_eq!(plan(r"foo|barx?", true, false).unwrap().0, Need::OneOf(vec![g(&["foo"]), g(&["bar"])]));
        // an optional part needs nothing, the rest still filters
        assert_eq!(plan(r"(abc)?def", true, false).unwrap().0, g(&["def"]));
        // an inline flag turns literals into classes: only the rest filters
        assert_eq!(plan(r"(?i:abc)def", true, false).unwrap().0, g(&["def"]));
        let (_, m) = plan(r"food+\d", true, true).unwrap();
        assert_eq!(m.find("foodd1 FOOD2"), vec![(0, 6)]);
        let (_, m) = plan(r"food+\d", true, false).unwrap();
        assert_eq!(m.find("foodd1 FOOD2"), vec![(0, 6), (7, 12)]);
        // too little plain text in one piece to filter on
        assert_eq!(plan(r"fo+\d", true, false).unwrap_err(), QueryError::NoLiteral);
    }
}
