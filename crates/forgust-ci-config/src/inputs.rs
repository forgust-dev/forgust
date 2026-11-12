//! CI/CD inputs: a `spec:inputs` header declares them, `include`
//! entries (and components) pass them, and `$[[ inputs.name ]]` in the file
//! takes their values, as in GitLab.

use crate::ConfigError;
use crate::value::{Map, Value};

/// At most this many interpolations in one file (GitLab's limit is 3 per string,
/// and a file-wide cap keeps a hostile file cheap).
const MAX_INTERPOLATIONS: usize = 10_000;

struct Input {
    kind: String,
    default: Option<Value>,
    options: Option<Vec<Value>>,
    regex: Option<regex::Regex>,
}

fn invalid(file: &str, msg: impl std::fmt::Display) -> ConfigError {
    ConfigError::Invalid(format!("{file}: {msg}"))
}

fn declared(spec: &Map, file: &str) -> Result<Vec<(String, Input)>, ConfigError> {
    for k in spec.keys() {
        if k != "inputs" {
            return Err(invalid(file, format!("unknown `spec:{k}`")));
        }
    }
    let Some(inputs) = spec.get("inputs") else { return Ok(Vec::new()) };
    let m = inputs.as_map().ok_or_else(|| invalid(file, "`spec:inputs` must be a hash"))?;
    let mut out = Vec::new();
    for (name, def) in m {
        let mut i = Input { kind: "string".into(), default: None, options: None, regex: None };
        match def {
            Value::Null => {}
            Value::Map(d) => {
                for (k, v) in d {
                    match k.as_str() {
                        "type" => {
                            i.kind = v.scalar_string().unwrap_or_default();
                            if !["string", "number", "boolean", "array"].contains(&i.kind.as_str()) {
                                return Err(invalid(file, format!("input `{name}`: type must be string, number, boolean or array")));
                            }
                        }
                        "default" => i.default = Some(v.clone()),
                        "options" => match v {
                            Value::Seq(o) => i.options = Some(o.clone()),
                            _ => return Err(invalid(file, format!("input `{name}`: `options` must be a list"))),
                        },
                        "regex" => {
                            let r = v.scalar_string().unwrap_or_default();
                            let r = r.trim_matches('/');
                            i.regex = Some(
                                regex::RegexBuilder::new(r)
                                    .size_limit(1 << 20)
                                    .build()
                                    .map_err(|e| invalid(file, format!("input `{name}`: bad regex: {e}")))?,
                            );
                        }
                        "description" => {}
                        other => return Err(invalid(file, format!("input `{name}`: unknown `{other}`"))),
                    }
                }
            }
            _ => return Err(invalid(file, format!("input `{name}` must be a hash"))),
        }
        out.push((name.clone(), i));
    }
    Ok(out)
}

fn check(name: &str, i: &Input, v: &Value, file: &str) -> Result<(), ConfigError> {
    let ok = matches!(
        (i.kind.as_str(), v),
        ("string", Value::Str(_)) | ("number", Value::Int(_) | Value::Float(_)) | ("boolean", Value::Bool(_)) | ("array", Value::Seq(_))
    );
    if !ok {
        return Err(invalid(file, format!("input `{name}` must be a {}, not a {}", i.kind, v.type_name())));
    }
    if let Some(opts) = &i.options
        && !opts.contains(v)
    {
        return Err(invalid(file, format!("input `{name}`: `{}` is not one of its options", v.scalar_string().unwrap_or_default())));
    }
    if let (Some(re), Value::Str(s)) = (&i.regex, v)
        && !re.is_match(s)
    {
        return Err(invalid(file, format!("input `{name}` does not match its regex")));
    }
    Ok(())
}

/// The values of `spec`'s inputs from `given` (and defaults), checked.
pub fn values(spec: &Map, given: &Map, file: &str) -> Result<Map, ConfigError> {
    let decl = declared(spec, file)?;
    for k in given.keys() {
        if !decl.iter().any(|(n, _)| n == k) {
            return Err(invalid(file, format!("unknown input `{k}`")));
        }
    }
    let mut out = Map::new();
    for (name, i) in &decl {
        let v = match given.get(name).or(i.default.as_ref()) {
            Some(v) => v.clone(),
            None => return Err(invalid(file, format!("input `{name}` is required"))),
        };
        // numbers and booleans arrive as strings from some callers
        let v = match (i.kind.as_str(), &v) {
            ("number", Value::Str(s)) => s.parse::<i64>().map(Value::Int).or_else(|_| s.parse::<f64>().map(Value::Float)).unwrap_or(v),
            ("boolean", Value::Str(s)) if s == "true" || s == "false" => Value::Bool(s == "true"),
            _ => v,
        };
        check(name, i, &v, file)?;
        out.insert(name.clone(), v);
    }
    Ok(out)
}

/// Replace `$[[ inputs.x ]]` everywhere in `doc` (keys and values) with the
/// inputs' values. A string that is only one interpolation takes the value's
/// type (an array stays an array).
pub fn interpolate(doc: Value, spec: &Map, given: &Map, file: &str) -> Result<Value, ConfigError> {
    let vals = values(spec, given, file)?;
    let mut budget = MAX_INTERPOLATIONS;
    walk(doc, &vals, file, &mut budget)
}

