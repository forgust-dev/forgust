//! Language breakdown of a tree by file extension: a small table of common
//! languages (colours as in GitHub's linguist), not a full classifier.
//! Documentation, vendored and generated paths are left out.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LanguageShare {
    pub name: String,
    /// `#rrggbb`
    pub color: String,
    pub bytes: u64,
}

/// (extensions or exact file names, language, colour)
const TABLE: &[(&[&str], &str, &str)] = &[
    (&["c", "h"], "C", "#555555"),
    (&["cc", "cpp", "cxx", "hh", "hpp", "hxx"], "C++", "#f34b7d"),
    (&["rs"], "Rust", "#dea584"),
    (&["go"], "Go", "#00add8"),
    (&["py", "pyi"], "Python", "#3572a5"),
    (&["js", "mjs", "cjs", "jsx"], "JavaScript", "#f1e05a"),
    (&["ts", "tsx", "mts", "cts"], "TypeScript", "#3178c6"),
    (&["java"], "Java", "#b07219"),
    (&["kt", "kts"], "Kotlin", "#a97bff"),
    (&["swift"], "Swift", "#f05138"),
    (&["cs"], "C#", "#178600"),
    (&["rb"], "Ruby", "#701516"),
    (&["php"], "PHP", "#4f5d95"),
    (&["pl", "pm"], "Perl", "#0298c3"),
    (&["sh", "bash", "zsh"], "Shell", "#89e051"),
    (&["s", "asm"], "Assembly", "#6e4c13"),
    (&["lua"], "Lua", "#000080"),
    (&["hs"], "Haskell", "#5e5086"),
    (&["ml", "mli"], "OCaml", "#ef7a08"),
    (&["scala"], "Scala", "#c22d40"),
    (&["dart"], "Dart", "#00b4ab"),
    (&["ex", "exs"], "Elixir", "#6e4a7e"),
    (&["erl", "hrl"], "Erlang", "#b83998"),
    (&["zig"], "Zig", "#ec915c"),
    (&["nix"], "Nix", "#7e7eff"),
    (&["vue"], "Vue", "#41b883"),
    (&["svelte"], "Svelte", "#ff3e00"),
    (&["html", "htm"], "HTML", "#e34c26"),
    (&["css"], "CSS", "#663399"),
    (&["scss", "sass"], "SCSS", "#c6538c"),
    (&["y", "yy"], "Yacc", "#4b6c4b"),
    (&["l", "lex"], "Lex", "#dbca00"),
    (&["dts", "dtsi"], "Device Tree", "#f0c674"),
    (&["cmake"], "CMake", "#da3434"),
    (&["mk", "Makefile", "makefile", "GNUmakefile", "Kbuild"], "Makefile", "#427819"),
    (&["Dockerfile"], "Dockerfile", "#384d54"),
    (&["sql"], "SQL", "#e38c00"),
    (&["tf"], "HCL", "#844fba"),
];

fn language_of(path: &str) -> Option<(&'static str, &'static str)> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e);
    TABLE.iter().find(|(keys, _, _)| keys.iter().any(|k| *k == name || Some(*k) == ext)).map(|(_, l, c)| (*l, *c))
}

/// Paths a language bar should ignore (documentation, vendored, generated).
fn ignored(path: &str) -> bool {
    const DIRS: &[&str] = &["Documentation/", "docs/", "doc/", "vendor/", "third_party/", "node_modules/", "dist/", "build/"];
    DIRS.iter().any(|d| path.starts_with(d) || path.contains(&format!("/{d}"))) || path.ends_with(".min.js") || path.ends_with(".min.css")
}

/// Bytes per language, largest first.
pub fn breakdown<'a>(files: impl IntoIterator<Item = (&'a str, u64)>) -> Vec<LanguageShare> {
    let mut by: std::collections::HashMap<&'static str, (u64, &'static str)> = Default::default();
    for (path, size) in files {
        if ignored(path) {
            continue;
        }
        if let Some((lang, color)) = language_of(path) {
            by.entry(lang).or_insert((0, color)).0 += size;
        }
    }
    let mut out: Vec<LanguageShare> =
        by.into_iter().map(|(name, (bytes, color))| LanguageShare { name: name.into(), color: color.into(), bytes }).collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_extension_and_name_skipping_docs() {
        let files =
            [("src/main.rs", 300), ("Makefile", 50), ("kernel/sched.c", 900), ("include/x.h", 100), ("docs/guide.py", 999), ("README", 10)];
        let b = breakdown(files.iter().map(|(p, s)| (*p, *s as u64)));
        let names: Vec<_> = b.iter().map(|l| (l.name.as_str(), l.bytes)).collect();
        assert_eq!(names, vec![("C", 1000), ("Rust", 300), ("Makefile", 50)]);
    }
}
