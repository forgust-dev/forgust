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
  390 px;
- **CI behaviour:** a case in [`ci-conformance/cases/`](ci-conformance/README.md), and
  a note in the pull request for the CI compatibility guide.

Keep each pull request to one change, with tests. Commit messages start with the
area, then say what changed: `CI: one run at a time on the runner`.

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
