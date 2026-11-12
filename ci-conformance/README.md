# CI conformance corpus

Each directory under `cases/` is one `.gitlab-ci.yml` (with any files it includes),
an optional `context.json` (`ref`, `tag`, `source`, `variables`) and an
`expected.json`: the jobs (`name`, `stage`, `when`, `allow_failure`, and `needs` or
`script` when they matter), `{"skipped": true}` for no pipeline, or `{"error": "..."}`.

- `cargo test -p forgust-ci-config --test conformance` checks Forgust against every case.
- `compare_gitlab.py` checks a real GitLab CE against the same cases (nightly
  workflow `conformance.yml`). It compares job names, `stage`, `when` and
  `allow_failure` (not `needs` or `script`); since 2026-11-11 every case its dry
  run can reproduce matches GitLab CE 19.5.3.
- `"forgust_only"` marks a deliberate difference (a keyword Forgust refuses that
  GitLab runs), listed in the CI compatibility guide.
- Cases GitLab's dry run cannot reproduce (merge request pipelines) set
  `"gitlab_dry_run": false`.
