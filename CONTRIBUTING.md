# Contributing to Forgust

Thanks for helping. Bug reports, fixes, docs and tests are all welcome.

## Before you start

- **Bugs:** open an issue with the steps to reproduce, what you expected and what
  happened. For CI problems, attach the `.gitlab-ci.yml` (or the smallest one that
  shows it).
- **Features:** open an issue first. Forgust supports a clear subset of what GitLab
  does and fails loudly on the rest, so not every feature fits. Check what is
  [not there yet](README.md#status) first.
- **Security problems:** do not open an issue; see [SECURITY.md](SECURITY.md).
- **Bigger design changes** (storage, permissions, the CI engine, anything hard to
  undo) start with an issue describing the problem and the options, before any
  code.

## Set up

See [Development](README.md#development) in the README: Rust, git, Node 22, and Docker for
Postgres, S3 and SMTP.

## Before you open a pull request

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
(cd web && npm run typecheck && npm run build)
```

And when you changed:

- **a SQL query:** `cargo sqlx prepare --workspace -- --all-targets` (CI builds
  offline from `.sqlx/`);
- **the schema:** a new file in `crates/forgust-db/migrations/`; never edit one that
  has been released;
- **the API:** `cargo run -p forgust-server -- openapi > web/openapi.json`, then
  `(cd web && npm run gen:api)`; a unit test fails while they are stale;
- **the UI:** the Playwright tests (`cd web && npm run e2e`), at desktop width and
  390 px; if a page in the README changed, its screenshots too
  (`node deploy/screenshots.mjs`, see the script for the demo it needs);
- **CI behaviour:** a case in [`ci-conformance/cases/`](ci-conformance/README.md), and
  a note in the pull request for the CI compatibility guide.

Keep each pull request to one change, with tests. Commit messages and pull request
titles follow [Conventional Commits](https://www.conventionalcommits.org/): a type,
then what the change does, in lower case (`fix: keep reviewed requests apart in your
work`). Types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `chore`,
`revert`; add `!` for a breaking change (`feat!: …`). Pull requests are squashed, so
their title becomes the commit on `main` and the line in the release notes: CI checks
it.

## Sign your commits (DCO)

Forgust uses the [Developer Certificate of Origin](https://developercertificate.org/):
by adding a `Signed-off-by` line to each commit, you certify that you wrote the change
or otherwise have the right to submit it under the project's
[MIT license](LICENSE).

```sh
git commit -s
```

adds the line from your `user.name` and `user.email`. To sign commits you already
made: `git rebase --signoff main`.

## Releasing (maintainers)

1. In a pull request, set the new version in `Cargo.toml` (`[workspace.package]`) and
   `web/package.json`, then refresh the lock files (`cargo check`,
   `cd web && npm install`): `chore: release 0.2.0`.
2. Once it is merged, tag that commit of `main` and push the tag:
   `git tag v0.2.0 && git push origin v0.2.0`.
3. The release workflow checks that the tag matches both versions and is on `main`,
   builds the image for amd64 and arm64, pushes `ghcr.io/forgust-dev/forgust` as
   `0.2.0`, `0.2` and `latest`, and opens a draft GitHub release whose notes group the
   commits since the previous release (its release candidates included) by type.
   Edit the notes, then publish it. To preview them before tagging:
   `.github/release-notes.sh v0.1.0 main`.

A tag such as `v0.2.0-rc.1` makes a pre-release: no `0.2` or `latest` tag.
