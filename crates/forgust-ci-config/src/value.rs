//! A YAML document as an ordered tree, with GitLab's `!reference` tag kept as
//! its own node and YAML merge keys (`<<`) applied.

use indexmap::IndexMap;
use saphyr::{Scalar, Yaml};

use crate::ConfigError;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Seq(Vec<Value>),
    Map(IndexMap<String, Value>),
    /// `!reference [job, key, ...]`, resolved after includes and `extends`.
    Ref(Vec<String>),
}

pub type Map = IndexMap<String, Value>;

impl Value {
    pub fn as_map(&self) -> Option<&Map> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Scalars as GitLab stringifies them (`1` → "1", `true` → "true").
    pub fn scalar_string(&self) -> Option<String> {
        Some(match self {
            Value::Str(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => {
                if f.fract() == 0.0 && f.abs() < 1e15 {
                    format!("{f:.1}")
                } else {
                    f.to_string()
                }
            }
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
            _ => return None,
        })
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Int(_) | Value::Float(_) => "number",
            Value::Str(_) => "string",
            Value::Seq(_) => "array",
            Value::Map(_) => "hash",
            Value::Ref(_) => "!reference",
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => (*b).into(),
            Value::Int(i) => (*i).into(),
            Value::Float(f) => serde_json::Number::from_f64(*f).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null),
            Value::Str(s) => s.clone().into(),
            Value::Seq(v) => serde_json::Value::Array(v.iter().map(Value::to_json).collect()),
            Value::Map(m) => serde_json::Value::Object(m.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
            Value::Ref(r) => serde_json::json!({ "!reference": r }),
        }
    }
}

/// Deeper than this and a document is refused before it is loaded: loading,
/// converting and dropping the tree all recurse.
const MAX_DEPTH: usize = 64;

/// How deep `source` nests, from the parser's events (no tree is built).
fn depth_ok(source: &str) -> Result<(), String> {
    use saphyr_parser::{Event, Parser};
    let mut depth = 0usize;
    for ev in Parser::new_from_str(source) {
        match ev.map_err(|e| e.to_string())?.0 {
            Event::SequenceStart(..) | Event::MappingStart(..) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(format!("nested deeper than {MAX_DEPTH} levels"));
                }
            }
            Event::SequenceEnd | Event::MappingEnd => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// The YAML documents of `source`. A key repeated in a mapping keeps its last
/// value, as GitLab's YAML parser (Ruby's Psych) does: real configs have, for
/// example, two top-level `variables:` blocks, and run on GitLab.
fn load_documents(source: &str) -> Result<Vec<Yaml<'_>>, saphyr::ScanError> {
    let mut parser = saphyr_parser::Parser::new_from_str(source);
    let mut loader = saphyr::YamlLoader::default();
    loader.allow_duplicate_keys(true);
    parser.load(&mut loader, true)?;
    if let Some(e) = loader.error() {
        return Err(e.clone());
    }
    Ok(loader.into_documents())
}

/// Parse one YAML document (GitLab reads only the first; a `spec:` header
/// document for inputs is not supported).
pub fn parse(source: &str, file: &str) -> Result<Value, ConfigError> {
    let (spec, doc) = parse_with_spec(source, file)?;
    match spec {
        // inputs nobody gave: their defaults
        Some(spec) => crate::inputs::interpolate(doc, &spec, &Map::new(), file),
        None => Ok(doc),
    }
}