fn walk(v: Value, vals: &Map, file: &str, budget: &mut usize) -> Result<Value, ConfigError> {
    Ok(match v {
        Value::Str(s) => string(&s, vals, file, budget)?,
        Value::Seq(items) => Value::Seq(items.into_iter().map(|x| walk(x, vals, file, budget)).collect::<Result<_, _>>()?),
        Value::Map(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                let key = match string(&k, vals, file, budget)? {
                    Value::Str(s) => s,
                    other => other.scalar_string().ok_or_else(|| invalid(file, "an input used as a key must be a string"))?,
                };
                out.insert(key, walk(val, vals, file, budget)?);
            }
            Value::Map(out)
        }
        other => other,
    })
}

/// One `$[[ ... ]]`: `inputs.name` then optional `| function` steps.
fn evaluate(expr: &str, vals: &Map, file: &str) -> Result<Value, ConfigError> {
    let mut parts = expr.split('|').map(str::trim);
    let head = parts.next().unwrap_or("");
    let name = head.strip_prefix("inputs.").ok_or_else(|| invalid(file, format!("`$[[ {head} ]]`: only `inputs.<name>` can be used")))?;
    let mut v = vals.get(name).cloned().ok_or_else(|| invalid(file, format!("unknown input `{name}` in `$[[ ... ]]`")))?;
    for f in parts {
        let s = v.scalar_string().ok_or_else(|| invalid(file, format!("`{f}` applies to text inputs only")))?;
        v = Value::Str(if f == "expand_vars" {
            // variables are expanded where the value is used, as GitLab's
            // runner does for scripts; kept as written here
            s
        } else if f == "posix_escape" {
            s.chars().flat_map(|c| if c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c) { vec![c] } else { vec!['\\', c] }).collect()
        } else if let Some(args) = f.strip_prefix("truncate(").and_then(|r| r.strip_suffix(')')) {
            let n: Vec<usize> = args.split(',').map(|x| x.trim().parse().unwrap_or(0)).collect();
            let (off, len) = (n.first().copied().unwrap_or(0), n.get(1).copied().unwrap_or(usize::MAX));
            s.chars().skip(off).take(len).collect()
        } else {
            return Err(invalid(file, format!("unknown function `{f}` in `$[[ ... ]]`")));
        });
    }
    Ok(v)
}

fn string(s: &str, vals: &Map, file: &str, budget: &mut usize) -> Result<Value, ConfigError> {
    if !s.contains("$[[") {
        return Ok(Value::Str(s.to_string()));
    }
    let trimmed = s.trim();
    if trimmed.starts_with("$[[") && trimmed.ends_with("]]") && trimmed.matches("$[[").count() == 1 {
        *budget = budget.checked_sub(1).ok_or_else(|| invalid(file, "too many interpolations"))?;
        return evaluate(&trimmed[3..trimmed.len() - 2], vals, file);
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("$[[") {
        let end = rest[start..].find("]]").ok_or_else(|| invalid(file, "`$[[` without `]]`"))? + start;
        *budget = budget.checked_sub(1).ok_or_else(|| invalid(file, "too many interpolations"))?;
        out.push_str(&rest[..start]);
        let v = evaluate(&rest[start + 3..end], vals, file)?;
        out.push_str(&v.scalar_string().ok_or_else(|| invalid(file, "an array input can only stand alone, not inside text"))?);
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    Ok(Value::Str(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::parse_with_spec;

    fn run(src: &str, given: &[(&str, Value)]) -> Result<Value, ConfigError> {
        let (spec, doc) = parse_with_spec(src, "t.yml")?;
        let given: Map = given.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        interpolate(doc, &spec.unwrap_or_default(), &given, "t.yml")
    }

    #[test]
    fn inputs_fill_keys_and_values() {
        let src = "spec:\n  inputs:\n    name: {}\n    stage: {default: test, options: [build, test]}\n    tags: {type: array, default: [a, b]}\n    n: {type: number, default: 2}\n---\n\"$[[ inputs.name ]]-job\":\n  stage: $[[ inputs.stage ]]\n  tags: $[[ inputs.tags ]]\n  script:\n    - echo $[[ inputs.name | truncate(0,3) ]] x$[[ inputs.n ]]\n";
        let v = run(src, &[("name", Value::Str("frontend".into()))]).unwrap();
        let job = v.as_map().unwrap().get("frontend-job").unwrap().as_map().unwrap();
        assert_eq!(job["stage"], Value::Str("test".into()));
        assert_eq!(job["tags"], Value::Seq(vec![Value::Str("a".into()), Value::Str("b".into())]));
        assert_eq!(job["script"], Value::Seq(vec![Value::Str("echo fro x2".into())]));
        assert!(run(src, &[]).unwrap_err().to_string().contains("`name` is required"));
        assert!(
            run(src, &[("name", Value::Str("x".into())), ("stage", Value::Str("deploy".into()))])
                .unwrap_err()
                .to_string()
                .contains("options")
        );
        assert!(
            run(src, &[("name", Value::Str("x".into())), ("nope", Value::Bool(true))]).unwrap_err().to_string().contains("unknown input")
        );
        assert!(run(src, &[("name", Value::Int(3))]).unwrap_err().to_string().contains("must be a string"));
    }
}
