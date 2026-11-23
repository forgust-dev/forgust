//! Helm: a `Chart.yaml` is a block (its folder), named and described by it, using the charts of
//! the repository it depends on (`repository: file://../common`), and running the images of the
//! repository its values name: a guess, `image.repository: registry.example.com/shop/api`
//! running the image built in `api/`. Its templates are not read: they are no YAML until Helm
//! renders them.

use serde_json::Value;

use super::{Ctx, Draft, To, described, dir_of, file_name, images_in, join};
use crate::model::{Certainty, Kind, LinkKind, Role};
use crate::yaml;

pub(super) fn reads(path: &str) -> bool {
    matches!(file_name(path), "Chart.yaml" | "values.yaml")
}

pub(super) fn detect(cx: &mut Ctx) {
    for path in cx.named("Chart.yaml") {
        let Some(text) = cx.read(path) else { continue };
        let chart = match yaml::parse(&text) {
            Ok(v) => v,
            Err(e) => {
                cx.warn(path, format!("invalid YAML: {e}"));
                continue;
            }
        };
        let dir = dir_of(path);
        let name = chart.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| cx.folder_name(&dir));
        let key = format!("helm:{dir}");
        let mut d = Draft::new(key.clone(), &name, Kind::Helm);
        d.description = described(chart.get("description").and_then(Value::as_str));
        // a library chart deploys nothing: it lends templates to those depending on it
        if chart.get("type").and_then(Value::as_str) == Some("library") {
            d.role = Some(Role::Library);
        }
        d.roots.push(dir.clone());
        d.manifests.push(path.to_string());
        cx.add(d);
        // the charts it depends on, of the repository: others come from a chart repository
        for dep in chart.get("dependencies").and_then(Value::as_array).into_iter().flatten() {
            if let Some(at) = dep.get("repository").and_then(Value::as_str).and_then(|r| r.strip_prefix("file://")) {
                cx.link(&key, To::Within(join(&dir, at)), LinkKind::Uses, Certainty::Declared, path);
            }
        }
        let values = join(&dir, "values.yaml");
        if !cx.exists(&values) {
            continue;
        }
        let Some(text) = cx.read(&values) else { continue };
        match yaml::parse(&text) {
            Ok(v) => {
                for image in images_in(&v) {
                    cx.link(&key, To::ImageNamed(image), LinkKind::Runs, Certainty::Inferred, &values);
                }
            }
            Err(e) => cx.warn(&values, format!("invalid YAML: {e}")),
        }
    }
}
