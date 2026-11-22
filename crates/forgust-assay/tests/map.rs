//! `forgust_assay::map` over small repositories written here: a Cargo workspace with an API and a
//! library, a web app using a package of an npm workspace, a schema, an image, compose
//! services, a GitLab CI, CODEOWNERS and assay.toml, with a git history of three people. Then
//! what assay.toml corrects, and the roles assay guesses, as a forge hands them over.

use std::path::{Path, PathBuf};
use std::process::Command;

use std::collections::HashMap;

use forgust_assay::{Certainty, Commit, Kind, LinkKind, Options, RepoMap, Role, Snapshot, Source};

fn repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

fn git(dir: &Path, args: &[&str], author: (&str, &str)) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", author.0)
        .env("GIT_AUTHOR_EMAIL", author.1)
        .env("GIT_COMMITTER_NAME", author.0)
        .env("GIT_COMMITTER_EMAIL", author.1)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

/// Changes `path` and commits it as `author`.
fn commit(dir: &Path, path: &str, author: (&str, &str)) {
    let p = dir.join(path);
    let mut text = std::fs::read_to_string(&p).unwrap_or_default();
    text.push_str("// more\n");
    std::fs::write(&p, text).unwrap();
    git(dir, &["add", "-A"], author);
    git(dir, &["commit", "-q", "-m", &format!("change {path}")], author);
}

fn link(map: &RepoMap, from: &str, to: &str) -> Option<(LinkKind, Certainty)> {
    map.links.iter().find(|l| l.from == from && l.to == to).map(|l| (l.kind, l.certainty))
}

const ADA: (&str, &str) = ("Ada", "ada@example.com");
const BOB: (&str, &str) = ("Bob", "bob@example.com");
const ZOE: (&str, &str) = ("Zoé", "zoe@example.com");

fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\ncore = { path = \"crates/core\" }\n"),
        (
            "crates/api/Cargo.toml",
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\n\n[dependencies]\naxum = \"0.8\"\ncore.workspace = true\n\n[dev-dependencies]\ntest-kit = { path = \"../test-kit\" }\n",
        ),
        ("crates/api/src/main.rs", "fn main() {}\n"),
        ("crates/core/Cargo.toml", "[package]\nname = \"core\"\nversion = \"0.1.0\"\n"),
        ("crates/core/src/lib.rs", "pub fn f() {}\n"),
        ("crates/core/migrations/0001_init.sql", "create table users (id int);\n"),
        ("crates/test-kit/Cargo.toml", "[package]\nname = \"test-kit\"\nversion = \"0.1.0\"\n"),
        ("crates/test-kit/src/lib.rs", "\n"),
        ("package.json", "{\"private\": true, \"workspaces\": [\"web\", \"packages/*\"]}\n"),
        ("web/package.json", "{\"name\": \"web\", \"dependencies\": {\"react\": \"19\", \"@demo/ui\": \"workspace:*\"}}\n"),
        ("web/src/main.tsx", "export {};\n"),
        ("web/tools/gen/package.json", "{\"name\": \"gen\", \"private\": true}\n"),
        ("packages/ui/package.json", "{\"name\": \"@demo/ui\", \"main\": \"index.js\"}\n"),
        ("packages/ui/index.js", "export {};\n"),
        (
            "Dockerfile",
            "FROM rust AS build\nCOPY Cargo.toml ./\nCOPY crates crates\nRUN cargo build --release -p api\nFROM debian\nCOPY --from=build /app /app\n",
        ),
        (
            "compose.yml",
            "services:\n  api:\n    build: .\n    depends_on: [db]\n    environment:\n      DATABASE_URL: postgres://app:pw@db:5432/app\n  db:\n    image: postgres:17\n    environment:\n      POSTGRES_USER: api\n  web:\n    build: web\n    environment:\n      - API_URL=http://api:8080\n",
        ),
        ("web/Dockerfile", "FROM node\nCOPY . .\nRUN npm ci\n"),
        // the ignore file of the Dockerfile beside it, no Dockerfile
        ("Dockerfile.dockerignore", "target\n"),
        (
            ".gitlab-ci.yml",
            "stages: [test]\n.base:\n  image: rust\n.core:\n  rules:\n    - changes: [\"crates/core/**/*\"]\napi-test:\n  script: [cargo test]\n  rules:\n    - changes: [\"crates/api/**/*\"]\ncore-test:\n  extends: [.base, .core]\n  script: [cargo test -p core]\ndocs-build:\n  extends: .core\n  rules:\n    - changes: [\"docs/**/*\"]\nweb-build:\n  script: [npm run build]\n  rules:\n    - changes:\n        paths: [\"web/**/*\"]\nlint:\n  script: [true]\n",
        ),
        ("CODEOWNERS", "/web/ @front\n"),
        ("docs/index.md", "# Docs\n"),
        ("README.md", "# Demo\n"),
        // test data that looks like a project: left out
        ("crates/core/tests/fixtures/sample/Cargo.toml", "[package]\nname = \"sample\"\n"),
        (
            "assay.toml",
            "version = 1\n\n[blocks.web]\nname = \"Front\"\nreferents = [\"@ada\"]\njobs = [\"e2e-*\", \"web-release\"]\n\n[blocks.gen]\nignore = true\n\n[[links]]\nfrom = \"web\"\nto = \"api\"\n",
        ),
    ]
}

