//! Pulumi: a `Pulumi.yaml` is a block (its folder), named and described by it. It is a program,
//! not the package its `package.json` (or `pyproject.toml`, `go.mod`) makes of the folder: it
//! stands for that package's block. It runs the images of the repository its stacks name
//! (`Pulumi.production.yaml`: `shop:apiImage: registry/shop/api:1.2`), or a YAML program does: a
//! guess.

use serde_json::Value;

use super::{Ctx, Draft, To, described, dir_of, file_name, images_in};
use crate::model::{Certainty, Kind, LinkKind};
use crate::yaml;

/// Its project file (`Pulumi.yaml`), and its stacks' (`Pulumi.<stack>.yaml`).
pub(super) fn reads(path: &str) -> bool {
    let name = file_name(path);
    name.starts_with("Pulumi.") && (name.ends_with(".yaml") || name.ends_with(".yml"))
}

fn is_project(path: &str) -> bool {
    matches!(file_name(path), "Pulumi.yaml" | "Pulumi.yml")
}

pub(super) fn detect(cx: &mut Ctx) {
    let found: Vec<&str> = cx.files.iter().map(String::as_str).filter(|f| is_project(f)).collect();
    for path in found {
        let Some(text) = cx.read(path) else { continue };
        let doc = match yaml::parse(&text) {
            Ok(v) => v,
            Err(e) => {
                cx.warn(path, format!("invalid YAML: {e}"));
                continue;
            }
        };
        let dir = dir_of(path);
        // the package of its folder is the program's
        let package = |k: Kind| matches!(k, Kind::Rust | Kind::Node | Kind::Python | Kind::Go);
        let program = cx.drafts.iter().position(|d| package(d.kind) && d.roots == [dir.clone()]).map(|i| cx.drafts.remove(i));
        let name = doc.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| cx.folder_name(&dir));
        let key = format!("pulumi:{dir}");
        let mut d = Draft::new(key.clone(), &name, Kind::Pulumi);
        d.description = described(doc.get("description").and_then(Value::as_str)).or(program.and_then(|p| p.description));
        d.roots.push(dir.clone());
        d.manifests.push(path.to_string());
        cx.add(d);
        let mut images: Vec<(String, String)> = images_in(&doc).into_iter().map(|i| (i, path.to_string())).collect();
        let stacks: Vec<&str> = cx.files.iter().map(String::as_str).filter(|f| dir_of(f) == dir && reads(f) && !is_project(f)).collect();
        for stack in stacks {
            let config = cx.read(stack).and_then(|t| yaml::parse(&t).ok()).and_then(|v| v.get("config").cloned());
            images.extend(config.iter().flat_map(images_in).map(|i| (i, stack.to_string())));
        }
        for (image, via) in images {
            cx.link(&key, To::ImageNamed(image), LinkKind::Runs, Certainty::Inferred, &via);
        }
    }
}
