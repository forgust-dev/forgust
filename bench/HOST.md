# Bench host

ADR-0008 targets are only enforced on one dedicated machine, recorded here. Numbers
from any other machine are informative only.

Until it exists, `.github/workflows/bench.yml` runs only when started by hand; register
the machine as a runner labelled `bench`, then restore its push and nightly triggers.

| Field | Value |
| --- | --- |
| Machine | _to be provisioned_ (dedicated, not shared with CI) |
| CPU | |
| RAM | |
| Disk | NVMe model: |
| Kernel / OS | |
| Postgres | version, `shared_buffers` |
| git | version (≥ 2.45) |

## Informative baseline from the development machine (2026-11-07)

`bench/baselines/dev-host.json` was produced on a shared 8-vCPU / 11 GB development
VM that also runs other containers. Single-client p95 there varies by 2–3 ms between
runs, which is why the regression gate ignores differences under 5 ms
(`--min-delta-ms`) on top of the 15 % rule.