#[test]
fn maps_a_repository() {
    let dir = repo("full", &files());
    git(&dir, &["init", "-q", "-b", "main"], ADA);
    git(&dir, &["add", "-A"], ADA);
    git(&dir, &["commit", "-q", "-m", "first"], ADA);
    for _ in 0..3 {
        commit(&dir, "crates/api/src/main.rs", BOB);
    }
    commit(&dir, "web/src/main.tsx", ADA);
    // a path git quotes, unless told -z
    commit(&dir, "crates/core/src/été.rs", ZOE);

    let map = forgust_assay::map(&dir, &Options::default()).unwrap();
    let ids: Vec<&str> = map.blocks.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "api",
            "core",
            "test-kit",
            "demo-ui",
            "web",
            "core-migrations",
            "image",
            "image-web",
            "api-service",
            "db",
            "web-service",
            "ci",
            "docs"
        ]
    );
    let block = |id: &str| map.blocks.iter().find(|b| b.id == id).unwrap();

    // packages, their roles and the links their manifests give
    assert_eq!((block("api").kind, block("api").role), (Kind::Rust, Some(Role::Api)));
    assert_eq!(block("core").role, Some(Role::Library));
    assert_eq!(block("demo-ui").role, Some(Role::Library), "an npm package with a main");
    assert_eq!((block("web").name.as_str(), block("web").role), ("Front", Some(Role::Web)), "assay.toml renamed it");
    assert_eq!(link(&map, "api", "core"), Some((LinkKind::Uses, Certainty::Declared)), "a workspace dependency by path");
    assert_eq!(link(&map, "api", "test-kit"), None, "dev-dependencies are left out");
    assert_eq!(link(&map, "web", "demo-ui"), Some((LinkKind::Uses, Certainty::Declared)), "a package of the npm workspace");
    assert!(!ids.contains(&"gen"), "ignored by assay.toml");
    assert!(!ids.contains(&"sample"), "fixtures are left out");

    // the schema, inside its crate
    let schema = block("core-migrations");
    assert_eq!(
        (schema.kind, schema.role, schema.parent.as_deref(), schema.files),
        (Kind::Migrations, Some(Role::Database), Some("core"), 1)
    );
    assert_eq!(block("core").files, 3, "its migrations are the schema's");

    // the image builds only what its cargo build names, though it copies the workspace
    assert_eq!(link(&map, "image", "api"), Some((LinkKind::Builds, Certainty::Declared)));
    assert_eq!(link(&map, "image", "core"), None);
    assert_eq!(link(&map, "image-web", "web"), Some((LinkKind::Builds, Certainty::Declared)));
    assert_eq!(block("image").roots, ["Dockerfile", "Dockerfile.dockerignore"], "its ignore file is its own, and no image");

    // services: what they run, need and reach; the image a name like api's is no host of
    assert_eq!(block("db").role, Some(Role::Database));
    assert_eq!(link(&map, "api-service", "image"), Some((LinkKind::Runs, Certainty::Declared)));
    assert_eq!(link(&map, "api-service", "db"), Some((LinkKind::DependsOn, Certainty::Declared)), "a guess too, but declared wins");
    assert_eq!(link(&map, "web-service", "image-web"), Some((LinkKind::Runs, Certainty::Declared)));
    assert_eq!(link(&map, "web-service", "api-service"), Some((LinkKind::Connects, Certainty::Inferred)));
    assert_eq!(link(&map, "db", "api-service"), None, "POSTGRES_USER=api names no host");
    assert_eq!(link(&map, "web", "api"), Some((LinkKind::Calls, Certainty::Declared)), "declared by assay.toml");

    // CI jobs by their paths, and by assay.toml
    assert_eq!(block("api").jobs, ["api-test"]);
    assert_eq!(block("web").jobs, ["web-build", "web-release"], "a glob that matches no job adds none; a plain name does");
    assert_eq!(block("core").jobs, ["core-test"], "the rules of what it extends");
    assert_eq!(block("docs").jobs, ["docs-build"], "its own rules replace those it extends");
    assert_eq!(block("ci").roots, [".gitlab-ci.yml"]);

    // who to ask
    assert_eq!(block("web").owners, ["@front", "@ada"], "CODEOWNERS, then assay.toml's referents");
    let api = block("api");
    assert_eq!(api.people.iter().map(|p| (p.name.as_str(), p.commits)).collect::<Vec<_>>(), [("Bob", 3), ("Ada", 1)]);
    assert_eq!(api.bus_factor, Some(2), "Ada made a fourth of its commits: two people know it");
    assert_eq!(block("web").bus_factor, Some(1));
    assert!(block("core").people.iter().any(|p| p.name == "Zoé"), "{:?}", block("core").people);

    // a file's block
    assert_eq!(map.block_of("crates/core/migrations/0001_init.sql").map(|b| b.id.as_str()), Some("core-migrations"));
    assert_eq!(map.block_of("web/src/main.tsx").map(|b| b.id.as_str()), Some("web"));
    assert!(map.block_of("README.md").is_none());
    assert!(map.unassigned.sample.contains(&"README.md".to_string()));
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    assert!(map.commit.is_some());

    // the JSON reads back
    let json = serde_json::to_string(&map).unwrap();
    let back: RepoMap = serde_json::from_str(&json).unwrap();
    assert_eq!(back.blocks.len(), map.blocks.len());
    assert_eq!(back.schema, forgust_assay::SCHEMA);
}

