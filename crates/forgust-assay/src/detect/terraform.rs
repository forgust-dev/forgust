//! Terraform (and OpenTofu): each folder of `.tf` files is a module, a block, using the modules of
//! the repository it calls (`module "network" { source = "../modules/network" }`), and running
//! the images of the repository its resources name (`image = "registry/shop/api:1.2"`): a guess.
//! The modules Terraform downloads (`.terraform/`) are left out of every map.

use std::collections::BTreeMap;

use super::{Ctx, Draft, To, dir_of, join};
use crate::model::{Certainty, Kind, LinkKind};

pub(super) fn reads(path: &str) -> bool {
    path.ends_with(".tf") || path.ends_with(".tofu")
}

pub(super) fn detect(cx: &mut Ctx) {
    let mut modules: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for f in cx.files.iter().map(String::as_str).filter(|f| reads(f)) {
        modules.entry(dir_of(f)).or_default().push(f);
    }
    for (dir, files) in modules {
        let key = format!("terraform:{dir}");
        let mut d = Draft::new(key.clone(), &cx.folder_name(&dir), Kind::Terraform);
        d.roots.push(dir.clone());
        let main = ["main.tf", "main.tofu"].iter().map(|m| join(&dir, m)).find(|m| cx.exists(m));
        d.manifests.push(main.unwrap_or_else(|| files[0].to_string()));
        cx.add(d);
        for f in files {
            let Some(text) = cx.read(f) else { continue };
            // the modules of the repository it calls; the others come from a registry or a URL
            for source in module_sources(&text).into_iter().filter(|s| s.starts_with("./") || s.starts_with("../")) {
                cx.link(&key, To::Within(join(&dir, &source)), LinkKind::Uses, Certainty::Declared, f);
            }
            for image in values_of(&text, "image") {
                cx.link(&key, To::ImageNamed(image), LinkKind::Runs, Certainty::Inferred, f);
            }
        }
    }
}

/// The sources of the modules a file calls, each in its `module "name" { … }` block.
fn module_sources(hcl: &str) -> Vec<String> {
    let braces = |s: &str| s.matches('{').count() as i64 - s.matches('}').count() as i64;
    let mut out = Vec::new();
    // inside a module block: the braces open
    let mut depth = 0;
    for line in hcl.lines().map(str::trim).filter(|l| !l.starts_with('#') && !l.starts_with("//")) {
        if depth == 0 {
            let Some((head, body)) = line.split_once('{') else { continue };
            if !head.trim_end().strip_prefix("module").is_some_and(|r| r.starts_with([' ', '"'])) {
                continue;
            }
            // `module "x" { source = "./x" }`, on one line
            out.extend(assigned(body.trim(), "source"));
            depth = (1 + braces(body)).max(0);
            continue;
        }
        if depth == 1 {
            out.extend(assigned(line, "source"));
        }
        depth = (depth + braces(line)).max(0);
    }
    out
}

/// The values given to `key` (`image = "…"`), but those a variable makes.
fn values_of(hcl: &str, key: &str) -> Vec<String> {
    hcl.lines().map(str::trim).filter(|l| !l.starts_with('#')).filter_map(|l| assigned(l, key)).collect()
}

/// `key = "value"`: the value, unless it interpolates (`"${var.image}"`).
fn assigned(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim_start();
    let value = rest.strip_prefix('"')?.split('"').next()?;
    (!value.contains("${")).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_modules_and_images() {
        let hcl = r#"
terraform {
  required_version = ">= 1.6"
}

module "network" {
  source = "./modules/network"
  cidr   = "10.0.0.0/16"
  tags = {
    source = "not a module's"
  }
}

# module "old" {
#   source = "./modules/old"
# }
module "vpc" { source = "terraform-aws-modules/vpc/aws" }

resource "google_cloud_run_v2_service" "api" {
  template {
    containers {
      image = "registry.example.com/shop/api:1.2"
      image_pull_policy = "Always"
    }
    containers {
      image = "${var.registry}/worker:1.0"
    }
  }
}
"#;
        assert_eq!(module_sources(hcl), ["./modules/network", "terraform-aws-modules/vpc/aws"]);
        assert_eq!(values_of(hcl, "image"), ["registry.example.com/shop/api:1.2"]);
    }
}
