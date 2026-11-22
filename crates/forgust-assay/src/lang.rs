//! A file's language, by its name: what a block's languages count.

pub fn of(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if crate::detect::is_dockerfile(name) {
        return Some("Dockerfile");
    }
    if name == "Makefile" || name == "makefile" || name == "GNUmakefile" {
        return Some("Makefile");
    }
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" | "pyi" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" => "Scala",
        "rb" => "Ruby",
        "php" => "PHP",
        "cs" => "C#",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "swift" => "Swift",
        "dart" => "Dart",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "lua" => "Lua",
        "zig" => "Zig",
        "nix" => "Nix",
        "sql" => "SQL",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" => "PowerShell",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "SCSS",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "md" | "mdx" => "Markdown",
        "proto" => "Protocol Buffers",
        "tf" | "hcl" => "HCL",
        "toml" => "TOML",
        "yml" | "yaml" => "YAML",
        "json" => "JSON",
        _ => return None,
    })
}
