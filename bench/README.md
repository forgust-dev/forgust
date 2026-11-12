# Benchmarks

`bench/run.sh` starts a release server on a scratch database, loads the fixtures
(a 2,000-file repo and a 1M-ref repo; linux and chromium bundles when provided), and
runs `forgust-bench run`:

- **single** profile: 20 warm-up requests, then 200 measured, per target;
- **concurrent** profile: 32 clients for 20 s per target;
- p50/p95/max per target, plus the first (cold-cache) request;
- **regression gate**: fail when a p95 is more than 15 % (and more than 5 ms) above
  the stored baseline; `UPDATE_BASELINE=1` writes a new baseline.

HTTP timings are taken by the client over loopback, a close proxy for the
server-side span timings ADR-0008 names. Git targets (1M refs) are timed around the
`git` command, so they include client start-up.

See `HOST.md` for the machine the targets are enforced on.
