# assay

Map any repository into blocks: what can be built, run or deployed apart (a package, an image, a service, a database's schema, the CI, the docs), the links between them, and the people who know them. The engine of Forgust's Smart View: a library, which the server calls, and a command, `assay`. It reads manifests, so it runs on any repository, whatever its forge.

From the repository's root:

```sh
cargo run -p forgust-assay -- view ../some-repo --open    # a page to look at, in the browser
cargo run -p forgust-assay -- map ../some-repo --text     # a summary to read
cargo run -p forgust-assay -- map ../some-repo --pretty   # the JSON (schema 1)
```

```
assay view [PATH] [-o FILE] [--open] [--no-people] [--days N]
assay map [PATH] [--text | --pretty] [--no-people] [--days N]
```

`assay view` writes one HTML file (`<repository>.assay.html` by default), its map inside and nothing to fetch: the blocks as cards, from what needs to what is needed, left to right; a block clicked shows its links, its languages and the people to ask.

## What it finds

| Block (`kind`) | Found from | Its files |
| --- | --- | --- |
| `rust` | a `Cargo.toml` with a `[package]` | its folder |
| `node` | a `package.json`, but a workspace's root | its folder |
| `python` | a project's `pyproject.toml`, a `setup.py` or `setup.cfg` | its folder |
| `go` | a `go.mod` | its folder |
| `migrations` | a `migrations` folder (Rails: `db/migrate`) | that folder |
| `docker` | a `Dockerfile` (`Containerfile`, `x.Dockerfile`, `Dockerfile.x`) | the file and its `.dockerignore` |
| `service` | a service of a compose file (not of a test stack) | none |
| `ci` | `.gitlab-ci.yml`, `.github/workflows/`, and other CI configurations | those files |
| `docs` | the top-level `docs/` folder | that folder |

A file belongs to the block with the deepest root holding it; the rest is reported as in no block. Left out of every map: `fixtures`, `testdata`, `vendor`, `node_modules`, `third_party` folders and the `.sqlx` cache, plus what assay.toml excludes. A block's `role` (`web`, `api`, `cli`, `library`, `database`, `cache`, `queue`, `search`, `storage`, `mail`) is a guess from its manifest (what it depends on, a binary, what it exports) or its image; without a clue, it has none. Its `description` is its manifest's (a package's, an image's `org.opencontainers.image.description` label) or assay.toml's.

Links go from the block that needs to the one needed:

| `kind` | Found from |
| --- | --- |
| `uses` | a Cargo path or workspace dependency (not a dev-dependency), an npm dependency on a package of the repository |
| `builds` | a Dockerfile's `COPY`/`ADD`; of the Cargo packages copied, only those its `cargo build -p`/`--bin` names, if it names some |
| `runs` | a compose service's `build` |
| `depends_on` | a compose service's `depends_on` |
| `connects` | a guess (`certainty: inferred`): a service's environment names another as a host (`postgres://app@db:5432`); assay.toml confirms it by declaring it with `kind = "connects"` |
| `calls` | declared in assay.toml |

People: who changed each block within the last year of git history (`.mailmap` applied, bots left out, one person per name or address), its CODEOWNERS owners and assay.toml's referents; someone `[people] exclude` names, by one of their names or addresses, is left out under all of them. `bus_factor` is how many people made at least a fifth of its recent commits: 1, one person knows it. CI jobs come with the blocks their `rules:changes` (GitLab, forgust; `extends` followed) or workflow `paths` filters (GitHub, Forgejo) name.

## assay.toml

At the top of the repository, to correct what the detection gets wrong and declare what no file tells. Blocks are named by the ids `assay map` prints; an unknown key is an error.

```toml
version = 1
exclude = ["bench/", "ci-conformance/"]

[blocks.forgust-server]
name = "API"
description = "The API, the web app and SSH"
referents = ["@alice"]
jobs = ["rust-*"]

[blocks.forgust-diff]
merge_into = "forgust-server"

[blocks.gen-api]
ignore = true

[blocks.site]
kind = "docs"
paths = ["site/"]

[[links]]
from = "forgust-web"
to = "forgust-server"   # kind = "calls" unless given

[[links]]
from = "image"
to = "forgust-bench"
remove = true

[people]
exclude = ["someone@example.com"]
days = 180
```

## As a library

```rust
let map = forgust_assay::map(std::path::Path::new("."), &forgust_assay::Options::default())?;
let block = map.block_of("crates/forgust-db/migrations/0001_init.sql");
```

A forge keeps no working copy: it gathers a `Snapshot` from its storage instead (every path, the text of the files `forgust_assay::reads` picks out, the commits of the last `forgust_assay::history_days(&texts)` days) and maps that. Forgust's server does so (`crates/forgust-server/src/api/smart_view.rs`), through this crate without its `cli` feature.

```rust
let snapshot = forgust_assay::Snapshot { name, commit: Some(id), files, texts, history: Some(commits) };
let map = forgust_assay::map_snapshot(&snapshot)?;
```

`RepoMap` serializes to, and reads back from, the CLI's JSON. The design, its definitions and what comes next: [docs/smart-view.md](docs/smart-view.md).

## License

[MIT](../../LICENSE)

## Tests

```sh
cargo test -p forgust-assay
```

The integration tests write small repositories (a Cargo workspace, an npm one, Dockerfiles, compose services, a GitLab CI, CODEOWNERS, assay.toml) with a git history, and map them; then map the same one as a forge hands it over, and smaller ones for what assay.toml corrects and for the roles assay guesses.