#[test]
fn maps_a_folder_out_of_git() {
    let dir = repo(
        "plain",
        &[
            ("go.mod", "module example.com/tools/sync\n\ngo 1.24\n"),
            ("main.go", "package main\n"),
            ("pyproject.toml", "[tool.ruff]\nline-length = 100\n"),
        ],
    );
    let map = forgust_assay::map(&dir, &Options::default()).unwrap();
    let ids: Vec<&str> = map.blocks.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["sync"], "a pyproject.toml of tools' settings is no Python project");
    assert_eq!(map.blocks[0].files, 3, "the module's folder is the top: it holds them all");
    assert!(map.blocks[0].people.is_empty() && map.commit.is_none(), "no history out of git");
    assert_eq!(map.blocks[0].source, Source::Detected);
    assert_eq!(map.blocks[0].role, Some(Role::Cli), "a main.go: a command");
}

#[test]
fn reports_what_it_cannot_read() {
    let dir = repo(
        "broken",
        &[
            ("crates/a/Cargo.toml", "[package\nname = \"a\""),
            ("py/pyproject.toml", "[project\nname = \"p\""),
            ("assay.toml", "[blocks.nothing]\nname = \"x\"\n"),
        ],
    );
    let map = forgust_assay::map(&dir, &Options { people: false, days: None }).unwrap();
    assert!(map.blocks.is_empty());
    assert!(map.warnings.iter().any(|w| w.starts_with("crates/a/Cargo.toml: invalid TOML")), "{:?}", map.warnings);
    assert!(map.warnings.iter().any(|w| w.starts_with("py/pyproject.toml: invalid TOML")), "{:?}", map.warnings);
    assert!(map.warnings.iter().any(|w| w.contains("no block `nothing`")), "{:?}", map.warnings);
    let typo = repo("typo", &[("assay.toml", "[blocks.a]\nrenamed = \"x\"\n")]);
    assert!(forgust_assay::map(&typo, &Options::default()).is_err(), "an unknown key in assay.toml stops the map");
}

