//! Go: a `go.mod` is a block (its folder), named after its module's last segment.

use super::{Ctx, Draft, dir_of, join};
use crate::model::{Kind, Role, holds};

/// Modules that serve HTTP or gRPC.
const SERVERS: &[&str] = &[
    "github.com/gin-gonic/gin",
    "github.com/labstack/echo",
    "github.com/gofiber/fiber",
    "github.com/go-chi/chi",
    "github.com/gorilla/mux",
    "github.com/beego/beego",
    "google.golang.org/grpc",
    "connectrpc.com/connect",
];
/// Modules a command line is built with.
const CLIS: &[&str] = &["github.com/spf13/cobra", "github.com/urfave/cli", "github.com/alecthomas/kong"];

pub(super) fn detect(cx: &mut Ctx) {
    for path in cx.named("go.mod") {
        let Some(text) = cx.read(path) else { continue };
        let dir = dir_of(path);
        let module = text.lines().find_map(|l| l.trim().strip_prefix("module ")).map(|m| m.trim().trim_matches('"').to_string());
        let name = module.as_deref().and_then(|m| m.rsplit('/').next()).map(str::to_string).unwrap_or_else(|| cx.folder_name(&dir));
        let required = requires(&text);
        // `github.com/labstack/echo/v4` is echo's
        let uses = |modules: &[&str]| {
            required.iter().any(|r| modules.iter().any(|m| r.strip_prefix(m).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))))
        };
        // a main package: main.go at the top, or commands under cmd/
        let cmd = join(&dir, "cmd");
        let main = cx.exists(&join(&dir, "main.go")) || cx.files.iter().any(|f| holds(&cmd, f));
        let mut d = Draft::new(format!("go:{dir}"), &name, Kind::Go);
        d.role = Some(if uses(SERVERS) {
            Role::Api
        } else if main || uses(CLIS) {
            Role::Cli
        } else {
            Role::Library
        });
        d.roots.push(dir);
        d.manifests.push(path.to_string());
        d.package = module;
        cx.add(d);
    }
}

/// The modules a go.mod requires itself: those marked `// indirect` are its requirements'.
fn requires(go_mod: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in go_mod.lines().map(str::trim) {
        let spec = if inside {
            if line.starts_with(')') {
                inside = false;
                continue;
            }
            line
        } else if let Some(rest) = line.strip_prefix("require")
            && rest.starts_with([' ', '\t', '('])
        {
            match rest.trim_start().strip_prefix('(') {
                Some(after) => {
                    inside = true;
                    after.trim()
                }
                None => rest.trim_start(),
            }
        } else {
            continue;
        };
        if spec.contains("// indirect") {
            continue;
        }
        if let Some(m) = spec.split_whitespace().next().filter(|m| !m.starts_with("//")) {
            out.push(m.trim_matches('"').to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_a_module_requires() {
        let go_mod = "module example.com/x\n\nrequire github.com/a/b v1.0.0\n\nrequire (\n\tgithub.com/c/d/v2 v2.1.0\n\t// a comment\n\tgithub.com/e/f v0.1.0 // indirect\n)\n";
        assert_eq!(requires(go_mod), ["github.com/a/b", "github.com/c/d/v2"]);
        assert!(requires("module requirements\n").is_empty());
    }
}
