//! Cargo: a `[package]` is a block (its folder); a path dependency on another package of
//! the repository is a link, and so is a `workspace = true` one the workspace gives by path.
//! Dev-dependencies are left out: what a package needs to run, not to be tested.

use std::collections::HashMap;

use toml::{Table, Value};

use super::{Ctx, Draft, To, described, dir_of, join};
use crate::model::{Certainty, Kind, LinkKind, Role};

/// Crates a web front end is built with.
const FRONTENDS: &[&str] = &["leptos", "yew", "sycamore"];
/// Crates that serve HTTP or gRPC: a package using one is an API.
const SERVERS: &[&str] = &["axum", "actix-web", "rocket", "warp", "poem", "tide", "salvo", "tonic", "loco-rs", "ntex", "gotham"];

pub(super) fn detect(cx: &mut Ctx) {
    let mut manifests: Vec<(&str, Table)> = Vec::new();
    for path in cx.named("Cargo.toml") {
        let Some(text) = cx.read(path) else { continue };
        match toml::from_str::<Table>(&text) {
            Ok(t) => manifests.push((path, t)),
            Err(e) => cx.warn(path, format!("invalid TOML: {}", e.message())),
        }
    }
    // each workspace's dependencies given by path: name → folder
    let mut workspaces: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (path, t) in &manifests {
        if let Some(ws) = t.get("workspace").and_then(Value::as_table) {
            let dir = dir_of(path);
            let deps = ws.get("dependencies").and_then(Value::as_table).map(|d| path_deps(d, &dir)).unwrap_or_default();
            workspaces.insert(dir, deps);
        }
    }
    for (path, t) in &manifests {
        let Some(name) = t.get("package").and_then(|p| p.get("name")).and_then(Value::as_str) else { continue };
        let dir = dir_of(path);
        let key = format!("rust:{dir}");
        let mut d = Draft::new(key.clone(), name, Kind::Rust);
        d.roots.push(dir.clone());
        d.manifests.push(path.to_string());
        d.package = Some(name.to_string());
        d.description = described(t.get("package").and_then(|p| p.get("description")).and_then(Value::as_str));
        d.role = Some(role(t, &dir, cx));
        // `[[bin]]`s, and the binary src/main.rs makes when none of them is it, named after
        // the package
        let declared: Vec<&Value> = t.get("bin").and_then(Value::as_array).into_iter().flatten().collect();
        d.bins = declared.iter().filter_map(|b| b.get("name")?.as_str().map(str::to_string)).collect();
        let main_declared = declared.iter().any(|b| b.get("path").and_then(Value::as_str) == Some("src/main.rs"));
        if cx.exists(&join(&dir, "src/main.rs")) && !main_declared && !d.bins.iter().any(|b| b == name) {
            d.bins.push(name.to_string());
        }
        cx.add(d);
        // the nearest workspace up from the package
        let ws = ancestors(&dir).find_map(|a| workspaces.get(&a));
        for deps in dep_tables(t) {
            for (dep, spec) in deps {
                let Value::Table(spec) = spec else { continue };
                let target = if spec.get("workspace").and_then(Value::as_bool) == Some(true) {
                    ws.and_then(|w| w.get(dep.as_str()).cloned())
                } else {
                    spec.get("path").and_then(Value::as_str).map(|p| join(&dir, p))
                };
                if let Some(target) = target {
                    cx.link(&key, To::Key(format!("rust:{target}")), LinkKind::Uses, Certainty::Declared, path);
                }
            }
        }
    }
}

/// The dependencies of a table given by path, as folders from the repository's top.
fn path_deps(deps: &Table, dir: &str) -> HashMap<String, String> {
    deps.iter().filter_map(|(name, spec)| spec.as_table()?.get("path")?.as_str().map(|p| (name.clone(), join(dir, p)))).collect()
}

/// `[dependencies]`, `[build-dependencies]`, and both under each `[target.'…']`.
fn dep_tables(t: &Table) -> Vec<&Table> {
    let sections = ["dependencies", "build-dependencies"];
    let mut out: Vec<&Table> = sections.iter().filter_map(|s| t.get(*s).and_then(Value::as_table)).collect();
    if let Some(targets) = t.get("target").and_then(Value::as_table) {
        for target in targets.values().filter_map(Value::as_table) {
            out.extend(sections.iter().filter_map(|s| target.get(*s).and_then(Value::as_table)));
        }
    }
    out
}

/// What the package is for: a web front end, else an API when it serves HTTP or gRPC, else a
/// CLI when it has a binary, else a library.
fn role(t: &Table, dir: &str, cx: &Ctx) -> Role {
    let deps = dep_tables(t);
    let uses = |crates: &[&str]| crates.iter().any(|c| deps.iter().any(|d| d.contains_key(*c)));
    if uses(FRONTENDS) {
        Role::Web
    } else if uses(SERVERS) {
        Role::Api
    } else if t.contains_key("bin") || cx.exists(&join(dir, "src/main.rs")) {
        Role::Cli
    } else {
        Role::Library
    }
}

/// A folder, then each one up to the top (".").
fn ancestors(dir: &str) -> impl Iterator<Item = String> {
    let mut next = Some(dir.to_string());
    std::iter::from_fn(move || {
        let d = next.take()?;
        next = (d != ".").then(|| dir_of(&d));
        Some(d)
    })
}
