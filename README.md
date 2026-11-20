<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset=".github/images/forgust-logo-dark.png" />
    <img src=".github/images/forgust-logo-light.png" alt="Forgust" width="300" />
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
    <td align="center"><a href=".github/images/code-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/code-dark.webp"/><img src=".github/images/code-light.webp" alt="Repository"/></picture></a><br/><sub>Repository</sub></td>
    <td align="center"><a href=".github/images/merge-request-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/merge-request-dark.webp"/><img src=".github/images/merge-request-light.webp" alt="Merge request"/></picture></a><br/><sub>Merge request</sub></td>
    <td align="center"><a href=".github/images/diff-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/diff-dark.webp"/><img src=".github/images/diff-light.webp" alt="Code review"/></picture></a><br/><sub>Code review</sub></td>
  </tr>
  <tr>
    <td align="center"><a href=".github/images/pipeline-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/pipeline-dark.webp"/><img src=".github/images/pipeline-light.webp" alt="Pipeline"/></picture></a><br/><sub>Pipeline</sub></td>
    <td align="center"><a href=".github/images/login-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/login-dark.webp"/><img src=".github/images/login-light.webp" alt="Sign in"/></picture></a><br/><sub>Sign in</sub></td>
    <td align="center"><a href=".github/images/security-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/security-dark.webp"/><img src=".github/images/security-light.webp" alt="Security findings"/></picture></a><br/><sub>Security findings</sub></td>
  </tr>
</table>

<details>
<summary>More screenshots</summary>

<table>
  <tr>
    <td align="center"><a href=".github/images/job-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/job-dark.webp"/><img src=".github/images/job-light.webp" alt="Job log"/></picture></a><br/><sub>Job log</sub></td>
    <td align="center"><a href=".github/images/dashboard-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/dashboard-dark.webp"/><img src=".github/images/dashboard-light.webp" alt="Home"/></picture></a><br/><sub>Home</sub></td>
    <td align="center"><a href=".github/images/file-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/file-dark.webp"/><img src=".github/images/file-light.webp" alt="Source file"/></picture></a><br/><sub>Source file</sub></td>
  </tr>
  <tr>
    <td align="center"><a href=".github/images/commits-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/commits-dark.webp"/><img src=".github/images/commits-light.webp" alt="Commits"/></picture></a><br/><sub>Commits</sub></td>
    <td align="center"><a href=".github/images/merge-requests-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/merge-requests-dark.webp"/><img src=".github/images/merge-requests-light.webp" alt="Merge requests"/></picture></a><br/><sub>Merge requests</sub></td>
    <td align="center"><a href=".github/images/issues-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/issues-dark.webp"/><img src=".github/images/issues-light.webp" alt="Issues"/></picture></a><br/><sub>Issues</sub></td>
  </tr>
  <tr>
    <td align="center"><a href=".github/images/environments-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/environments-dark.webp"/><img src=".github/images/environments-light.webp" alt="Environments"/></picture></a><br/><sub>Environments</sub></td>
    <td align="center"><a href=".github/images/releases-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/releases-dark.webp"/><img src=".github/images/releases-light.webp" alt="Releases"/></picture></a><br/><sub>Releases</sub></td>
    <td align="center"><a href=".github/images/registry-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/registry-dark.webp"/><img src=".github/images/registry-light.webp" alt="Container registry"/></picture></a><br/><sub>Container registry</sub></td>
  </tr>
  <tr>
    <td align="center"><a href=".github/images/group-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/group-dark.webp"/><img src=".github/images/group-light.webp" alt="Group"/></picture></a><br/><sub>Group</sub></td>
    <td align="center"><a href=".github/images/user-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/user-dark.webp"/><img src=".github/images/user-light.webp" alt="User profile"/></picture></a><br/><sub>User profile</sub></td>
    <td align="center"><a href=".github/images/settings-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/settings-dark.webp"/><img src=".github/images/settings-light.webp" alt="Project settings"/></picture></a><br/><sub>Project settings</sub></td>
  </tr>
  <tr>
    <td align="center"><a href=".github/images/profile-security-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/profile-security-dark.webp"/><img src=".github/images/profile-security-light.webp" alt="Password, 2FA and passkeys"/></picture></a><br/><sub>Password, 2FA and passkeys</sub></td>
    <td align="center"><a href=".github/images/admin-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/admin-dark.webp"/><img src=".github/images/admin-light.webp" alt="Admin overview"/></picture></a><br/><sub>Admin overview</sub></td>
    <td align="center"><a href=".github/images/admin-users-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/admin-users-dark.webp"/><img src=".github/images/admin-users-light.webp" alt="Admin users"/></picture></a><br/><sub>Admin users</sub></td>
  </tr>
  <tr>
    <td align="center"><a href=".github/images/admin-runners-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/admin-runners-dark.webp"/><img src=".github/images/admin-runners-light.webp" alt="Admin runners"/></picture></a><br/><sub>Admin runners</sub></td>
    <td align="center"><a href=".github/images/admin-storage-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/admin-storage-dark.webp"/><img src=".github/images/admin-storage-light.webp" alt="Admin storage"/></picture></a><br/><sub>Admin storage</sub></td>
    <td align="center"><a href=".github/images/admin-health-dark.webp"><picture><source media="(prefers-color-scheme: dark)" srcset=".github/images/admin-health-dark.webp"/><img src=".github/images/admin-health-light.webp" alt="Admin health"/></picture></a><br/><sub>Admin health</sub></td>
  </tr>
</table>
</details>

