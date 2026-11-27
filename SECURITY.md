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

## Verifying a release

The release workflow builds `ghcr.io/forgust-dev/forgust` on GitHub's runners and
signs it with a build provenance attestation: a Sigstore certificate that names the
repository, the tag and the workflow run that built the image. To check an image
before running it, with the [GitHub CLI](https://cli.github.com):

```sh
gh attestation verify oci://ghcr.io/forgust-dev/forgust:0.2.0 \
  --repo forgust-dev/forgust --signer-workflow forgust-dev/forgust/.github/workflows/release.yml
```

Each image also carries an SBOM (SPDX): the Debian packages of the image, the crates
compiled into the binary, and the packages of the build stages.

```sh
docker buildx imagetools inspect ghcr.io/forgust-dev/forgust:0.2.0 --format '{{ json .SBOM }}'
```

The image is scanned for known vulnerabilities at each release, and `latest` every
week. Releases up to 0.1.0 were published before signing and have no attestation
to verify.

## Signing in

- **Passwords** are hashed with Argon2. Sign-in attempts are limited per client, per
  login and per account; past the account's limit, only addresses it signed in from
  before may keep trying.
- **Two-factor authentication** is an authenticator app (TOTP) or security keys
  (WebAuthn), asked for after the password or the identity provider. Ten single-use
  recovery codes come with the first second factor; using one emails the account.
  Wrong answers, whatever the factor, share one limit of 5 per account per 10 minutes.
  With two-factor authentication on, git over HTTP and the registry take tokens, not
  the password.
- **Passkeys** sign in without a username or password. They must verify their user
  (PIN or biometrics), so a passkey sign-in skips the second step. They are not a
  second factor: an account with only a passkey still signs in with its password
  alone, and does not meet an organization's two-factor requirement.
- **WebAuthn** uses the host of `server.external_url` as its relying party id and
  accepts that URL's origin only (no subdomains, no other port), over https or on
  `localhost`. Ceremony state stays in the database and is used once. Attestation is
  not asked for. A counter that goes back is refused and audited
  (`webauthn_clone_suspected`).
- **Changing the domain** (the host of `server.external_url`) makes every security key
  and passkey stop working: they belong to the old host. Before moving, make sure
  people with security keys have recovery codes or an authenticator app; after it,
  they add their keys again (an administrator can reset two-factor authentication
  for anyone locked out).
- **Changes to the ways in** (password, email, two-factor, keys, tokens) need a
  browser session of the account itself, never an impersonation or a token. Changing
  the email address or the second factors, and adding or removing a security key or
  passkey, ask for the password again. They are audited; a new or removed security key
  or passkey is emailed to the account.

## MCP code mode

Off until an administrator turns it on (Admin, Settings). Its `search_api` and
`execute_api` tools run JavaScript that an assistant writes, in QuickJS on the server:

- **The sandbox** has no files, network, processes, timers or modules. A run gets at
  most 64 MB of memory, 1 MB of stack and 30 seconds, and returns at most 1 MB of
  JSON.
- **The one way out** is `forgust.request`. It calls the `/api/v1` router in-process
  as the token's user, through the same handlers and permission checks as over HTTP.
  It cannot reach another host or another part of the server.
- **Scopes:** `execute_api` needs a token that may use `/api/v1`: `read_api` to read
  (GET), `api` to change things too. An `mcp` token, the one OAuth clients get, has no
  `execute_api`, and `mcp_write` keeps to the write tools the person allowed. The admin API still needs `admin_mode`, and credentials are never made
  with a token.
- **Limits:** a run makes at most 50 calls, and each call counts against the token's
  MCP rate limit (120 a minute). The server runs at most 8 scripts at once, and at
  most 2 for one user. Each change is audited (`mcp_tool_call`, with the
  method and the route).

## Scope

Anything in this repository: the server and its APIs (`/api/v1`, `/api/v4`, the
runner API, the registry at `/v2/`, the MCP server), git over HTTP and SSH, the
importer, and the web UI. Problems in `gitlab-runner`, Renovate or the scanners'
images belong to those projects, unless Forgust uses them unsafely.

Security settings (the image proxy, a token for `/metrics`, which private addresses
webhooks and imports may reach, limits) are in the `[security]`, `[telemetry]`,
`[features]` and `[limits]` sections of
[forgust.example.toml](forgust.example.toml).
