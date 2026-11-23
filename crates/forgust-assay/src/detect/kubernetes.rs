//! Kubernetes: a Kustomize folder (`kustomization.yaml`) is a block, with the bases and overlays
//! it builds from or that build from it, at the folder holding them all; so is a folder of
//! manifests (`k8s/`, `kubernetes/`, `manifests/`) that no chart nor Kustomize folder holds. Each
//! runs the images of the repository its manifests name (and a Kustomize folder's `images`): a
//! guess. Manifests are read in those folders and in Kustomize's (`base/`, `overlays/`), not
//! elsewhere: most YAML files of a repository are none.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Ctx, Draft, To, dir_of, file_name, images_in, join};
use crate::model::{Certainty, Kind, LinkKind, holds};
use crate::yaml;

/// Folders of manifests, by their name.
const FOLDERS: &[&str] = &["k8s", "kubernetes", "kube", "manifests"];
/// Kustomize's folders, by their name: their manifests are read too.
const KUSTOMIZE: &[&str] = &["base", "bases", "overlays", "components"];
const KUSTOMIZATIONS: &[&str] = &["kustomization.yaml", "kustomization.yml", "Kustomization"];

pub(super) fn reads(path: &str) -> bool {
    KUSTOMIZATIONS.contains(&file_name(path)) || in_manifests(path)
}

/// A YAML file in a folder of manifests, or of Kustomize's; not in a chart's templates, which are
/// no YAML until Helm renders them.
fn in_manifests(path: &str) -> bool {
    let yaml = path.ends_with(".yaml") || path.ends_with(".yml");
    let dirs: Vec<&str> = path.split('/').rev().skip(1).collect();
    yaml && !dirs.contains(&"templates") && dirs.iter().any(|d| FOLDERS.contains(d) || KUSTOMIZE.contains(d))
}

pub(super) fn detect(cx: &mut Ctx) {
    // what a chart holds is the chart's
    let mut taken: Vec<String> = cx.drafts.iter().filter(|d| d.kind == Kind::Helm).flat_map(|d| d.roots.clone()).collect();
    let kustomized = kustomize(cx, &taken);
    taken.extend(kustomized);
    manifests(cx, &taken);
}

/// A Kustomize folder as read: the others it builds from, and the images it names, with the file
/// naming each.
struct Kustomization {
    dir: String,
    file: String,
    builds_from: Vec<usize>,
    images: Vec<(String, String)>,
}

/// Kustomize's folders, each with those it builds from or that build from it: a block each group,
/// at the folder holding them all, or a block each folder when that is the repository's top.
/// Their roots.
fn kustomize(cx: &mut Ctx, taken: &[String]) -> Vec<String> {
    let files: Vec<&str> = cx
        .files
        .iter()
        .map(String::as_str)
        .filter(|f| KUSTOMIZATIONS.contains(&file_name(f)) && !taken.iter().any(|t| holds(t, f)))
        .collect();
    let dirs: Vec<String> = files.iter().map(|f| dir_of(f)).collect();
    let mut found = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let mut k = Kustomization { dir: dirs[i].clone(), file: file.to_string(), builds_from: Vec::new(), images: Vec::new() };
        let doc = cx.read(file).map(|t| yaml::parse(&t));
        match doc {
            Some(Ok(doc)) => {
                // the images it sets: their new name, else the one it renames
                for img in doc.get("images").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(name) = img.get("newName").or_else(|| img.get("name")).and_then(Value::as_str) {
                        k.images.push((name.to_string(), file.to_string()));
                    }
                }
                for entry in ["resources", "bases", "components"].iter().flat_map(|f| yaml::strings(doc.get(*f))) {
                    // from elsewhere: a URL, a repository at a ref
                    if entry.contains("://") || entry.contains("?ref=") {
                        continue;
                    }
                    let at = join(&k.dir, &entry);
                    if let Some(j) = dirs.iter().position(|d| *d == at) {
                        if j != i {
                            k.builds_from.push(j);
                        }
                    } else if cx.exists(&at) && in_manifests(&at) {
                        let images = cx.read(&at).and_then(|t| yaml::parse_all(&t).ok()).unwrap_or_default();
                        k.images.extend(images.iter().flat_map(images_in).map(|image| (image, at.clone())));
                    }
                }
            }
            Some(Err(e)) => cx.warn(file, format!("invalid YAML: {e}")),
            None => {}
        }
        found.push(k);
    }

    // the groups: each folder with those it builds from, and theirs
    let mut group: Vec<usize> = (0..found.len()).collect();
    fn top(group: &mut [usize], mut i: usize) -> usize {
        while group[i] != i {
            group[i] = group[group[i]];
            i = group[i];
        }
        i
    }
    let pairs: Vec<(usize, usize)> = found.iter().enumerate().flat_map(|(i, k)| k.builds_from.iter().map(move |j| (i, *j))).collect();
    for (i, j) in pairs {
        let (a, b) = (top(&mut group, i), top(&mut group, j));
        group[b] = a;
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..found.len() {
        let g = top(&mut group, i);
        groups.entry(g).or_default().push(i);
    }

    let mut roots = Vec::new();
    for members in groups.values() {
        let common = members.iter().map(|i| found[*i].dir.clone()).reduce(|a, b| common_dir(&a, &b)).unwrap_or_else(|| ".".to_string());
        if common == "." && members.len() > 1 {
            // at the top, a group would hold the whole repository: its folders are blocks apart
            for i in members {
                let k = &found[*i];
                let key = add(cx, &k.dir, vec![k.file.clone()], &k.images);
                for j in &k.builds_from {
                    cx.link(&key, To::Key(format!("kubernetes:{}", found[*j].dir)), LinkKind::Uses, Certainty::Declared, &k.file);
                }
                roots.push(k.dir.clone());
            }
        } else {
            let manifests = members.iter().map(|i| found[*i].file.clone()).collect();
            let images: Vec<(String, String)> = members.iter().flat_map(|i| found[*i].images.clone()).collect();
            add(cx, &common, manifests, &images);
            roots.push(common);
        }
    }
    roots
}

