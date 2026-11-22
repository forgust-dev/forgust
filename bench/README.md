# Benchmarks

`bench/run.sh` starts a release server on a scratch database, loads the fixtures
(a 2,000-file repo with a 1,000-file merge request and 2,000 issues, and a 1M-ref
repo; linux and chromium bundles when provided), and runs `forgust-bench run`:

- **single** profile: 20 warm-up requests, then 200 measured, per target;
- **concurrent** profile: 32 clients for 20 s per target;
- p50/p95/max per target, plus the first (cold-cache) request;
- **regression gate**: fail when a p95 is more than 15 % (and more than 5 ms) above
  the stored baseline; `UPDATE_BASELINE=1` writes a new baseline.

The targets are the pages of a repository (tree, README, highlighted file, commit,
blame, history, file finder, compare), code search, the merge request list, page and
diff, the issue list and page, the instance search (projects, issues and merge
requests by title), and Git over HTTP on the 1M-ref repo (`ls-refs`, a fetch with
nothing new, a push of one branch).

With `LINUX_BUNDLE`, code search is also timed in linux (about 100,000 files): text few
files have, text thousands have, and a regular expression. The bundle must hold the
full history or a single commit: a push from a shallow clone is refused.

HTTP timings are taken by the client over loopback, a close proxy for the
server-side span timings ADR-0008 names. Git targets (1M refs) are timed around the
`git` command, so they include client start-up.

See `HOST.md` for the machine the targets are enforced on.