## Quick start

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
    image: ghcr.io/forgust-dev/forgust:0.5
    restart: unless-stopped
    read_only: true
    tmpfs: [/tmp]
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
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

- **Verify the image:** releases are signed and carry an SBOM, see
  [SECURITY.md](SECURITY.md#verifying-a-release).
- **First administrator:** the log prints a one-time link on first start
  (`docker compose logs forgust | grep setup`).
- **Configuration:** every key in [`forgust.example.toml`](forgust.example.toml) can
  be set as `FORGUST_<SECTION>__<KEY>`; OIDC providers go in `[oidc.<name>]`.
- **Security keys and passkeys** are bound to the host of `server.external_url`, which
  must be served over https (or be `localhost`); they are off otherwise. On a move to
  another host they stop working: people sign in with their password and a recovery
  code (or an administrator resets their two-factor authentication), then add them
  again. See [SECURITY.md](SECURITY.md#signing-in).
- **Backups:** `forgust backup --output DIR` and `forgust restore --input DIR` cover
  the database and the repositories. Also keep `/var/lib/forgust/secret.key`, the
  SSH host key next to it, and the S3 bucket if you use one.

## Features
### 📦 Code
- Git over HTTP and SSH, Git LFS with file locking
- Fast code browsing: tree, blame, history, compare, syntax highlighting
- Code search across projects: exact text or regular expressions, filters by path,
  file name and language, only in code you can read
- Markdown with diagrams (Mermaid), math, alerts and emoji shortcodes
- Edit files, create branches and tags from the browser
- Protected branches and tags, push rules, signed-commit verification (GPG and SSH)
- Releases with notes and attached files

### 🔍 Code review
- Merge requests with threaded line and multi-line comments
- Suggestions the author applies in one click
- Approvals, `CODEOWNERS`, and merge checks (pipeline, approvals, threads, security)
- Stacked merge requests, merge trains, auto-merge when every check passes
- Keyboard-driven review: `j`/`k` to move, `c` to comment
- AI review on request, with any OpenAI-compatible endpoint (a local model works):
  comments from a bot that never approves, each checked against the diff before
  it is posted, some with a change to apply in one click. The review runs in
  CI with the [reviewer image](crates/forgust-reviewer), released with Forgust.
  Off until an administrator sets the endpoint the code is sent to

### 🗂️ Planning
- Issues with labels, milestones, due dates, assignees and templates
- Confidential issues (only Reporters and up, the author and the assignees see them)
  and internal comments on issues and merge requests (only Reporters and up)
- Mentions, `#12` and `!45` references, to-dos and a Home page of what is waiting
  for you
- Emoji reactions, watching projects, per-project notification levels
- A live notification inbox in the app, with a switch per kind of notification for
  the app and for email

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
- OIDC sign-in, passkeys, two-factor authentication (an authenticator app or security
  keys, with recovery codes), personal, project and deploy tokens
- Strict CSP, an image proxy for Markdown images, rate limits

### 🛠️ Administration
- Overview with activity and CI statistics
- Users: block, reset two-factor, impersonate (audited)
- Storage by kind and by project, quotas per group and project
- Runners, background jobs, health checks and backups from the UI
- An instance banner, and a notice to administrators when a new release is out (can
  be turned off)

### 🔌 Integrations
- Import from GitLab and GitHub: repositories, merge requests, issues (confidential
  ones stay confidential, internal notes internal), labels, members; a dry run first,
  resumable
- Webhooks, also formatted for Slack, Discord, Matrix, Mattermost and Teams;
  confidential issues and internal comments only go to hooks that ask for their own
  events
- A GitLab-compatible `/api/v4` subset, so Renovate and `glab` work
- An MCP server for AI assistants, with OAuth sign-in

## Performance

Measured on an 8-vCPU development machine, p95 with one client, release build:

| | |
| --- | --- |
| Repository tree, highlighted 2,000-line file, commit diff, blame | 7–11 ms |
| Merge request page and first diff page of a 1,000-file merge request | 9 ms |
| `ls-refs` with a prefix on a repository with 1M refs | 13 ms |
| Code search in a 2,000-file repository (text one file has, text every file has) | 15–25 ms |
| Code search in the Linux kernel, 97,000 files (plain text, a regular expression) | 40–250 ms |
| Pipeline created → job picked up by a runner | 24–40 ms |

Pushing one branch to a 1M-ref repository is the one speed target still missed (7 s
against 300 ms).

## Status

Forgust is young (0.5) and built for private teams first. Not there yet: forks and
merge requests from forks, wikis, snippets, package registries, push and pull
mirroring, issue boards, system hooks, and SAML or LDAP sign-in.
Code search covers each project's default branch, not other branches or tags.

## Tech stack
| Layer | Technology |
|---|---|
| Backend | Rust + axum + tokio + sqlx |
| Git | git (writes and transfer) + gitoxide (hot reads), russh for SSH |
| Frontend | React 19 + TypeScript + Vite + TanStack Query |
| Database | PostgreSQL 17+ |
| Job queue | graphile_worker (on PostgreSQL) |
| Storage | Local disk or S3-compatible object storage |

## Development
Contributions are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md) (commits are signed
off under the DCO). Report security problems privately, as [SECURITY.md](SECURITY.md)
explains.

### AI transparency disclosure
AI-assisted contributions are accepted, but must be disclosed on the PR. The same
quality and security bar applies regardless of how the code was written.

## License

[MIT](LICENSE). GitLab is a trademark of GitLab Inc.; Forgust is not affiliated with
GitLab.
