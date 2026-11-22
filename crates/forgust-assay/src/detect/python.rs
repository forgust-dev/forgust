//! Python: a folder with a project's pyproject.toml (`[project]` or Poetry's), a setup.py or a
//! setup.cfg is a block. A pyproject.toml holding only tools' settings is none.

use std::collections::BTreeSet;

use toml::{Table, Value};

use super::{Ctx, Draft, described, dir_of, file_name, join};
use crate::model::{Kind, Role};

/// Packages that serve HTTP.
const SERVERS: &[&str] = &["fastapi", "flask", "django", "starlette", "aiohttp", "sanic", "litestar", "tornado", "quart"];

pub(super) fn detect(cx: &mut Ctx) {
    let dirs: BTreeSet<String> =
        cx.files.iter().filter(|f| matches!(file_name(f), "pyproject.toml" | "setup.py" | "setup.cfg")).map(|f| dir_of(f)).collect();
    for dir in dirs {
        let pyproject = join(&dir, "pyproject.toml");
        let project = if cx.exists(&pyproject) { parse(cx, &pyproject) } else { None };
        let poetry = project.as_ref().and_then(|p| p.get("tool")).and_then(|t| t.get("poetry"));
        let spec = project.as_ref().and_then(|p| p.get("project"));
        let setup = cx.exists(&join(&dir, "setup.py")) || cx.exists(&join(&dir, "setup.cfg"));
        if spec.is_none() && poetry.is_none() && !setup {
            continue;
        }
        let name =
            spec.or(poetry).and_then(|s| s.get("name")).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| cx.folder_name(&dir));
        // `fastapi>=0.110` in [project], or a key of Poetry's dependencies
        let mut deps: Vec<String> = spec
            .and_then(|s| s.get("dependencies"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|d| d.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')).next().unwrap_or("").to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        deps.extend(
            poetry
                .and_then(|p| p.get("dependencies"))
                .and_then(Value::as_table)
                .map(|t| t.keys().map(|k| k.to_lowercase()).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
        // the commands it installs; a build system: a package to install, and import
        let scripts =
            [spec, poetry].into_iter().flatten().any(|s| s.get("scripts").and_then(Value::as_table).is_some_and(|t| !t.is_empty()));
        let packaged = project.as_ref().is_some_and(|p| p.contains_key("build-system"));
        let mut d = Draft::new(format!("python:{dir}"), &name, Kind::Python);
        d.manifests = ["pyproject.toml", "setup.py", "setup.cfg"].iter().map(|f| join(&dir, f)).filter(|f| cx.exists(f)).collect();
        d.roots.push(dir);
        d.description = described(spec.or(poetry).and_then(|s| s.get("description")).and_then(Value::as_str));
        d.role = if deps.iter().any(|d| SERVERS.contains(&d.as_str())) {
            Some(Role::Api)
        } else if scripts {
            Some(Role::Cli)
        } else if packaged {
            Some(Role::Library)
        } else {
            // an app it runs, or a setup.py, which is not read
            None
        };
        cx.add(d);
    }
}

/// A pyproject.toml's table; one that does not parse is a warning.
fn parse(cx: &mut Ctx, path: &str) -> Option<Table> {
    let text = cx.read(path)?;
    match toml::from_str::<Table>(&text) {
        Ok(t) => Some(t),
        Err(e) => {
            cx.warn(path, format!("invalid TOML: {}", e.message()));
            None
        }
    }
}
