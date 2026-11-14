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

## Scope

Anything in this repository: the server and its APIs (`/api/v1`, `/api/v4`, the
runner API, the registry at `/v2/`, the MCP server), git over HTTP and SSH, the
importer, and the web UI. Problems in `gitlab-runner`, Renovate or the scanners'
images belong to those projects, unless Forgust uses them unsafely.

Security settings (the image proxy, a token for `/metrics`, which private addresses
webhooks and imports may reach, limits) are in the `[security]`, `[telemetry]`,
`[features]` and `[limits]` sections of
[forgust.example.toml](forgust.example.toml).