/// What a forge does: no folder and no git, but the paths, the text of the files assay reads
/// and the history, gathered from its storage.
#[test]
fn maps_a_snapshot() {
    let all = files();
    let texts: HashMap<String, String> =
        all.iter().filter(|(p, _)| forgust_assay::reads(p)).map(|(p, t)| (p.to_string(), t.to_string())).collect();
    assert!(!texts.contains_key("crates/api/src/main.rs") && !texts.contains_key("README.md"), "sources are not read");
    assert!(!texts.contains_key("crates/core/tests/fixtures/sample/Cargo.toml"), "nor what every map leaves out");
    let change = |who: (&str, &str), time: i64, path: &str| Commit {
        name: who.0.to_string(),
        email: who.1.to_string(),
        time,
        files: vec![path.to_string()],
    };
    let history = vec![
        change(ADA, 1_700_000_400, "web/src/main.tsx"),
        change(BOB, 1_700_000_300, "crates/api/src/main.rs"),
        change(BOB, 1_700_000_200, "crates/api/Cargo.toml"),
        change(("dependabot[bot]", "bot@example.com"), 1_700_000_100, "crates/api/Cargo.toml"),
        change(ADA, 1_700_000_000, "crates/api/src/main.rs"),
    ];
    let snapshot = Snapshot {
        name: "demo".to_string(),
        commit: Some("0123abc".to_string()),
        files: all.iter().map(|(p, _)| p.to_string()).collect(),
        texts,
        history: Some(history),
    };
    let map = forgust_assay::map_snapshot(&snapshot).unwrap();
    let from_folder = forgust_assay::map(&repo("snapshot", &all), &Options { people: false, days: None }).unwrap();
    let ids = |m: &RepoMap| m.blocks.iter().map(|b| b.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&map), ids(&from_folder), "the same blocks as from the folder");
    assert_eq!(map.links, from_folder.links, "and the same links");
    assert_eq!(map.commit.as_deref(), Some("0123abc"));
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);

    let api = map.blocks.iter().find(|b| b.id == "api").unwrap();
    let people: Vec<(&str, usize, &str)> = api.people.iter().map(|p| (p.name.as_str(), p.commits, p.last.as_str())).collect();
    assert_eq!(people, [("Bob", 2, "2023-11-14"), ("Ada", 1, "2023-11-14")], "the bot is left out");
    assert_eq!(map.blocks.iter().find(|b| b.id == "web").unwrap().owners, ["@front", "@ada"]);

    // what was not handed over is said so; an assay.toml that was not, stops the map
    let mut partial = snapshot.clone();
    partial.texts.remove("web/package.json");
    let map = forgust_assay::map_snapshot(&partial).unwrap();
    assert!(map.warnings.contains(&"web/package.json: could not be read".to_string()), "{:?}", map.warnings);
    partial.texts.remove(forgust_assay::CONFIG_FILE);
    assert!(forgust_assay::map_snapshot(&partial).is_err());
}

#[test]
fn says_what_it_reads() {
    for path in ["Cargo.toml", "web/package.json", "Dockerfile", "deploy/api.Dockerfile", "compose.yaml", ".gitlab-ci.yml"] {
        assert!(forgust_assay::reads(path), "{path}");
    }
    for path in [".github/workflows/ci.yml", ".github/CODEOWNERS", "assay.toml", "py/pyproject.toml", "go.mod"] {
        assert!(forgust_assay::reads(path), "{path}");
    }
    for path in [
        "src/main.rs",
        "README.md",
        "web/node_modules/x/package.json",
        "vendor/a/go.mod",
        ".github/dependabot.yml",
        "sub/assay.toml",
        "Dockerfile.dockerignore",
    ] {
        assert!(!forgust_assay::reads(path), "{path}");
    }
    let mut texts = HashMap::new();
    assert_eq!(forgust_assay::history_days(&texts), forgust_assay::DAYS);
    texts.insert("assay.toml".to_string(), "[people]\ndays = 90\n".to_string());
    assert_eq!(forgust_assay::history_days(&texts), 90);
}

