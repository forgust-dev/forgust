//! Docker: a Dockerfile is a block (the file, and the .dockerignore beside it), linked to the
//! blocks whose files it copies into the image.

use super::{Ctx, Draft, To, dir_of, file_name, join, literal_prefix, slug};
use crate::model::{Certainty, Kind, LinkKind, depth, holds};

/// Whether a file is a Dockerfile, by its name. `Dockerfile.dockerignore` is none: it is the
/// ignore file of the Dockerfile beside it.
pub(crate) fn is_dockerfile(name: &str) -> bool {
    let named = name == "Dockerfile"
        || name == "Containerfile"
        || name.starts_with("Dockerfile.")
        || name.ends_with(".Dockerfile")
        || name.ends_with(".dockerfile");
    named && !name.ends_with(".dockerignore")
}

/// The key of the block a Dockerfile gives: what a compose `build` points at.
pub(super) fn key(path: &str) -> String {
    format!("docker:{path}")
}

pub(super) fn detect(cx: &mut Ctx) {
    let found: Vec<&str> = cx.files.iter().map(String::as_str).filter(|f| is_dockerfile(file_name(f))).collect();
    for path in found {
        let dir = dir_of(path);
        let key = key(path);
        let mut d = Draft::new(key.clone(), path, Kind::Docker);
        // `Dockerfile` at the top is the image; `api.Dockerfile` or `Dockerfile.api` is api's
        d.want_id = match file_name(path) {
            "Dockerfile" | "Containerfile" if dir == "." => "image".to_string(),
            "Dockerfile" | "Containerfile" => format!("image-{}", slug(file_name(&dir))),
            n => format!(
                "image-{}",
                slug(n.trim_end_matches(".Dockerfile").trim_end_matches(".dockerfile").trim_start_matches("Dockerfile."))
            ),
        };
        // the names it may be pulled by: its folder's (the repository's, at the top), its own
        // (`api.Dockerfile`), the package's whose folder holds it
        d.aliases.push(cx.folder_name(&dir));
        d.aliases.extend(d.want_id.strip_prefix("image-").map(str::to_string));
        d.aliases.extend(package_names(cx, &dir));
        d.roots.push(path.to_string());
        for ignore in [join(&dir, ".dockerignore"), format!("{path}.dockerignore")] {
            if cx.exists(&ignore) && !d.roots.contains(&ignore) {
                d.roots.push(ignore);
            }
        }
        d.manifests.push(path.to_string());
        // a Dockerfile that cannot be read is still an image: one without links
        let steps = cx.read(path).map(|t| instructions(&t));
        d.description = steps.as_deref().and_then(description);
        cx.add(d);
        let Some(steps) = steps else { continue };
        // the Cargo packages and binaries its builds name: of a workspace copied whole, the
        // image has only those
        let built: Vec<String> = steps.iter().filter(|(i, _)| i == "RUN").flat_map(|(_, args)| cargo_targets(args)).collect();
        for (instruction, args) in steps {
            if instruction != "COPY" && instruction != "ADD" {
                continue;
            }
            let args = arguments(&args);
            // from another stage of the build, not from the repository
            if args.iter().any(|a| a.starts_with("--from")) {
                continue;
            }
            let args: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
            let Some((_, sources)) = args.split_last() else { continue };
            for src in sources {
                if src.contains("://") || src.contains('$') {
                    continue;
                }
                let prefix = literal_prefix(src);
                // from the Dockerfile's folder, else from the top (`docker build -f x/Dockerfile .`)
                let target = [join(&dir, &prefix), join(".", &prefix)].into_iter().find(|t| cx.any_at(t));
                if let Some(target) = target {
                    let to = if built.is_empty() { To::Within(target) } else { To::Built { path: target, names: built.clone() } };
                    cx.link(&key, to, LinkKind::Builds, Certainty::Declared, path);
                }
            }
        }
    }
}

/// The names of the package whose folder holds `dir` (the deepest): its own and its manifest's,
/// without an npm scope or a Go module's path (`@shop/web` is `web`).
fn package_names(cx: &Ctx, dir: &str) -> Vec<String> {
    let package = |k: Kind| matches!(k, Kind::Rust | Kind::Node | Kind::Python | Kind::Go);
    let holder = cx
        .drafts
        .iter()
        .filter(|d| package(d.kind))
        .filter_map(|d| d.roots.iter().filter(|r| holds(r, dir)).map(|r| depth(r)).max().map(|at| (d, at)))
        .max_by_key(|(_, at)| *at)
        .map(|(d, _)| d);
    let Some(d) = holder else { return Vec::new() };
    let short = |n: &str| n.rsplit('/').next().unwrap_or(n).to_string();
    std::iter::once(short(&d.name)).chain(d.package.as_deref().map(short)).collect()
}

/// The label OCI gives an image's description by.
const DESCRIPTION_LABEL: &str = "org.opencontainers.image.description";

/// What the image is for, by its description label (the last one given).
fn description(steps: &[(String, String)]) -> Option<String> {
    let labels = steps.iter().filter(|(i, _)| i == "LABEL").flat_map(|(_, args)| labels(args));
    super::described(labels.filter(|(k, _)| k == DESCRIPTION_LABEL).map(|(_, v)| v).next_back().as_deref())
}

