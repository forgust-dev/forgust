//! YAML (compose files, CI configurations, charts, manifests, playbooks) read into JSON values,
//! which the detectors walk.

use saphyr::{Scalar, Yaml};
use serde_json::{Map, Number, Value};

/// The first document of `source`; YAML merge keys (`<<`) applied, tags dropped. A key given
/// twice keeps its last value, as most YAML readers of these files do.
pub fn parse(source: &str) -> Result<Value, String> {
    Ok(parse_all(source)?.into_iter().next().unwrap_or(Value::Null))
}

/// Every document of `source`, as [`parse`] reads the first: Kubernetes manifests put several
/// in a file, a `---` between each.
pub fn parse_all(source: &str) -> Result<Vec<Value>, String> {
    let mut parser = saphyr_parser::Parser::new_from_str(source);
    let mut loader = saphyr::YamlLoader::default();
    loader.allow_duplicate_keys(true);
    parser.load(&mut loader, true).map_err(|e| e.to_string())?;
    if let Some(e) = loader.error() {
        return Err(e.to_string());
    }
    Ok(loader.into_documents().iter().map(convert).collect())
}

fn convert(y: &Yaml) -> Value {
    match y {
        Yaml::Value(s) => scalar(s),
        Yaml::Representation(s, style, tag) => match Scalar::parse_from_cow_and_metadata(s.clone(), *style, tag.as_ref()) {
            Some(sc) => scalar(&sc),
            None => Value::String(s.to_string()),
        },
        Yaml::Sequence(items) => Value::Array(items.iter().map(convert).collect()),
        Yaml::Mapping(m) => {
            let mut out = Map::new();
            let mut merges = Vec::new();
            for (k, v) in m.iter() {
                let Some(key) = key(k) else { continue };
                if key == "<<" {
                    merges.push(convert(v));
                } else {
                    out.insert(key, convert(v));
                }
            }
            // `<<: *a` or `<<: [*a, *b]`: keys already there win, then the earlier merges
            for m in merges {
                let sources = match m {
                    Value::Array(v) => v,
                    other => vec![other],
                };
                for s in sources {
                    if let Value::Object(sm) = s {
                        for (k, v) in sm {
                            out.entry(k).or_insert(v);
                        }
                    }
                }
            }
            Value::Object(out)
        }
        Yaml::Tagged(_, inner) => convert(inner),
        Yaml::Alias(_) | Yaml::BadValue => Value::Null,
    }
}

fn scalar(s: &Scalar) -> Value {
    match s {
        Scalar::Null => Value::Null,
        Scalar::Boolean(b) => Value::Bool(*b),
        Scalar::Integer(i) => Value::Number((*i).into()),
        Scalar::FloatingPoint(f) => Number::from_f64(f.into_inner()).map(Value::Number).unwrap_or(Value::Null),
        Scalar::String(s) => Value::String(s.to_string()),
    }
}

fn key(k: &Yaml) -> Option<String> {
    match k {
        Yaml::Value(s) => Some(match scalar(s) {
            Value::String(s) => s,
            other => other.to_string(),
        }),
        Yaml::Representation(s, _, _) => Some(s.to_string()),
        _ => None,
    }
}

/// The strings of a value that is a string or a list of them.
pub fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items.iter().filter_map(|i| i.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_merge_keys_and_keeps_on_a_string() {
        let v = parse("base: &b\n  image: postgres\nservices:\n  db:\n    <<: *b\n    ports: [5432]\non: push\n").unwrap();
        assert_eq!(v["services"]["db"]["image"], "postgres");
        assert_eq!(v["services"]["db"]["ports"][0], 5432);
        // YAML 1.2: `on` is a key like any other (GitHub workflows)
        assert_eq!(v["on"], "push");
    }

    #[test]
    fn reads_every_document() {
        let docs = parse_all("kind: Service\n---\nkind: Deployment\n").unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[1]["kind"], "Deployment");
        assert_eq!(parse("kind: Service\n---\nkind: Deployment\n").unwrap()["kind"], "Service", "the first");
    }
}