/// A config with its optional `spec:` header document (CI/CD inputs): the
/// header's `spec` hash, and the document itself (not interpolated yet).
pub fn parse_with_spec(source: &str, file: &str) -> Result<(Option<Map>, Value), ConfigError> {
    depth_ok(source).map_err(|e| ConfigError::Yaml(format!("{file}: {e}")))?;
    let docs = load_documents(source).map_err(|e| ConfigError::Yaml(format!("{file}: {e}")))?;
    let top = |d: Option<&Yaml>| -> Result<Value, ConfigError> {
        match d {
            None => Ok(Value::Map(Map::new())),
            Some(d) => match convert(d, file)? {
                v @ Value::Map(_) => Ok(v),
                Value::Null => Ok(Value::Map(Map::new())),
                other => Err(ConfigError::Yaml(format!("{file}: the top level must be a hash, not a {}", other.type_name()))),
            },
        }
    };
    match docs.len() {
        0 | 1 => Ok((None, top(docs.first())?)),
        2 => {
            let header = top(docs.first())?;
            let Some(spec) = header.as_map().and_then(|m| m.get("spec")).and_then(Value::as_map).cloned() else {
                return Err(ConfigError::Yaml(format!("{file}: only one YAML document is allowed (or a `spec:` header and the config)")));
            };
            if header.as_map().is_some_and(|m| m.len() > 1) {
                return Err(ConfigError::Invalid(format!("{file}: the header document holds only `spec`")));
            }
            Ok((Some(spec), top(docs.get(1))?))
        }
        _ => Err(ConfigError::Yaml(format!("{file}: at most two YAML documents (a `spec:` header and the config)"))),
    }
}

fn scalar(s: &Scalar) -> Value {
    match s {
        Scalar::Null => Value::Null,
        Scalar::Boolean(b) => Value::Bool(*b),
        Scalar::Integer(i) => Value::Int(*i),
        Scalar::FloatingPoint(f) => Value::Float(f.into_inner()),
        Scalar::String(s) => Value::Str(s.to_string()),
    }
}

fn key_string(k: &Yaml, file: &str) -> Result<String, ConfigError> {
    match k {
        Yaml::Value(s) => Ok(scalar(s).scalar_string().unwrap_or_default()),
        Yaml::Representation(s, _, _) => Ok(s.to_string()),
        _ => Err(ConfigError::Yaml(format!("{file}: keys must be strings"))),
    }
}

fn convert(y: &Yaml, file: &str) -> Result<Value, ConfigError> {
    Ok(match y {
        Yaml::Value(s) => scalar(s),
        Yaml::Representation(s, style, tag) => {
            let resolved = Scalar::parse_from_cow_and_metadata(s.clone(), *style, tag.as_ref());
            match resolved {
                Some(sc) => scalar(&sc),
                None => Value::Str(s.to_string()),
            }
        }
        Yaml::Sequence(items) => Value::Seq(items.iter().map(|i| convert(i, file)).collect::<Result<_, _>>()?),
        Yaml::Mapping(m) => {
            let mut out = Map::new();
            let mut merges: Vec<Value> = Vec::new();
            for (k, v) in m.iter() {
                let key = key_string(k, file)?;
                let val = convert(v, file)?;
                if key == "<<" {
                    merges.push(val);
                } else {
                    out.insert(key, val);
                }
            }
            // `<<: *a` or `<<: [*a, *b]`: keys already present win, then earlier merges win
            for m in merges {
                let sources = match m {
                    Value::Seq(v) => v,
                    other => vec![other],
                };
                for s in sources {
                    let Value::Map(sm) = s else {
                        return Err(ConfigError::Yaml(format!("{file}: `<<` merges hashes only")));
                    };
                    for (k, v) in sm {
                        out.entry(k).or_insert(v);
                    }
                }
            }
            Value::Map(out)
        }
        Yaml::Tagged(tag, inner) => {
            if tag.suffix == "reference" {
                let Value::Seq(parts) = convert(inner, file)? else {
                    return Err(ConfigError::Yaml(format!("{file}: !reference takes an array, like !reference [job, script]")));
                };
                let path: Vec<String> = parts.iter().map(|p| p.scalar_string().unwrap_or_default()).collect();
                if path.is_empty() || path.iter().any(String::is_empty) {
                    return Err(ConfigError::Yaml(format!("{file}: invalid !reference {path:?}")));
                }
                Value::Ref(path)
            } else {
                // other tags (`!!str` and friends are handled by the loader)
                return Err(ConfigError::Unsupported(format!("{file}: the YAML tag !{} is not supported", tag.suffix)));
            }
        }
        Yaml::Alias(_) | Yaml::BadValue => return Err(ConfigError::Yaml(format!("{file}: invalid YAML value (unknown anchor?)"))),
    })
}

