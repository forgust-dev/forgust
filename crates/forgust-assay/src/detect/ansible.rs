//! Ansible: a role (`roles/<name>/`, with its `tasks/main.yml` or `meta/main.yml`) is a block,
//! described by its `meta/main.yml` and using the roles beside it that it depends on there; so is
//! a collection (`galaxy.yml`), and the playbooks around roles (the folder of `ansible.cfg`, else
//! the one holding `roles/`), which hold them. A role or a playbook runs the images of the
//! repository its tasks name (`docker_container`, `podman_container`…): a guess.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Ctx, Draft, To, compose, described, dir_of, file_name, images_in, join};
use crate::model::{Certainty, Kind, LinkKind, holds};
use crate::yaml;

/// What the folder of playbooks holds besides them, when it is the repository's top.
const AROUND: &[&str] =
    &["ansible.cfg", "roles", "playbooks", "inventory", "inventories", "group_vars", "host_vars", "requirements.yml", "collections"];

pub(super) fn reads(path: &str) -> bool {
    let name = file_name(path);
    if name == "galaxy.yml" {
        return true;
    }
    if !(name.ends_with(".yml") || name.ends_with(".yaml")) {
        return false;
    }
    let segs: Vec<&str> = path.split('/').collect();
    let n = segs.len();
    // a role's tasks, and its meta: roles/<name>/tasks/<file>, roles/<name>/meta/main.yml
    let in_role = n >= 4 && segs[n - 4] == "roles" && (segs[n - 2] == "tasks" || (segs[n - 2] == "meta" && name.starts_with("main.")));
    // playbooks: in a folder named for them, or named so
    let playbook = (n >= 2 && matches!(segs[n - 2], "ansible" | "playbooks"))
        || matches!(name, "site.yml" | "site.yaml" | "playbook.yml" | "playbook.yaml");
    in_role || playbook
}

fn role_key(dir: &str) -> String {
    format!("ansible-role:{dir}")
}

pub(super) fn detect(cx: &mut Ctx) {
    // the roles, each with the folder of roles holding it
    let mut roles: BTreeMap<String, String> = BTreeMap::new();
    for f in cx.files {
        let segs: Vec<&str> = f.split('/').collect();
        let n = segs.len();
        if n >= 4 && segs[n - 4] == "roles" && matches!(segs[n - 2], "tasks" | "meta") && matches!(segs[n - 1], "main.yml" | "main.yaml") {
            roles.insert(segs[..n - 2].join("/"), segs[..n - 3].join("/"));
        }
    }
    for (dir, folder) in &roles {
        role(cx, dir, folder);
    }

    let mut collections = Vec::new();
    for path in cx.named("galaxy.yml") {
        let dir = dir_of(path);
        let Some(doc) = parsed(cx, path) else { continue };
        let field = |k: &str| doc.get(k).and_then(Value::as_str);
        let name = match (field("namespace"), field("name")) {
            (Some(ns), Some(n)) => format!("{ns}.{n}"),
            (None, Some(n)) => n.to_string(),
            _ => cx.folder_name(&dir),
        };
        let mut d = Draft::new(format!("ansible:{dir}"), &name, Kind::Ansible);
        d.description = described(field("description"));
        d.roots.push(dir.clone());
        d.manifests.push(path.to_string());
        cx.add(d);
        collections.push(dir);
    }

    // the playbooks: by ansible.cfg, else around roles (a collection's are its own)
    let mut projects: Vec<String> = cx.named("ansible.cfg").into_iter().map(dir_of).collect();
    for folder in roles.values() {
        let dir = dir_of(folder);
        if !projects.contains(&dir) {
            projects.push(dir);
        }
    }
    for dir in projects.into_iter().filter(|d| !collections.contains(d)) {
        playbooks(cx, &dir);
    }
}

