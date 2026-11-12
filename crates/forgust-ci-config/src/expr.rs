//! GitLab CI/CD expressions (`rules:if`, `only:variables`, `workflow:rules`):
//! `$VAR`, string and regex literals, `null`, `==`, `!=`, `=~`, `!~`, `&&`,
//! `||` and parentheses. `&&` binds tighter than `||`.

use std::collections::HashMap;

use regex::RegexBuilder;

use crate::ConfigError;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Var(String),
    Str(String),
    Re(String, String),
    Null,
    Eq,
    Ne,
    Match,
    NoMatch,
    And,
    Or,
    LParen,
    RParen,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let two: String = b[i..(i + 2).min(b.len())].iter().collect();
        match two.as_str() {
            "==" => {
                out.push(Tok::Eq);
                i += 2;
                continue;
            }
            "!=" => {
                out.push(Tok::Ne);
                i += 2;
                continue;
            }
            "=~" => {
                out.push(Tok::Match);
                i += 2;
                continue;
            }
            "!~" => {
                out.push(Tok::NoMatch);
                i += 2;
                continue;
            }
            "&&" => {
                out.push(Tok::And);
                i += 2;
                continue;
            }
            "||" => {
                out.push(Tok::Or);
                i += 2;
                continue;
            }
            _ => {}
        }
        match c {
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '$' => {
                i += 1;
                let braced = i < b.len() && b[i] == '{';
                if braced {
                    i += 1;
                }
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                    i += 1;
                }
                if start == i {
                    return Err("`$` must be followed by a variable name".into());
                }
                let name: String = b[start..i].iter().collect();
                if braced {
                    if i >= b.len() || b[i] != '}' {
                        return Err("unclosed `${`".into());
                    }
                    i += 1;
                }
                out.push(Tok::Var(name));
            }
            '"' | '\'' => {
                let q = c;
                i += 1;
                let mut v = String::new();
                while i < b.len() && b[i] != q {
                    if b[i] == '\\' && i + 1 < b.len() && b[i + 1] == q {
                        i += 1;
                    }
                    v.push(b[i]);
                    i += 1;
                }
                if i >= b.len() {
                    return Err("unterminated string".into());
                }
                i += 1;
                out.push(Tok::Str(v));
            }
            '/' => {
                i += 1;
                let mut v = String::new();
                while i < b.len() && b[i] != '/' {
                    if b[i] == '\\' && i + 1 < b.len() && b[i + 1] == '/' {
                        v.push('/');
                        i += 2;
                        continue;
                    }
                    v.push(b[i]);
                    i += 1;
                }
                if i >= b.len() {
                    return Err("unterminated regex".into());
                }
                i += 1;
                let mut flags = String::new();
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    flags.push(b[i]);
                    i += 1;
                }
                out.push(Tok::Re(v, flags));
            }
            'n' if b[i..].starts_with(&['n', 'u', 'l', 'l']) && b.get(i + 4).is_none_or(|c| !c.is_ascii_alphanumeric()) => {
                out.push(Tok::Null);
                i += 4;
            }
            _ => return Err(format!("unexpected `{c}`")),
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
enum Expr {
    Var(String),
    Str(String),
    Re(String, String),
    Null,
    Cmp(Box<Expr>, Tok, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

/// Parsing and evaluating recurse: a pushed config must not overflow the stack.
const MAX_TOKENS: usize = 1024;
const MAX_DEPTH: usize = 32;

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.cmp()?;
        while self.peek() == Some(&Tok::And) {
            self.pos += 1;
            l = Expr::And(Box::new(l), Box::new(self.cmp()?));
        }
        Ok(l)
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.atom()?;
        match self.peek() {
            Some(t @ (Tok::Eq | Tok::Ne | Tok::Match | Tok::NoMatch)) => {
                let op = t.clone();
                self.pos += 1;
                let r = self.atom()?;
                Ok(Expr::Cmp(Box::new(l), op, Box::new(r)))
            }
            _ => Ok(l),
        }
    }
    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Var(v)) => Ok(Expr::Var(v)),
            Some(Tok::Str(s)) => Ok(Expr::Str(s)),
            Some(Tok::Re(r, f)) => Ok(Expr::Re(r, f)),
            Some(Tok::Null) => Ok(Expr::Null),
            Some(Tok::LParen) => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err("too many nested parentheses".into());
                }
                let e = self.or()?;
                self.depth -= 1;
                if self.next() != Some(Tok::RParen) {
                    return Err("missing `)`".into());
                }
                Ok(e)
            }
            Some(t) => Err(format!("unexpected {t:?}")),
            None => Err("unexpected end of expression".into()),
        }
    }
}

/// A parsed expression, ready to evaluate against variables.
#[derive(Debug, Clone)]
pub struct Expression {
    src: String,
    root: Expr,
}

impl Expression {
    pub fn parse(src: &str) -> Result<Expression, ConfigError> {
        let toks = tokenize(src).map_err(|e| ConfigError::Invalid(format!("invalid expression `{src}`: {e}")))?;
        if toks.len() > MAX_TOKENS {
            return Err(ConfigError::Invalid(format!("invalid expression: longer than {MAX_TOKENS} terms")));
        }
        let mut p = Parser { toks, pos: 0, depth: 0 };
        let root = p.or().map_err(|e| ConfigError::Invalid(format!("invalid expression `{src}`: {e}")))?;
        if p.pos != p.toks.len() {
            return Err(ConfigError::Invalid(format!("invalid expression `{src}`: unexpected trailing input")));
        }
        Ok(Expression { src: src.to_string(), root })
    }