/// Deep merge: hashes merge key by key, anything else is replaced by `over`.
pub fn deep_merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Map(b), Value::Map(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_nesting_is_refused() {
        // a 1 MiB file can nest hundreds of thousands of levels: no stack overflow
        let block = |n: usize| format!("a:\n  {}x\n", "- ".repeat(n));
        let flow = |n: usize| format!("a: {}{}\n", "[".repeat(n), "]".repeat(n));
        assert!(parse(&block(200_000), "x.yml").unwrap_err().to_string().contains("deeper than"));
        assert!(parse(&flow(200_000), "x.yml").is_err());
        // the top-level hash is one level
        assert!(parse(&block(MAX_DEPTH - 1), "x.yml").is_ok());
        assert!(parse(&flow(MAX_DEPTH - 1), "x.yml").is_ok());
        assert!(parse(&block(MAX_DEPTH), "x.yml").is_err());
        // anchors chained into each other nest the loaded tree with flat events: it must still return
        let chain: String = (1..30_000).map(|i| format!("a{i}: &a{i} [*a{}]\n", i - 1)).collect();
        let _ = parse(&format!("a0: &a0 [x]\n{chain}"), "x.yml");
    }

    #[test]
    fn anchors_merge_keys_and_references() {
        let v = parse(
            ".base: &base\n  image: alpine\n  tags: [a]\njob:\n  <<: *base\n  tags: [b]\n  script: !reference [.setup, script]\n",
            "x.yml",
        )
        .unwrap();
        let job = v.as_map().unwrap()["job"].as_map().unwrap();
        assert_eq!(job["image"], Value::Str("alpine".into()));
        assert_eq!(job["tags"], Value::Seq(vec![Value::Str("b".into())]), "own keys win over <<");
        assert_eq!(job["script"], Value::Ref(vec![".setup".into(), "script".into()]));
    }

    #[test]
    fn a_repeated_key_keeps_its_last_value() {
        // as on GitLab (Ruby's Psych): found in a real project's .gitlab-ci.yml
        let v = parse("variables:\n  A: one\njob:\n  script: [x]\nvariables:\n  B: two\n", "x.yml").unwrap();
        let vars = v.as_map().unwrap()["variables"].as_map().unwrap();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["B"], Value::Str("two".into()));
    }

    #[test]
    fn scalars_keep_their_types() {
        let v = parse("variables:\n  A: 1\n  B: true\n  C: '1'\n  D: 1.5\n  E: ~\n", "x.yml").unwrap();
        let vars = v.as_map().unwrap()["variables"].as_map().unwrap();
        assert_eq!(vars["A"].scalar_string().unwrap(), "1");
        assert_eq!(vars["B"].scalar_string().unwrap(), "true");
        assert_eq!(vars["C"], Value::Str("1".into()));
        assert_eq!(vars["D"].scalar_string().unwrap(), "1.5");
        assert_eq!(vars["E"], Value::Null);
    }

    #[test]
    fn errors_are_clear() {
        assert!(matches!(parse("a: [", "x.yml"), Err(ConfigError::Yaml(_))));
        assert!(matches!(parse("- a\n- b\n", "x.yml"), Err(ConfigError::Yaml(_))));
        // a `spec:` header is fine; a second document without one is not
        assert!(parse("spec:\n  inputs: {}\n---\na: 1\n", "x.yml").is_ok());
        assert!(matches!(parse("a: 1\n---\nb: 2\n", "x.yml"), Err(ConfigError::Yaml(_))));
    }

    #[test]
    fn deep_merge_replaces_arrays() {
        let mut a = parse("j:\n  script: [a]\n  variables: {X: 1}\n", "a").unwrap();
        let b = parse("j:\n  script: [b]\n  variables: {Y: 2}\n", "b").unwrap();
        deep_merge(&mut a, b);
        let j = a.as_map().unwrap()["j"].as_map().unwrap();
        assert_eq!(j["script"], Value::Seq(vec![Value::Str("b".into())]));
        assert_eq!(j["variables"].as_map().unwrap().len(), 2);
    }
}