/// What assay.toml corrects and declares, reached by the links found: as a forge hands it over.
#[test]
fn follows_what_assay_toml_corrects() {
    let files: &[(&str, &str)] = &[
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
        ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
        ("crates/a/src/lib.rs", ""),
        ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ("crates/b/src/lib.rs", ""),
        ("crates/c/Cargo.toml", "[package]\nname = \"c\"\n\n[dependencies]\na = { path = \"../a\" }\nb = { path = \"../b\" }\n"),
        ("crates/c/src/main.rs", ""),
        ("crates/d/Cargo.toml", "[package]\nname = \"d\"\n\n[dependencies]\na = { path = \"../a\" }\n"),
        ("crates/d/src/lib.rs", ""),
        ("site/index.html", ""),
        ("Dockerfile", "FROM nginx\nCOPY site /usr/share/nginx/html\n"),
        (
            "deploy/b.Dockerfile",
            "FROM rust\nCOPY Cargo.toml ./\nCOPY crates/a crates/a\nCOPY crates/b crates/b\nCOPY crates/c crates/c\nRUN cargo build -p b\n",
        ),
        (
            "compose.yml",
            "services:\n  front:\n    image: nginx\n    environment:\n      BACKEND_URL: http://back:8080\n  back:\n    build:\n      context: .\n      dockerfile: deploy/b.Dockerfile\n",
        ),
        (
            "assay.toml",
            "[blocks.b]\nmerge_into = \"a\"\n\n[blocks.site]\nkind = \"docs\"\npaths = [\"site/\"]\n\n[[links]]\nfrom = \"d\"\nto = \"a\"\nremove = true\n\n[[links]]\nfrom = \"front\"\nto = \"back\"\nkind = \"connects\"\n\n[people]\nexclude = [\"eve@work.example\"]\n",
        ),
    ];
    let change = |name: &str, email: &str| Commit {
        name: name.into(),
        email: email.into(),
        time: 1_700_000_000,
        files: vec!["crates/d/src/lib.rs".into()],
    };
    let map = forgust_assay::map_snapshot(&Snapshot {
        name: "fixes".to_string(),
        files: files.iter().map(|(p, _)| p.to_string()).collect(),
        texts: files.iter().filter(|(p, _)| forgust_assay::reads(p)).map(|(p, t)| (p.to_string(), t.to_string())).collect(),
        history: Some(vec![change("Eve", "eve@work.example"), change("Eve", "eve@home.example"), change("Ada", "ada@example.com")]),
        ..Default::default()
    })
    .unwrap();
    assert!(map.warnings.is_empty(), "{:?}", map.warnings);
    let block = |id: &str| map.blocks.iter().find(|b| b.id == id).unwrap_or_else(|| panic!("no block {id}"));

    // b is part of a: what uses b uses a, a build naming b builds a
    assert!(map.blocks.iter().all(|b| b.id != "b"));
    assert_eq!(block("a").roots, ["crates/a", "crates/b"]);
    assert_eq!(link(&map, "c", "a"), Some((LinkKind::Uses, Certainty::Declared)));
    assert_eq!(link(&map, "image-b", "a"), Some((LinkKind::Builds, Certainty::Declared)));
    assert_eq!(link(&map, "image-b", "c"), None, "copied for the workspace, not built");
    assert_eq!(link(&map, "back", "image-b"), Some((LinkKind::Runs, Certainty::Declared)));

    // a block only assay.toml gives is reached as any other
    assert_eq!(block("site").source, Source::Configured);
    assert_eq!(link(&map, "image", "site"), Some((LinkKind::Builds, Certainty::Declared)));

    // a wrong link taken away, a guess confirmed
    assert_eq!(link(&map, "d", "a"), None);
    assert_eq!(link(&map, "front", "back"), Some((LinkKind::Connects, Certainty::Declared)));

    // Eve opted out under one of her addresses: she is left out under both
    assert_eq!(block("d").people.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Ada"]);
}

/// What each block is for, in a line: its manifest's, else assay.toml's.
#[test]
fn reads_descriptions() {
    let files: &[(&str, &str)] = &[
        ("api/Cargo.toml", "[package]\nname = \"api\"\ndescription = \"  The HTTP API  \"\n"),
        ("web/package.json", r#"{"name": "web", "description": "The shop"}"#),
        ("py/pyproject.toml", "[project]\nname = \"py\"\ndescription = \"Search\"\n"),
        ("go/go.mod", "module example.com/worker\n"),
        ("Dockerfile", "FROM alpine\nLABEL org.opencontainers.image.description=\"The image we ship\"\n"),
        ("compose.yml", "services:\n  db:\n    image: postgres:17\n"),
        (
            "assay.toml",
            "[blocks.worker]\ndescription = \"Ships orders\"\n\n[blocks.db]\ndescription = \"Orders and products\"\n\n[blocks.api]\ndescription = \"The API\"\n",
        ),
    ];
    let map = forgust_assay::map_snapshot(&Snapshot {
        name: "described".to_string(),
        files: files.iter().map(|(p, _)| p.to_string()).collect(),
        texts: files.iter().filter(|(p, _)| forgust_assay::reads(p)).map(|(p, t)| (p.to_string(), t.to_string())).collect(),
        ..Default::default()
    })
    .unwrap();
    let said: Vec<(&str, Option<&str>)> = map.blocks.iter().map(|b| (b.id.as_str(), b.description.as_deref())).collect();
    assert_eq!(
        said,
        [
            ("api", Some("The API")),
            ("web", Some("The shop")),
            ("py", Some("Search")),
            ("worker", Some("Ships orders")),
            ("image", Some("The image we ship")),
            ("db", Some("Orders and products")),
        ],
        "assay.toml's comes first, then the manifest's, trimmed; a go.mod or a service has none of its own"
    );
}

/// Each package's role, from its manifest: a guess, which assay.toml corrects.
#[test]
fn guesses_roles() {
    let files: &[(&str, &str)] = &[
        ("rs/Cargo.toml", "[package]\nname = \"front\"\n\n[dependencies]\nleptos = \"0.8\"\n"),
        ("app/package.json", r#"{"name": "app", "dependencies": {"react": "19"}}"#),
        ("lib/package.json", r#"{"name": "lib", "exports": "./index.js"}"#),
        ("tool/package.json", r#"{"name": "tool", "private": true, "scripts": {"gen": "node gen.js"}}"#),
        ("py/api/pyproject.toml", "[project]\nname = \"pyapi\"\ndependencies = [\"fastapi>=0.110\"]\n"),
        ("py/app/pyproject.toml", "[project]\nname = \"pyapp\"\ndependencies = [\"pandas\"]\n"),
        ("py/cli/pyproject.toml", "[project]\nname = \"pycli\"\n\n[project.scripts]\npycli = \"pycli:main\"\n"),
        ("py/lib/pyproject.toml", "[project]\nname = \"pylib\"\n\n[build-system]\nrequires = [\"hatchling\"]\n"),
        (
            "go/api/go.mod",
            "module example.com/goapi\n\nrequire (\n\tgithub.com/labstack/echo/v4 v4.12.0\n\tgithub.com/spf13/cobra v1.8.0 // indirect\n)\n",
        ),
        ("go/cli/go.mod", "module example.com/gocli\n"),
        ("go/cli/cmd/gocli/main.go", "package main\n"),
        ("go/lib/go.mod", "module example.com/golib\n\nrequire github.com/spf13/cobra v1.8.0 // indirect\n"),
        ("go/lib/lib.go", "package golib\n"),
    ];
    let map = forgust_assay::map_snapshot(&Snapshot {
        name: "roles".to_string(),
        files: files.iter().map(|(p, _)| p.to_string()).collect(),
        texts: files.iter().filter(|(p, _)| forgust_assay::reads(p)).map(|(p, t)| (p.to_string(), t.to_string())).collect(),
        ..Default::default()
    })
    .unwrap();
    let roles: Vec<(&str, Option<Role>)> = map.blocks.iter().map(|b| (b.id.as_str(), b.role)).collect();
    assert_eq!(
        roles,
        [
            ("front", Some(Role::Web)),
            ("app", Some(Role::Web)),
            ("lib", Some(Role::Library)),
            ("tool", None),
            ("pyapi", Some(Role::Api)),
            ("pyapp", None),
            ("pycli", Some(Role::Cli)),
            ("pylib", Some(Role::Library)),
            ("goapi", Some(Role::Api)),
            ("gocli", Some(Role::Cli)),
            ("golib", Some(Role::Library)),
        ]
    );
}
