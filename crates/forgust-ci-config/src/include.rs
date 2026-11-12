//! `include:` — local (with globs), project and remote files, merged into the
//! main config: included files in order (later wins), then the main file on top.

use async_trait::async_trait;

use crate::ConfigError;
use crate::value::{self, Map, Value};

/// Where included content comes from. The server implements it with the
/// repository at the pipeline's commit, permission checks for other projects,
/// and an SSRF-safe HTTP client for remote files.
#[async_trait]
pub trait Resolver: Send + Sync {
    /// A file at the pipeline's commit.
    async fn local(&self, path: &str) -> Result<String, String>;
    /// Paths at the pipeline's commit matching a glob (`*` within a segment, `**` across).
    async fn local_glob(&self, pattern: &str) -> Result<Vec<String>, String>;
    /// A file of another project (the pipeline's user must be able to read it).
    async fn project(&self, project: &str, git_ref: Option<&str>, file: &str) -> Result<String, String>;
    /// A public URL.
    async fn remote(&self, url: &str) -> Result<String, String>;
    /// A CI/CD component of a project on this server at `version` (a tag,
    /// branch or commit; `~latest` for the latest release): its template's text.
    async fn component(&self, project: &str, name: &str, version: &str) -> Result<String, String> {
        let _ = version;
        Err(format!("cannot include the component {project}/{name} here"))
    }
    /// This server's host, as components are addressed (`$CI_SERVER_FQDN`).
    fn server_host(&self) -> String {
        String::new()
    }
    /// Variables `include:rules` may test (predefined, project, pipeline).
    fn variables(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
}

/// A resolver for configs that must not include anything (tests, linting).
pub struct NoIncludes;

#[async_trait]
impl Resolver for NoIncludes {
    async fn local(&self, path: &str) -> Result<String, String> {
        Err(format!("cannot include {path} here"))
    }
    async fn local_glob(&self, pattern: &str) -> Result<Vec<String>, String> {
        Err(format!("cannot include {pattern} here"))
    }
    async fn project(&self, project: &str, _: Option<&str>, file: &str) -> Result<String, String> {
        Err(format!("cannot include {project}:{file} here"))
    }
    async fn remote(&self, url: &str) -> Result<String, String> {
        Err(format!("cannot include {url} here"))
    }
}

pub const MAX_INCLUDES: usize = 150;
pub const MAX_DEPTH: usize = 100;

#[derive(Clone, PartialEq)]
enum Origin {
    Local,
    Project { project: String, git_ref: Option<String> },
    Remote,
}

struct State<'a> {
    resolver: &'a dyn Resolver,
    count: usize,
    seen: Vec<String>,
}

/// One `include:` entry, normalized; `Map` holds the `inputs` it passes.
enum Inc {
    Local(String, Map),
    Project { project: String, git_ref: Option<String>, files: Vec<String>, inputs: Map },
    Remote(String, Map),
    Component { project: String, name: String, version: String, inputs: Map },
}

/// `<host>/<project path>/<name>@<version>` → (project, name, version).
fn component_address(addr: &str, host: &str) -> Result<(String, String, String), ConfigError> {
    let bad = || ConfigError::Invalid(format!("`include:component: {addr}` must be <server>/<project>/<name>@<version>"));
    let (path, version) = addr.rsplit_once('@').ok_or_else(bad)?;
    let mut segs: Vec<&str> = path.split('/').collect();
    let first = segs.first().copied().unwrap_or("");
    if first == "$CI_SERVER_FQDN" || first == "${CI_SERVER_FQDN}" || (!host.is_empty() && first.eq_ignore_ascii_case(host)) {
        segs.remove(0);
    } else if first.contains('.') || first.contains(':') {
        return Err(ConfigError::Unsupported(format!("`include:component`: components come from this server, not {first}")));
    }
    if segs.len() < 2 || version.is_empty() || segs.iter().any(|s| s.is_empty()) {
        return Err(bad());
    }
    let name = segs.pop().expect("two or more").to_string();
    Ok((segs.join("/"), name, version.to_string()))
}

