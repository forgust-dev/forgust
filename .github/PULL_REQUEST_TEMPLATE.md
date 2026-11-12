## What and why

<!-- What changes, and the issue it closes (Closes #…). -->

## Checklist

- [ ] Tests for the change; `cargo test --workspace` passes
- [ ] `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` are clean
- [ ] `.sqlx/`, `web/openapi.json` and `web/src/api/schema.d.ts` regenerated if queries or the API changed
- [ ] Behaviour changes described for the docs (CI behaviour, configuration, UI)
- [ ] Commits signed off (`git commit -s`, see CONTRIBUTING.md)