/// The folders of manifests no chart nor Kustomize folder holds: a block each, at the outermost
/// folder named for them on the way (`deploy/k8s`, holding `deploy/k8s/worker/`). A file is a
/// manifest when a document of it gives its API and kind.
fn manifests(cx: &mut Ctx, taken: &[String]) {
    let files: Vec<&str> = cx.files.iter().map(String::as_str).filter(|f| in_manifests(f) && !taken.iter().any(|t| holds(t, f))).collect();
    let mut by_root: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for file in files {
        let segs: Vec<&str> = file.split('/').collect();
        let Some(at) = segs[..segs.len() - 1].iter().position(|s| FOLDERS.contains(s)) else { continue };
        // not YAML (a template), or no manifest in it: not a block's
        let Some(Ok(docs)) = cx.read(file).map(|t| yaml::parse_all(&t)) else { continue };
        let docs: Vec<&Value> =
            docs.iter().filter(|d| ["apiVersion", "kind"].iter().all(|k| d.get(*k).is_some_and(Value::is_string))).collect();
        if docs.is_empty() {
            continue;
        }
        let images = by_root.entry(segs[..=at].join("/")).or_default();
        images.extend(docs.into_iter().flat_map(images_in).map(|image| (image, file.to_string())));
    }
    for (root, images) in by_root {
        add(cx, &root, vec![root.clone()], &images);
    }
}

/// Adds the block at `root`, running the images named: its key.
fn add(cx: &mut Ctx, root: &str, manifests: Vec<String>, images: &[(String, String)]) -> String {
    let key = format!("kubernetes:{root}");
    let mut d = Draft::new(key.clone(), &cx.folder_name(root), Kind::Kubernetes);
    d.roots.push(root.to_string());
    d.manifests = manifests;
    cx.add(d);
    let mut seen = Vec::new();
    for (image, via) in images {
        if !seen.contains(image) {
            seen.push(image.clone());
            cx.link(&key, To::ImageNamed(image.clone()), LinkKind::Runs, Certainty::Inferred, via);
        }
    }
    key
}

/// The deepest folder holding both: "." for the top.
fn common_dir(a: &str, b: &str) -> String {
    if a == "." || b == "." {
        return ".".to_string();
    }
    let shared: Vec<&str> = a.split('/').zip(b.split('/')).take_while(|(x, y)| x == y).map(|(x, _)| x).collect();
    if shared.is_empty() { ".".to_string() } else { shared.join("/") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_read() {
        assert!(reads("deploy/k8s/worker.yaml"));
        assert!(reads("apps/api/kustomize/overlays/prod/patch.yml"));
        assert!(reads("ops/kustomization.yaml"));
        assert!(!reads("deploy/k8s/chart/templates/deployment.yaml"), "a chart's templates are no YAML");
        assert!(!reads("config/settings.yaml"), "most YAML files are no manifests");
        assert!(!reads("k8s/README.md"));
        assert_eq!(common_dir("deploy/k8s/base", "deploy/k8s/overlays/prod"), "deploy/k8s");
        assert_eq!(common_dir("base", "overlays/prod"), ".");
    }
}