/// Whether an `include:rules` list keeps the entry (`if` and `exists`; with
/// `changes` the entry is kept, the changes are not known yet).
async fn include_rules_keep(rules: &Value, resolver: &dyn Resolver) -> Result<bool, ConfigError> {
    let list = crate::job::rules("include", rules, &["always", "never"])?;
    let vars = resolver.variables();
    for r in &list {
        if let Some(cond) = &r.if_
            && !crate::expr::Expression::parse(cond)?.eval(&vars)
        {
            continue;
        }
        if let Some(paths) = &r.exists {
            let mut found = false;
            for p in paths {
                let p = p.trim_start_matches('/');
                if p.contains('*') {
                    found |= resolver.local_glob(p).await.map(|v| !v.is_empty()).unwrap_or(false);
                } else {
                    found |= resolver.local(p).await.is_ok();
                }
                if found {
                    break;
                }
            }
            if !found {
                continue;
            }
        }
        return Ok(r.when.as_deref() != Some("never"));
    }
    Ok(false)
}

async fn entries(v: &Value, resolver: &dyn Resolver) -> Result<Vec<Inc>, ConfigError> {
    let items: Vec<&Value> = match v {
        Value::Seq(s) => s.iter().collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for item in items {
        match item {
            Value::Str(s) if s.starts_with("https://") || s.starts_with("http://") => out.push(Inc::Remote(s.clone(), Map::new())),
            Value::Str(s) => out.push(Inc::Local(s.clone(), Map::new())),
            Value::Map(m) => {
                for k in m.keys() {
                    match k.as_str() {
                        "local" | "project" | "ref" | "file" | "remote" | "component" | "inputs" | "rules" => {}
                        "template" => {
                            let name = m.get("template").and_then(|v| v.as_str()).unwrap_or("");
                            let security =
                                ["Security/", "Jobs/SAST", "Jobs/Secret-Detection", "Jobs/Dependency-Scanning", "Jobs/Container-Scanning"]
                                    .iter()
                                    .any(|p| name.starts_with(p));
                            return Err(ConfigError::Unsupported(if security {
                                format!(
                                    "`include:template: {name}`: GitLab's security templates are not bundled. Turn on Settings > Security scanning \
                                     (built-in scanners), or run the analyzers' images in your own jobs: their `artifacts:reports` \
                                     (sast, secret_detection, dependency_scanning, container_scanning, or SARIF) show in the Security tab"
                                )
                            } else {
                                "`include:template`: GitLab's templates are not bundled".into()
                            }));
                        }
                        "cache" | "integrity" => {}
                        other => return Err(ConfigError::Invalid(format!("unknown `include:{other}`"))),
                    }
                }
                if let Some(r) = m.get("rules")
                    && !include_rules_keep(r, resolver).await?
                {
                    continue;
                }
                let inputs = match m.get("inputs") {
                    None => Map::new(),
                    Some(Value::Map(i)) => i.clone(),
                    Some(_) => return Err(ConfigError::Invalid("`include:inputs` must be a hash".into())),
                };
                if let Some(Value::Str(c)) = m.get("component") {
                    let (project, name, version) = component_address(c, &resolver.server_host())?;
                    out.push(Inc::Component { project, name, version, inputs });
                } else if let Some(Value::Str(l)) = m.get("local") {
                    out.push(Inc::Local(l.clone(), inputs));
                } else if let Some(Value::Str(r)) = m.get("remote") {
                    out.push(Inc::Remote(r.clone(), inputs));
                } else if let Some(Value::Str(p)) = m.get("project") {
                    let files = match m.get("file") {
                        Some(Value::Str(f)) => vec![f.clone()],
                        Some(Value::Seq(fs)) => fs.iter().filter_map(|f| f.as_str().map(String::from)).collect(),
                        _ => return Err(ConfigError::Invalid(format!("`include:project: {p}` needs a `file`"))),
                    };
                    out.push(Inc::Project {
                        project: p.trim_matches('/').to_string(),
                        git_ref: m.get("ref").and_then(Value::scalar_string),
                        files,
                        inputs,
                    });
                } else {
                    return Err(ConfigError::Invalid("each `include` needs local, project, remote or component".into()));
                }
            }
            other => return Err(ConfigError::Invalid(format!("`include` entries are strings or hashes, not {}", other.type_name()))),
        }
    }
    Ok(out)
}

/// Resolve every `include` (recursively) and merge it all into one document.
pub async fn resolve(main: Value, resolver: &dyn Resolver) -> Result<Value, ConfigError> {
    let mut st = State { resolver, count: 0, seen: Vec::new() };
    expand(main, &Origin::Local, &mut st, 0).await
}

fn expand<'a>(
    doc: Value,
    origin: &'a Origin,
    st: &'a mut State<'_>,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, ConfigError>> + Send + 'a>> {
    Box::pin(async move {
        let Value::Map(mut map) = doc else { return Ok(doc) };
        let Some(inc) = map.shift_remove("include") else { return Ok(Value::Map(map)) };
        if depth >= MAX_DEPTH {
            return Err(ConfigError::Invalid(format!("includes nest more than {MAX_DEPTH} levels")));
        }
        let mut merged = Value::Map(Map::new());
        for e in entries(&inc, st.resolver).await? {
            for (key, text, child_origin, label, inputs) in fetch(e, origin, st).await? {
                // a file with inputs is its own instance per set of inputs
                let key = if inputs.is_empty() { key } else { format!("{key}#{}", Value::Map(inputs.clone()).to_json()) };
                if st.seen.contains(&key) {
                    continue;
                }
                st.seen.push(key);
                st.count += 1;
                if st.count > MAX_INCLUDES {
                    return Err(ConfigError::Invalid(format!("more than {MAX_INCLUDES} included files")));
                }
                let parsed = match value::parse_with_spec(&text, &label)? {
                    (Some(spec), doc) => crate::inputs::interpolate(doc, &spec, &inputs, &label)?,
                    (None, _) if !inputs.is_empty() => {
                        return Err(ConfigError::Invalid(format!("{label}: `inputs` given, but the file declares no `spec:inputs`")));
                    }
                    (None, doc) => doc,
                };
                let resolved = expand(parsed, &child_origin, st, depth + 1).await?;
                value::deep_merge(&mut merged, resolved);
            }
        }
        value::deep_merge(&mut merged, Value::Map(map));
        Ok(merged)
    })
}

