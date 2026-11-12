//! After includes: legacy global keywords, `extends`, and `!reference`.

use std::collections::HashMap;

use crate::ConfigError;
use crate::value::{Map, Value, deep_merge};

/// Top-level keys that are not jobs.
pub const GLOBAL_KEYWORDS: &[&str] = &[
    "stages",
    "variables",
    "default",
    "workflow",
    "include",
    "image",
    "services",
    "before_script",
    "after_script",
    "cache",
    "types",
    "spec",
];
/// Deprecated top-level keywords GitLab still reads as `default:` values.
const LEGACY_DEFAULTS: &[&str] = &["image", "services", "before_script", "after_script", "cache"];

pub const MAX_EXTENDS_DEPTH: usize = 11;

pub fn compose(root: Value) -> Result<Map, ConfigError> {
    let Value::Map(mut root) = root else { return Err(ConfigError::Yaml("the top level must be a hash".into())) };
    if root.contains_key("types") {
        return Err(ConfigError::Unsupported("`types` was removed from GitLab CI; use `stages`".into()));
    }
    // legacy globals → default (an explicit default wins)
    let mut default = match root.shift_remove("default") {
        Some(Value::Map(m)) => m,
        None | Some(Value::Null) => Map::new(),
        Some(_) => return Err(ConfigError::Invalid("`default` must be a hash".into())),
    };
    for k in LEGACY_DEFAULTS {
        if let Some(v) = root.shift_remove(*k) {
            default.entry(k.to_string()).or_insert(v);
        }
    }
    root.insert("default".into(), Value::Map(default));

    // extends: resolve every job (hidden ones too) against the others
    let names: Vec<String> = root.keys().filter(|k| !GLOBAL_KEYWORDS.contains(&k.as_str())).cloned().collect();
    let mut done: HashMap<String, Map> = HashMap::new();
    for n in &names {
        let mut stack = Vec::new();
        resolve_extends(n, &root, &mut done, &mut stack)?;
    }
    for (n, m) in done {
        root.insert(n, Value::Map(m));
    }

    // !reference against the extended document
    let snapshot = root.clone();
    for (_, v) in root.iter_mut() {
        resolve_refs(v, &snapshot, 0)?;
    }
    Ok(root)
}

fn resolve_extends(name: &str, root: &Map, done: &mut HashMap<String, Map>, stack: &mut Vec<String>) -> Result<Map, ConfigError> {
    if let Some(m) = done.get(name) {
        return Ok(m.clone());
    }
    if stack.iter().any(|s| s == name) {
        stack.push(name.to_string());
        return Err(ConfigError::Invalid(format!("circular `extends`: {}", stack.join(" → "))));
    }
    if stack.len() >= MAX_EXTENDS_DEPTH {
        return Err(ConfigError::Invalid(format!("`extends` nests more than {MAX_EXTENDS_DEPTH} levels at `{name}`")));
    }
    let own = match root.get(name) {
        Some(Value::Map(m)) => m.clone(),
        Some(Value::Null) => Map::new(),
        Some(other) => return Err(ConfigError::Invalid(format!("job `{name}` must be a hash, not a {}", other.type_name()))),
        None => return Err(ConfigError::Invalid(format!("`extends` names an unknown job `{name}`"))),
    };
    let parents: Vec<String> = match own.get("extends") {
        None => Vec::new(),
        Some(Value::Str(s)) => vec![s.clone()],
        Some(Value::Seq(s)) => s
            .iter()
            .map(|p| p.as_str().map(String::from).ok_or_else(|| ConfigError::Invalid(format!("job `{name}`: `extends` takes job names"))))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(ConfigError::Invalid(format!("job `{name}`: `extends` takes a job name or a list"))),
    };
    stack.push(name.to_string());
    let mut merged = Value::Map(Map::new());
    for p in &parents {
        if GLOBAL_KEYWORDS.contains(&p.as_str()) {
            return Err(ConfigError::Invalid(format!("job `{name}` cannot extend `{p}`")));
        }
        let pm = resolve_extends(p, root, done, stack)?;
        deep_merge(&mut merged, Value::Map(pm));
    }
    stack.pop();
    let mut own = own;
    own.shift_remove("extends");
    deep_merge(&mut merged, Value::Map(own));
    let Value::Map(m) = merged else { unreachable!() };
    done.insert(name.to_string(), m.clone());
    Ok(m)
}

fn lookup<'a>(root: &'a Map, path: &[String]) -> Option<&'a Value> {
    let mut cur = root.get(&path[0])?;
    for k in &path[1..] {
        cur = cur.as_map()?.get(k)?;
    }
    Some(cur)
}

fn resolve_refs(v: &mut Value, root: &Map, depth: usize) -> Result<(), ConfigError> {
    if depth > 10 {
        return Err(ConfigError::Invalid("!reference nests more than 10 levels".into()));
    }
    match v {
        Value::Ref(path) => {
            let found =
                lookup(root, path).cloned().ok_or_else(|| ConfigError::Invalid(format!("!reference {path:?} points to nothing")))?;
            *v = found;
            resolve_refs(v, root, depth + 1)
        }
        Value::Seq(items) => items.iter_mut().try_for_each(|i| resolve_refs(i, root, depth)),
        Value::Map(m) => m.values_mut().try_for_each(|i| resolve_refs(i, root, depth)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::parse;

    #[test]
    fn extends_merges_deeply_in_order() {
        let root = parse(
            ".a:\n  image: a\n  variables: {X: 1, Y: 1}\n  script: [one]\n.b:\n  extends: .a\n  variables: {Y: 2}\njob:\n  extends: [.b]\n  script: [two]\n",
            "x",
        )
        .unwrap();
        let c = compose(root).unwrap();
        let job = c["job"].as_map().unwrap();
        assert_eq!(job["image"], Value::Str("a".into()));
        assert_eq!(job["script"], Value::Seq(vec![Value::Str("two".into())]));
        let vars = job["variables"].as_map().unwrap();
        assert_eq!(vars["X"].scalar_string().unwrap(), "1");
        assert_eq!(vars["Y"].scalar_string().unwrap(), "2");
        assert!(!job.contains_key("extends"));
    }

    #[test]
    fn extends_errors() {
        let c = compose(parse("a:\n  extends: b\nb:\n  extends: a\n", "x").unwrap());
        assert!(matches!(c, Err(ConfigError::Invalid(m)) if m.contains("circular")));
        let c = compose(parse("a:\n  extends: .missing\n  script: x\n", "x").unwrap());
        assert!(matches!(c, Err(ConfigError::Invalid(m)) if m.contains(".missing")));
    }

    #[test]
    fn references_resolve_after_extends() {
        let root = parse(
            ".setup:\n  script: [apt-get update]\n.base:\n  before_script: [echo base]\njob:\n  extends: .base\n  script:\n    - !reference [.setup, script]\n    - make\n  after_script: !reference [job, before_script]\n",
            "x",
        )
        .unwrap();
        let c = compose(root).unwrap();
        let job = c["job"].as_map().unwrap();
        assert_eq!(crate::job::script("job", "script", &job["script"]).unwrap(), vec!["apt-get update", "make"]);
        assert_eq!(crate::job::script("job", "after_script", &job["after_script"]).unwrap(), vec!["echo base"]);
    }

    #[test]
    fn legacy_globals_become_defaults() {
        let c = compose(parse("image: ruby\ndefault:\n  before_script: [b]\njob:\n  script: x\n", "x").unwrap()).unwrap();
        let d = c["default"].as_map().unwrap();
        assert_eq!(d["image"], Value::Str("ruby".into()));
        assert!(d.contains_key("before_script"));
    }
}
