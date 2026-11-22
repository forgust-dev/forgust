//! Compose: each service is a block, without files (a compose file declares it), linked to
//! the image it builds from the repository, the services it depends on, and those its
//! environment names: a guess, `postgres://app@db:5432` reaching `db`.

use serde_json::Value;

use super::{Ctx, Draft, To, dir_of, docker, file_name, join};
use crate::model::{Certainty, Kind, LinkKind, Role};
use crate::yaml;

/// Images by what they are, from their name.
const ROLES: &[(Role, &[&str])] = &[
    (
        Role::Database,
        &[
            "postgres",
            "postgis",
            "timescale",
            "mysql",
            "mariadb",
            "mongo",
            "cockroach",
            "clickhouse",
            "cassandra",
            "scylla",
            "couchdb",
            "neo4j",
            "surrealdb",
            "influxdb",
        ],
    ),
    (Role::Cache, &["redis", "valkey", "keydb", "memcached", "dragonfly"]),
    (Role::Queue, &["rabbitmq", "nats", "kafka", "redpanda", "activemq", "pulsar"]),
    (Role::Search, &["elasticsearch", "opensearch", "meilisearch", "typesense", "solr", "quickwit"]),
    (Role::Storage, &["minio", "maxio", "garage", "seaweedfs", "azurite", "localstack", "rustfs"]),
    (Role::Mail, &["mailpit", "mailhog", "maildev", "smtp4dev", "greenmail"]),
];

pub(crate) fn is_compose(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n.starts_with("docker-compose") || n.starts_with("compose")) && (n.ends_with(".yml") || n.ends_with(".yaml"))
}

/// A stack the tests start (`tests/e2e/docker-compose.yml`, `compose.test.yml`): its services,
/// simulators and sinks, are none of what the repository runs.
fn is_test_stack(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    let in_tests = p.split('/').rev().skip(1).any(|seg| matches!(seg, "test" | "tests" | "e2e" | "integration-tests"));
    in_tests || [".test.", ".e2e.", ".ci."].iter().any(|t| file_name(&p).contains(t))
}

pub(super) fn detect(cx: &mut Ctx) {
    let found: Vec<&str> = cx.files.iter().map(String::as_str).filter(|f| is_compose(file_name(f)) && !is_test_stack(f)).collect();
    for path in found {
        let Some(text) = cx.read(path) else { continue };
        let doc = match yaml::parse(&text) {
            Ok(v) => v,
            Err(e) => {
                cx.warn(path, format!("invalid YAML: {e}"));
                continue;
            }
        };
        let Some(services) = doc.get("services").and_then(Value::as_object) else { continue };
        let dir = dir_of(path);
        let names: Vec<&String> = services.keys().collect();
        for (name, svc) in services {
            let key = format!("service:{name}");
            let mut d = Draft::new(key.clone(), name, Kind::Service);
            d.manifests.push(path.to_string());
            d.image = svc.get("image").and_then(Value::as_str).map(str::to_string);
            d.role = d.image.as_deref().and_then(role);
            cx.add(d);
            let build = match svc.get("build") {
                Some(Value::String(context)) => Some((join(&dir, context), "Dockerfile")),
                Some(Value::Object(b)) => Some((
                    join(&dir, b.get("context").and_then(Value::as_str).unwrap_or(".")),
                    b.get("dockerfile").and_then(Value::as_str).unwrap_or("Dockerfile"),
                )),
                _ => None,
            };
            if let Some((context, dockerfile)) = build {
                let image = docker::key(&join(&context, dockerfile));
                cx.link(&key, To::Image { image, context }, LinkKind::Runs, Certainty::Declared, path);
            }
            let deps: Vec<String> = match svc.get("depends_on") {
                Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
                Some(Value::Object(m)) => m.keys().cloned().collect(),
                _ => Vec::new(),
            };
            for dep in &deps {
                cx.link(&key, To::Key(format!("service:{dep}")), LinkKind::DependsOn, Certainty::Declared, path);
            }
            for (var, value) in env_vars(svc.get("environment")) {
                for other in names.iter().copied().filter(|o| *o != name && !deps.contains(o)) {
                    if names_host(&var, &value, other) {
                        cx.link(&key, To::Key(format!("service:{other}")), LinkKind::Connects, Certainty::Inferred, path);
                    }
                }
            }
        }
    }
}

/// What an image is, from its name: `registry:5000/library/postgres:17-alpine` is a database.
fn role(image: &str) -> Option<Role> {
    let name = image.rsplit('/').next().unwrap_or(image);
    let name = name.split([':', '@']).next().unwrap_or(name).to_ascii_lowercase();
    ROLES.iter().find(|(_, names)| names.iter().any(|n| name.contains(n))).map(|(r, _)| *r)
}

/// The variables of `environment:`, a map or `KEY=value` lines, as (name, value).
fn env_vars(env: Option<&Value>) -> Vec<(String, String)> {
    match env {
        Some(Value::Object(m)) => m
            .iter()
            .filter_map(|(k, v)| v.as_str().map(str::to_string).or_else(|| v.as_number().map(|n| n.to_string())).map(|v| (k.clone(), v)))
            .collect(),
        Some(Value::Array(a)) => {
            a.iter().filter_map(|v| v.as_str()?.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect()
        }
        _ => Vec::new(),
    }
}

/// Variables whose value is a host, or holds one.
const HOST_VARS: &[&str] = &["HOST", "URL", "URI", "ENDPOINT", "ADDR", "SERVER", "DSN", "BROKER"];

/// Whether a variable names `host` as one: in an address (`postgres://app@db:5432`,
/// `http://s3:9000`, `host=db port=5432`), or as the whole value of a host's variable
/// (`REDIS_HOST=redis`), not of any other (`POSTGRES_USER=app` beside a service `app`).
fn names_host(var: &str, value: &str, host: &str) -> bool {
    if value == host {
        let var = var.to_ascii_uppercase();
        return HOST_VARS.iter().any(|h| var.contains(h));
    }
    let bytes = value.as_bytes();
    let mut from = 0;
    while let Some(i) = value[from..].find(host) {
        let (start, end) = (from + i, from + i + host.len());
        let before = &value[..start];
        let opens = before.ends_with("//") || before.ends_with('@') || before.ends_with("host=");
        let closes = end == bytes.len() || matches!(bytes[end], b':' | b'/' | b'?' | b' ' | b',' | b';');
        if opens && closes {
            return true;
        }
        from = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_and_hosts() {
        assert_eq!(role("postgres:17"), Some(Role::Database));
        assert_eq!(role("ghcr.io/axllent/mailpit:v1.20"), Some(Role::Mail));
        assert_eq!(role("forgust:latest"), None);
        assert!(is_test_stack("tests/e2e/docker-compose.yml"));
        assert!(is_test_stack("compose.test.yml"));
        assert!(!is_test_stack("compose.dev.yml"));
        assert!(!is_test_stack("deploy/compose.yml"));
        assert!(names_host("DATABASE_URL", "postgres://app:pw@db:5432/app", "db"));
        assert!(names_host("S3_ENDPOINT", "http://s3:9000", "s3"));
        assert!(names_host("PG_DSN", "host=db port=5432", "db"));
        assert!(names_host("REDIS_HOST", "redis", "redis"));
        assert!(!names_host("POSTGRES_USER", "forgust", "forgust"), "a user named like a service is no host");
        assert!(!names_host("APP_URL", "https://web.example.com", "web"));
        assert!(!names_host("DB", "mydb", "db"));
    }
}