type Fetched = Vec<(String, String, Origin, String, Map)>;

async fn fetch(e: Inc, origin: &Origin, st: &State<'_>) -> Result<Fetched, ConfigError> {
    let r = st.resolver;
    let fail = |what: String, e: String| ConfigError::Include(format!("{what}: {e}"));
    Ok(match (e, origin) {
        (Inc::Local(path, inputs), Origin::Local) => {
            let path = path.trim_start_matches('/').to_string();
            let paths = if path.contains('*') {
                let mut p = r.local_glob(&path).await.map_err(|e| fail(format!("local `{path}`"), e))?;
                p.retain(|x| x.ends_with(".yml") || x.ends_with(".yaml"));
                p.sort();
                p
            } else {
                if !(path.ends_with(".yml") || path.ends_with(".yaml")) {
                    return Err(ConfigError::Invalid(format!("include `{path}` must be a .yml or .yaml file")));
                }
                vec![path]
            };
            let mut out = Vec::new();
            for p in paths {
                let text = r.local(&p).await.map_err(|e| fail(format!("local `{p}`"), e))?;
                out.push((format!("local:{p}"), text, Origin::Local, p, inputs.clone()));
            }
            out
        }
        // a local include inside a project-included file means that project
        (Inc::Local(path, inputs), Origin::Project { project, git_ref }) => {
            let p = path.trim_start_matches('/');
            let text = r.project(project, git_ref.as_deref(), p).await.map_err(|e| fail(format!("project `{project}` file `{p}`"), e))?;
            vec![(format!("project:{project}@{git_ref:?}:{p}"), text, origin.clone(), format!("{project}/{p}"), inputs)]
        }
        (Inc::Local(path, _), Origin::Remote) => {
            return Err(ConfigError::Invalid(format!("a remote include cannot include the local file `{path}`")));
        }
        (Inc::Project { project, git_ref, files, inputs }, _) => {
            let mut out = Vec::new();
            for f in files {
                let p = f.trim_start_matches('/').to_string();
                let text =
                    r.project(&project, git_ref.as_deref(), &p).await.map_err(|e| fail(format!("project `{project}` file `{p}`"), e))?;
                out.push((
                    format!("project:{project}@{git_ref:?}:{p}"),
                    text,
                    Origin::Project { project: project.clone(), git_ref: git_ref.clone() },
                    format!("{project}/{p}"),
                    inputs.clone(),
                ));
            }
            out
        }
        (Inc::Remote(url, inputs), _) => {
            let text = r.remote(&url).await.map_err(|e| fail(format!("remote `{url}`"), e))?;
            vec![(format!("remote:{url}"), text, Origin::Remote, url, inputs)]
        }
        // a component's own local includes resolve in its project, at its version
        (Inc::Component { project, name, version, inputs }, _) => {
            let text =
                r.component(&project, &name, &version).await.map_err(|e| fail(format!("component `{project}/{name}@{version}`"), e))?;
            vec![(
                format!("component:{project}/{name}@{version}"),
                text,
                Origin::Project { project: project.clone(), git_ref: Some(version.clone()) },
                format!("{project}/{name}@{version}"),
                inputs,
            )]
        }
    })
}
