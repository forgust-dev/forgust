<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/images/forgust-logo-dark.svg" />
    <img src=".github/images/forgust-logo-light.svg" alt="Forgust" width="300" />
  </picture>
</p>

A fast, self-hosted code forge written in Rust: Git hosting, code review, issues,
CI/CD, a container registry and Pages in a single binary next to Postgres. Bring your
existing pipelines and runners, import your projects, and keep your team's whole
workflow on your own servers.

[![CI](https://github.com/forgust-dev/forgust/actions/workflows/ci.yml/badge.svg)](https://github.com/forgust-dev/forgust/actions/workflows/ci.yml)
[![Rust 1.94+](https://img.shields.io/badge/rust-1.94%2B-orange.svg)](https://www.rust-lang.org/)
[![PostgreSQL 17+](https://img.shields.io/badge/postgres-17%2B-336791.svg)](https://www.postgresql.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

## Screenshots
<table>
  <tr>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/code-dark.png"/><img src=".github/images/code-light.png" alt="Repository"/></picture><br/><sub>Repository</sub></td>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/file-dark.png"/><img src=".github/images/file-light.png" alt="Source file"/></picture><br/><sub>Source file</sub></td>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/merge-request-dark.png"/><img src=".github/images/merge-request-light.png" alt="Merge request"/></picture><br/><sub>Merge request</sub></td>
  </tr>
  <tr>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/diff-dark.png"/><img src=".github/images/diff-light.png" alt="Code review"/></picture><br/><sub>Code review</sub></td>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/pipeline-dark.png"/><img src=".github/images/pipeline-light.png" alt="Pipeline"/></picture><br/><sub>Pipeline</sub></td>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/job-dark.png"/><img src=".github/images/job-light.png" alt="Job log"/></picture><br/><sub>Job log</sub></td>
  </tr>
  <tr>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/issues-dark.png"/><img src=".github/images/issues-light.png" alt="Issues"/></picture><br/><sub>Issues</sub></td>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/security-dark.png"/><img src=".github/images/security-light.png" alt="Security findings"/></picture><br/><sub>Security findings</sub></td>
  </tr>
</table>

### Admin
<table>
  <tr>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/admin-dark.png"/><img src=".github/images/admin-light.png" alt="Overview"/></picture><br/><sub>Overview</sub></td>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/admin-storage-dark.png"/><img src=".github/images/admin-storage-light.png" alt="Storage"/></picture><br/><sub>Storage</sub></td>
    <td align="center"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/admin-health-dark.png"/><img src=".github/images/admin-health-light.png" alt="Health"/></picture><br/><sub>Health</sub></td>
  </tr>
</table>

## Quick Start

```bash
git clone https://github.com/forgust-dev/forgust && cd forgust

# Your machine's address, not localhost: CI jobs run in containers and must reach it
export FORGUST_HOST=192.0.2.10

# Start Forgust with Postgres, S3 storage, a mail catcher and a runner
docker compose up -d --build

# Fill it with users, projects, merge requests and pipelines (prints the accounts)
python3 deploy/seed.py
```

The app is available at **http://$FORGUST_HOST:8080**, and the emails it sends at
**http://$FORGUST_HOST:8027**.

### Production

Run the published image (amd64 and arm64) next to Postgres, or build your own with
`docker build -t forgust .`:

```yaml
services:
  forgust:
    image: ghcr.io/forgust-dev/forgust:0.1
    restart: unless-stopped
    ports: ["8080:8080", "2222:2222"]
    environment:
      FORGUST_DATABASE__URL: postgres://forgust:change-me@db:5432/forgust
      FORGUST_SERVER__EXTERNAL_URL: https://git.example.com
      FORGUST_SERVER__SSH_EXTERNAL: git.example.com:2222
    volumes: [forgust-data:/var/lib/forgust]
    depends_on:
      db: { condition: service_healthy }
  db:
    image: postgres:17
    restart: unless-stopped
    environment: { POSTGRES_USER: forgust, POSTGRES_PASSWORD: change-me, POSTGRES_DB: forgust }
    healthcheck: { test: ["CMD-SHELL", "pg_isready -U forgust"], interval: 5s }
    volumes: [db-data:/var/lib/postgresql/data]
volumes: { forgust-data: {}, db-data: {} }
```

- **First administrator:** the log prints a one-time link on first start
  (`docker compose logs forgust | grep setup`).
- **Configuration:** every key in [`forgust.example.toml`](forgust.example.toml) can
  be set as `FORGUST_<SECTION>__<KEY>`; OIDC providers go in `[oidc.<name>]`.
- **Backups:** `forgust backup --output DIR` and `forgust restore --input DIR` cover
  the database and the repositories. Also keep `/var/lib/forgust/secret.key`, the
  SSH host key next to it, and the S3 bucket if you use one.

## Features
### 📦 Code
- Git over HTTP and SSH, Git LFS with file locking
- Fast code browsing: tree, blame, history, compare, syntax highlighting
- Markdown with diagrams (Mermaid), math and alerts
- Edit files, create branches and tags from the browser
- Protected branches and tags, push rules, signed-commit verification (GPG and SSH)
- Releases with notes and attached files

### 🔍 Code review
- Merge requests with threaded line and multi-line comments
- Suggestions the author applies in one click
- Approvals, `CODEOWNERS`, and merge checks (pipeline, approvals, threads, security)
- Stacked merge requests, merge trains, auto-merge when every check passes
- Keyboard-driven review: `j`/`k` to move, `c` to comment

### 🗂️ Planning
- Issues with labels, milestones, due dates, assignees and templates
- Mentions, `#12` and `!45` references, to-dos and a "Your work" inbox
- Emoji reactions, watching projects, per-project notification levels

### ⚙️ CI/CD
- Pipelines with stages and `needs` graphs, live logs, artifacts, JUnit test reports
- Runs on GitLab Runner (any executor; Docker is tested): `.gitlab-ci.yml` files work
  unchanged within a documented subset, and anything outside it fails with a clear
  error
- Schedules, manual and delayed jobs, child pipelines, `resource_group`, `id_tokens`
- Environments and deployments, Pages, a built-in container registry
- Built-in dependency updates (Renovate), no bot account or token to set up

### 🔐 Security
- Built-in scanners (code, secrets, dependencies, containers, infrastructure as code)
  with a Security tab, merge request checks and alerts
- OIDC sign-in, two-factor authentication, personal, project and deploy tokens
- Strict CSP, an image proxy for Markdown images, rate limits

### 🛠️ Administration
- Overview with activity and CI statistics
- Users: block, reset two-factor, impersonate (audited)
- Storage by kind and by project, quotas per group and project
- Background jobs, health checks, backups from the UI, an instance banner, a notice to administrators when a new release is out (can be turned off)

### 🔌 Integrations
- Import from GitLab and GitHub: repositories, merge requests, issues, labels,
  members; a dry run first, resumable
- Webhooks, also formatted for Slack, Discord, Matrix, Mattermost and Teams
- A GitLab-compatible `/api/v4` subset, so Renovate and `glab` work
- An MCP server for AI assistants, with OAuth sign-in

## Performance

Measured on an 8-vCPU development machine, p95 with one client, release build:

| | |
| --- | --- |
| Repository tree, highlighted 2,000-line file, commit diff, blame | 7–11 ms |
| Merge request page and first diff page of a 1,000-file merge request | 9 ms |
| `ls-refs` with a prefix on a repository with 1M refs | 13 ms |
| Pipeline created → job picked up by a runner | 24–40 ms |

Pushing one branch to a 1M-ref repository is the one speed target still missed (7 s
against 300 ms).

## Status

Forgust is young (0.1) and built for private teams first. Not there yet: forks and
merge requests from forks, wikis, snippets, package registries, push and pull
mirroring, issue boards, confidential issues, code search (search covers titles),
system hooks, and WebAuthn, SAML or LDAP sign-in.

## Tech Stack
| Layer | Technology |
|---|---|
| Backend | Rust + axum + tokio + sqlx |
| Git | git (writes and transfer) + gitoxide (hot reads), russh for SSH |
| Frontend | React 18 + TypeScript + Vite + TanStack Query |
| Database | PostgreSQL 17+ |
| Job queue | graphile_worker (on PostgreSQL) |
| Storage | Local disk or S3-compatible object storage |

## Development
```bash
docker compose -f compose.dev.yml up -d      # Postgres :5440, S3 :9010, SMTP :1026 (inbox on :8026)
echo DATABASE_URL=postgres://forgust:forgust@localhost:5440/forgust > .env
cp forgust.example.toml forgust.toml         # set database.url
(cd web && npm ci && npm run build)          # or `npm run dev` with FORGUST_URL set
cargo run -p forgust-server -- serve         # migrates on start
cargo test --workspace                       # one throwaway database per test
(cd web && npm run e2e)                      # end-to-end tests (Playwright)
```

Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) (commits are signed
off under the DCO). Report security problems privately, as [SECURITY.md](SECURITY.md)
explains.

### AI transparency disclosure
AI-assisted contributions are accepted, but must be disclosed on the PR. The same
quality and security bar applies regardless of how the code was written.

## License

[MIT](LICENSE). GitLab is a trademark of GitLab Inc.; Forgust is not affiliated with
GitLab.