    pub fn source(&self) -> &str {
        &self.src
    }

    pub fn eval(&self, vars: &HashMap<String, String>) -> bool {
        truthy(&eval(&self.root, vars))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Str(String),
    Re(String, String),
    Bool(bool),
}

fn truthy(v: &Val) -> bool {
    match v {
        Val::Null => false,
        Val::Str(s) => !s.is_empty(),
        Val::Re(..) => true,
        Val::Bool(b) => *b,
    }
}

/// Interpret the parsed tree (a small evaluator over our own AST; nothing is executed).
fn eval(e: &Expr, vars: &HashMap<String, String>) -> Val {
    match e {
        Expr::Var(v) => vars.get(v).map(|s| Val::Str(s.clone())).unwrap_or(Val::Null),
        Expr::Str(s) => Val::Str(s.clone()),
        Expr::Re(r, f) => Val::Re(r.clone(), f.clone()),
        Expr::Null => Val::Null,
        Expr::And(a, b) => Val::Bool(truthy(&eval(a, vars)) && truthy(&eval(b, vars))),
        Expr::Or(a, b) => Val::Bool(truthy(&eval(a, vars)) || truthy(&eval(b, vars))),
        Expr::Cmp(a, op, b) => {
            let (l, r) = (eval(a, vars), eval(b, vars));
            let res = match op {
                Tok::Eq => equal(&l, &r),
                Tok::Ne => !equal(&l, &r),
                Tok::Match => matches(&l, &r),
                Tok::NoMatch => !matches(&l, &r),
                _ => false,
            };
            Val::Bool(res)
        }
    }
}

fn equal(l: &Val, r: &Val) -> bool {
    match (l, r) {
        (Val::Null, Val::Null) => true,
        (Val::Null, _) | (_, Val::Null) => false,
        (Val::Str(a), Val::Str(b)) => a == b,
        _ => l == r,
    }
}

/// The right side is a regex literal, or a variable holding `/pattern/flags`.
fn matches(l: &Val, r: &Val) -> bool {
    let Val::Str(text) = l else { return false };
    let (pattern, flags) = match r {
        Val::Re(p, f) => (p.clone(), f.clone()),
        Val::Str(s) if s.starts_with('/') && s.len() > 1 => match s[1..].rfind('/') {
            Some(end) => (s[1..end + 1].to_string(), s[end + 2..].to_string()),
            None => return false,
        },
        _ => return false,
    };
    let pattern = if flags.contains('i') { format!("(?i){pattern}") } else { pattern };
    // linear-time engine (no backtracking); cap compiled size against huge patterns
    match RegexBuilder::new(&pattern).size_limit(1 << 20).build() {
        Ok(re) => re.is_match(text),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn e(s: &str, v: &HashMap<String, String>) -> bool {
        Expression::parse(s).unwrap().eval(v)
    }

    #[test]
    fn deep_or_long_expressions_are_refused() {
        assert!(Expression::parse(&("(".repeat(100_000) + "$A" + &")".repeat(100_000))).is_err());
        assert!(Expression::parse(&vec!["$A"; 100_000].join(" || ")).is_err());
        assert!(Expression::parse(&("(".repeat(MAX_DEPTH) + "$A" + &")".repeat(MAX_DEPTH))).is_ok());
    }

    #[test]
    fn comparisons_and_presence() {
        let v = vars(&[("CI_COMMIT_BRANCH", "main"), ("CI_PIPELINE_SOURCE", "push"), ("EMPTY", "")]);
        assert!(e("$CI_COMMIT_BRANCH == \"main\"", &v));
        assert!(e("$CI_COMMIT_BRANCH != 'dev'", &v));
        assert!(e("$CI_COMMIT_BRANCH", &v));
        assert!(!e("$EMPTY", &v));
        assert!(!e("$CI_COMMIT_TAG", &v));
        assert!(e("$CI_COMMIT_TAG == null", &v));
        assert!(!e("$CI_COMMIT_TAG == ''", &v), "undefined is not the empty string");
        assert!(e("${CI_COMMIT_BRANCH} == 'main'", &v));
    }

    #[test]
    fn regexes() {
        let v = vars(&[("CI_COMMIT_BRANCH", "release/1.2"), ("PATTERN", "/^RELEASE\\//i")]);
        assert!(e("$CI_COMMIT_BRANCH =~ /^release\\/\\d+/", &v));
        assert!(e("$CI_COMMIT_BRANCH =~ /^RELEASE/i", &v));
        assert!(e("$CI_COMMIT_BRANCH !~ /^main$/", &v));
        assert!(e("$CI_COMMIT_BRANCH =~ $PATTERN", &v), "regex from a variable");
        assert!(!e("$MISSING =~ /x/", &v));
    }

    #[test]
    fn boolean_logic() {
        let v = vars(&[("A", "1"), ("B", "")]);
        assert!(e("$A && ($B || $A == '1')", &v));
        assert!(e("$B || $A", &v));
        assert!(!e("$A && $B", &v));
        assert!(e("$B && $A || $A", &v), "&& before ||");
    }

    #[test]
    fn syntax_errors() {
        assert!(Expression::parse("$A ==").is_err());
        assert!(Expression::parse("($A").is_err());
        assert!(Expression::parse("$A = 'x'").is_err());
        assert!(Expression::parse("'open").is_err());
    }
}