/// The `key=value` pairs of a LABEL, a value quoted or not (`a="b c" d=e`). The old `LABEL key
/// value` form is not read.
fn labels(args: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = args.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut key = String::new();
        while let Some(c) = chars.next_if(|c| *c != '=' && !c.is_whitespace()) {
            key.push(c);
        }
        if key.is_empty() || chars.next_if_eq(&'=').is_none() {
            break;
        }
        let mut value = String::new();
        match chars.next_if(|c| *c == '"' || *c == '\'') {
            Some(quote) => {
                while let Some(c) = chars.next() {
                    match c {
                        c if c == quote => break,
                        '\\' if quote == '"' => value.extend(chars.next()),
                        c => value.push(c),
                    }
                }
            }
            None => {
                while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                    value.push(c);
                }
            }
        }
        out.push((key.trim_matches('"').to_string(), value));
    }
    out
}

/// The packages (`-p x`, `--package x`) and binaries (`--bin y`) a `cargo build` or `cargo
/// install` names, as `x` and `y` alike.
fn cargo_targets(run: &str) -> Vec<String> {
    let mut out = Vec::new();
    // one RUN may chain commands: each `cargo` up to its `&&`, `;` or `|`
    for cmd in run.split(['&', ';', '|']) {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let Some(at) = words.iter().position(|w| *w == "cargo") else { continue };
        if !words[at..].iter().any(|w| *w == "build" || *w == "install") {
            continue;
        }
        let mut it = words[at..].iter();
        while let Some(w) = it.next() {
            match *w {
                "-p" | "--package" | "--bin" => out.extend(it.next().map(|n| n.to_string())),
                w => {
                    if let Some(n) = w.strip_prefix("--package=").or_else(|| w.strip_prefix("--bin=")) {
                        out.push(n.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Each instruction with its arguments, lines continued with `\` joined, comments dropped.
fn instructions(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if current.is_empty() && (line.is_empty() || line.starts_with('#')) {
            continue;
        }
        if let Some(head) = line.strip_suffix('\\') {
            current.push_str(head);
            current.push(' ');
            continue;
        }
        current.push_str(line);
        let (instr, args) = current.trim().split_once(char::is_whitespace).unwrap_or((current.trim(), ""));
        out.push((instr.to_ascii_uppercase(), args.trim().to_string()));
        current.clear();
    }
    out
}

/// `["a", "b"]` (the exec form) or `a b`.
fn arguments(args: &str) -> Vec<String> {
    if args.starts_with('[')
        && let Ok(list) = serde_json::from_str::<Vec<String>>(args)
    {
        return list;
    }
    args.split_whitespace().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_a_dockerfile_by_its_name() {
        for name in ["Dockerfile", "Containerfile", "Dockerfile.api", "api.Dockerfile", "api.dockerfile"] {
            assert!(is_dockerfile(name), "{name}");
        }
        for name in
            ["Dockerfile.dockerignore", "Dockerfile.api.dockerignore", "api.Dockerfile.dockerignore", ".dockerignore", "Dockerfiles"]
        {
            assert!(!is_dockerfile(name), "{name}");
        }
    }

    #[test]
    fn reads_copies_across_lines() {
        let text = "FROM rust AS build\n# deps first\nCOPY Cargo.toml \\\n  Cargo.lock ./\nCOPY --from=build /out /out\ncopy [\"web\", \"/web\"]\n";
        let got = instructions(text);
        assert_eq!(got.len(), 4, "the comment is no instruction");
        assert_eq!(got[1].0, "COPY");
        assert_eq!(arguments(&got[1].1), vec!["Cargo.toml", "Cargo.lock", "./"]);
        assert_eq!(got[3].0, "COPY", "instructions are read in any case");
        assert_eq!(arguments(&got[3].1), vec!["web", "/web"]);
    }

    #[test]
    fn reads_the_description_label() {
        assert_eq!(
            labels(r#"org.opencontainers.image.description="The API, \"built\"" version=1 'x'='y z'"#),
            [
                ("org.opencontainers.image.description".to_string(), "The API, \"built\"".to_string()),
                ("version".to_string(), "1".to_string()),
                ("'x'".to_string(), "y z".to_string())
            ]
        );
        let steps = instructions(
            "FROM alpine\nLABEL org.opencontainers.image.description=\"Old\"\nLABEL maintainer=a \\\n  org.opencontainers.image.description='The web, served'\n",
        );
        assert_eq!(description(&steps).as_deref(), Some("The web, served"), "the last one given");
        assert_eq!(description(&instructions("LABEL org.opencontainers.image.description=\"  \"\n")), None);
    }

    #[test]
    fn reads_what_cargo_builds() {
        assert_eq!(
            cargo_targets("--mount=type=cache,target=/x cargo auditable build --release --locked -p forgust-server && cp a b"),
            vec!["forgust-server"]
        );
        assert_eq!(cargo_targets("cargo build --bin=forgust --package api"), vec!["forgust", "api"]);
        assert!(cargo_targets("cargo install --locked cargo-auditable@1.0").is_empty(), "a tool installed is no package of the repository");
        assert!(cargo_targets("npm run build -p x").is_empty());
    }
}