/// The role at `dir`, in the folder of roles `folder`.
fn role(cx: &mut Ctx, dir: &str, folder: &str) {
    let name = file_name(dir);
    let key = role_key(dir);
    let mut d = Draft::new(key.clone(), name, Kind::Ansible);
    // a role named for what it installs: `postgres` sets up a database
    d.role = compose::role(name);
    d.roots.push(dir.to_string());
    let first = |cx: &Ctx, part: &str| ["main.yml", "main.yaml"].iter().map(|m| join(dir, &format!("{part}/{m}"))).find(|p| cx.exists(p));
    let meta = first(cx, "meta");
    d.manifests.extend(meta.clone().or_else(|| first(cx, "tasks")));
    let meta_doc = meta.as_deref().and_then(|m| parsed(cx, m));
    d.description = meta_doc.as_ref().and_then(|v| described(v.pointer("/galaxy_info/description").and_then(Value::as_str)));
    cx.add(d);
    if let (Some(meta), Some(doc)) = (&meta, &meta_doc) {
        for dep in doc.get("dependencies").and_then(Value::as_array).into_iter().flatten() {
            let name = match dep {
                Value::String(s) => Some(s.as_str()),
                Value::Object(o) => o.get("role").or_else(|| o.get("name")).and_then(Value::as_str),
                _ => None,
            };
            // a collection's role (`acme.platform.base`) by its own name, when it is beside it
            if let Some(name) = name.map(|n| n.rsplit('.').next().unwrap_or(n)) {
                cx.link(&key, To::Key(role_key(&join(folder, name))), LinkKind::Uses, Certainty::Declared, meta);
            }
        }
    }
    let tasks = join(dir, "tasks");
    let files: Vec<&str> = cx.files.iter().map(String::as_str).filter(|f| dir_of(f) == tasks && reads(f)).collect();
    for f in files {
        let docs = cx.read(f).and_then(|t| yaml::parse_all(&t).ok()).unwrap_or_default();
        for image in docs.iter().flat_map(images_in) {
            cx.link(&key, To::ImageNamed(image), LinkKind::Runs, Certainty::Inferred, f);
        }
    }
}

/// The playbooks of the folder `dir`, and what is around them: their inventory, their roles.
fn playbooks(cx: &mut Ctx, dir: &str) {
    let key = format!("ansible:{dir}");
    let mut d = Draft::new(key.clone(), &cx.folder_name(dir), Kind::Ansible);
    // its playbooks: the files read at its top or in its playbooks/ that hold plays
    let inside = join(dir, "playbooks");
    let candidates: Vec<&str> = cx
        .files
        .iter()
        .map(String::as_str)
        .filter(|f| reads(f) && (dir_of(f) == dir || dir_of(f) == inside) && file_name(f) != "galaxy.yml")
        .collect();
    let mut found = Vec::new();
    for f in candidates {
        let docs = cx.read(f).and_then(|t| yaml::parse_all(&t).ok()).unwrap_or_default();
        let plays: Vec<&Value> = docs.iter().filter_map(Value::as_array).flatten().collect();
        if plays.iter().any(|p| p.get("hosts").is_some() || p.get("import_playbook").is_some()) {
            found.push((f.to_string(), plays.into_iter().flat_map(images_in).collect::<Vec<_>>()));
        }
    }
    let cfg = join(dir, "ansible.cfg");
    if dir == "." {
        // the whole repository: only its Ansible files
        let around: Vec<String> = AROUND.iter().filter(|p| cx.any_at(p)).map(|p| p.to_string()).collect();
        let outside: Vec<String> = found.iter().map(|(f, _)| f.clone()).filter(|f| !around.iter().any(|r| holds(r, f))).collect();
        d.roots = around.into_iter().chain(outside).collect();
    } else {
        d.roots.push(dir.to_string());
    }
    d.manifests.extend(cx.exists(&cfg).then_some(cfg));
    d.manifests.extend(found.iter().map(|(f, _)| f.clone()));
    cx.add(d);
    for (f, images) in found {
        for image in images {
            cx.link(&key, To::ImageNamed(image), LinkKind::Runs, Certainty::Inferred, &f);
        }
    }
}

/// A YAML file's first document; one that is not YAML is a warning.
fn parsed(cx: &mut Ctx, path: &str) -> Option<Value> {
    match yaml::parse(&cx.read(path)?) {
        Ok(v) => Some(v),
        Err(e) => {
            cx.warn(path, format!("invalid YAML: {e}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_read() {
        assert!(reads("deploy/ansible/site.yml"));
        assert!(reads("deploy/ansible/roles/postgres/meta/main.yml"));
        assert!(reads("roles/postgres/tasks/backup.yaml"));
        assert!(reads("collections/acme/galaxy.yml"));
        assert!(!reads("deploy/ansible/roles/postgres/templates/pg_hba.conf.j2"));
        assert!(!reads("deploy/ansible/roles/postgres/defaults/main.yml"), "its variables name no image to run");
        assert!(!reads("config/app.yml"));
    }
}
