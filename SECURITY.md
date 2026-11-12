# Security policy

## Reporting a vulnerability

Please report security problems privately, through GitHub's
**Report a vulnerability** button on this repository's **Security** tab. Do not open
a public issue or pull request for them.

Include what an attacker can do, the steps to reproduce it, and the version or commit
you tested. You will get an answer in the advisory, and credit when the fix is
published, unless you prefer not to be named.

## Supported versions

Forgust is before 1.0: only the latest release gets security fixes.

## Scope

Anything in this repository: the server and its APIs (`/api/v1`, `/api/v4`, the
runner API, the registry at `/v2/`, the MCP server), git over HTTP and SSH, the
importer, and the web UI. Problems in `gitlab-runner`, Renovate or the scanners'
images belong to those projects, unless Forgust uses them unsafely.

Security settings (the image proxy, a token for `/metrics`, which private addresses
webhooks and imports may reach, limits) are in the `[security]`, `[telemetry]`,
`[features]` and `[limits]` sections of
[forgust.example.toml](forgust.example.toml).
