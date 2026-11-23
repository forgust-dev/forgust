//! npm: a `package.json` is a block (its folder), but a workspace's root, which only gathers
//! packages; a dependency on another package of the repository is a link.

use serde_json::Value;

use super::{Ctx, Draft, To, described, dir_of, join};
use crate::model::{Certainty, Kind, LinkKind, Role};

/// Packages a web front end is built with.
const FRONTENDS: &[&str] = &[
    "react",
    "react-dom",
    "vue",
    "svelte",
    "@sveltejs/kit",
    "@angular/core",
    "solid-js",
    "preact",
    "next",
    "nuxt",
    "astro",
    "lit",
    "@remix-run/react",
    "vite",
];
/// Packages that serve HTTP.
const SERVERS: &[&str] = &["express", "fastify", "koa", "@nestjs/core", "hono", "@hapi/hapi", "restify", "@trpc/server", "elysia"];
const SECTIONS: &[&str] = &["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"];
/// The fields that say what another package imports from it.
const ENTRIES: &[&str] = &["main", "module", "exports", "types", "typings"];

pub(super) fn detect(cx: &mut Ctx) {
    for path in cx.named("package.json") {
        let Some(text) = cx.read(path) else { continue };
        let pkg: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                cx.warn(path, format!("invalid JSON: {e}"));
                continue;
            }
        };
        // a workspace's root (npm, Yarn, pnpm, Lerna): it gathers packages, it is none, unless
        // it says it is published
        let dir = dir_of(path);
        let gathers = pkg.get("workspaces").is_some() || ["pnpm-workspace.yaml", "lerna.json"].iter().any(|f| cx.exists(&join(&dir, f)));
        if gathers && pkg.get("private").and_then(Value::as_bool) != Some(false) {
            continue;
        }
        let name = pkg.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| cx.folder_name(&dir));
        let key = format!("node:{dir}");
        let mut d = Draft::new(key.clone(), &name, Kind::Node);
        d.roots.push(dir);
        d.manifests.push(path.to_string());
        d.package = Some(name);
        d.description = described(pkg.get("description").and_then(Value::as_str));
        let deps: Vec<&str> =
            SECTIONS.iter().filter_map(|s| pkg.get(*s).and_then(Value::as_object)).flat_map(|m| m.keys().map(String::as_str)).collect();
        d.role = if deps.iter().any(|d| FRONTENDS.contains(d)) {
            Some(Role::Web)
        } else if deps.iter().any(|d| SERVERS.contains(d)) {
            Some(Role::Api)
        } else if pkg.get("bin").is_some() {
            Some(Role::Cli)
        } else if ENTRIES.iter().any(|e| pkg.get(*e).is_some()) {
            Some(Role::Library)
        } else {
            // an app, or a tool its scripts run
            None
        };
        cx.add(d);
        // most name packages from npm: only those of the repository make links
        for dep in deps {
            cx.link(&key, To::Npm(dep.to_string()), LinkKind::Uses, Certainty::Declared, path);
        }
    }
}
