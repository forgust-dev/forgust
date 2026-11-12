#!/usr/bin/env bash
# Benchmark run (ADR-0008): start a release server on a scratch database, load the
# fixtures, measure, and compare p95 with the stored baseline.
#
#   BENCH_DATABASE_URL  admin URL of a Postgres the script may create databases in
#                       (default postgres://forgust:forgust@localhost:5432/forgust)
#   BASELINE            baseline file (default bench/baselines/bench-host.json)
#   UPDATE_BASELINE=1   write the results as the new baseline instead of gating
#   LINUX_BUNDLE        optional `git bundle` of linux to load as a real-world repo
#   NIGHTLY=true        also load CHROMIUM_BUNDLE when set
set -euo pipefail
cd "$(dirname "$0")/.."

ADMIN_URL=${BENCH_DATABASE_URL:-postgres://forgust:forgust@localhost:5432/forgust}
DB=forgust_bench_$$
BASE=${ADMIN_URL%/*}
WORK=$(mktemp -d)
PORT=${BENCH_PORT:-18999}
BASELINE=${BASELINE:-bench/baselines/bench-host.json}
BIN=target/release

psql "$ADMIN_URL" -qc "CREATE DATABASE $DB"
cleanup() {
  kill "${SERVER:-0}" 2>/dev/null || true
  psql "$ADMIN_URL" -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" || true
  rm -rf "$WORK"
}
trap cleanup EXIT

export FORGUST_DATABASE__URL="$BASE/$DB" FORGUST_SERVER__DATA_DIR="$WORK/data" FORGUST_SERVER__LISTEN="127.0.0.1:$PORT" \
  FORGUST_SERVER__EXTERNAL_URL="http://127.0.0.1:$PORT" FORGUST_SERVER__SSH_LISTEN= FORGUST_TELEMETRY__LOG_FILTER=warn
$BIN/forgust admin create-user --username bench --email bench@example.com --password benchpassword --admin
$BIN/forgust serve > "$WORK/server.log" 2>&1 &
SERVER=$!
for _ in $(seq 1 60); do curl -sf "127.0.0.1:$PORT/health" >/dev/null && break; sleep 0.5; done

API="http://127.0.0.1:$PORT/api/v1"
LOGIN=$(curl -s -c "$WORK/jar" -X POST "$API/auth/login" -H 'content-type: application/json' -d '{"login":"bench","password":"benchpassword"}')
CSRF=$(echo "$LOGIN" | python3 -c 'import sys,json; print(json.load(sys.stdin)["session"]["csrf_token"])')
call() { curl -sf -b "$WORK/jar" -H "x-csrf-token: $CSRF" -H 'content-type: application/json' "$@"; }
TOKEN=$(call -X POST "$API/user/tokens" -d '{"name":"bench","scopes":["api","read_repository","write_repository"]}' | python3 -c 'import sys,json; print(json.load(sys.stdin)["token"])')
REMOTE="http://bench:$TOKEN@127.0.0.1:$PORT"

# typical repo: 2,000 files, 200 commits, one 2,000-line file
call -X POST "$API/projects" -d '{"name":"fixture"}' >/dev/null
$BIN/forgust-bench fixture --out "$WORK/fixture" >/dev/null
git -C "$WORK/fixture" push -q "$REMOTE/bench/fixture.git" main

# merge request with 1,000 changed files
git -C "$WORK/fixture" checkout -q -b big
mkdir -p "$WORK/fixture/gen" && for i in $(seq -w 0 999); do echo "fn f$i() {}" > "$WORK/fixture/gen/f$i.rs"; done
git -C "$WORK/fixture" add gen && git -C "$WORK/fixture" -c user.name=b -c user.email=b@x commit -q -m "1000 files"
git -C "$WORK/fixture" push -q "$REMOTE/bench/fixture.git" big
call -X POST "$API/projects/bench%2Ffixture/merge_requests" -d '{"source_branch":"big","title":"Big"}' >/dev/null

# 1M-ref repo: one commit, then refs written straight into packed-refs
REFS_ID=$(call -X POST "$API/projects" -d '{"name":"refs1m"}' | python3 -c 'import sys,json; print(json.load(sys.stdin)["id"])')
git init -q -b main "$WORK/small" && git -C "$WORK/small" -c user.name=b -c user.email=b@x commit -q --allow-empty -m one
git -C "$WORK/small" push -q "$REMOTE/bench/refs1m.git" main
H=$(printf %s "$REFS_ID" | sha256sum | cut -c1-64)
$BIN/forgust-bench refs --repo "$WORK/data/repositories/@repos/${H:0:2}/${H:2:2}/$H.git" --count 1000000

if [[ -n "${LINUX_BUNDLE:-}" ]]; then
  call -X POST "$API/projects" -d '{"name":"linux"}' >/dev/null
  git clone -q --mirror "$LINUX_BUNDLE" "$WORK/linux.git" && git -C "$WORK/linux.git" push -q --mirror "$REMOTE/bench/linux.git"
fi

ARGS=(--url "http://127.0.0.1:$PORT" --token "$TOKEN" --project bench/fixture --file src/file_00000.rs --refs-project bench/refs1m --mr 'bench/fixture!1'
  --out "bench/results/$(date -u +%Y%m%dT%H%M%SZ).json" --baseline "$BASELINE" --host "$(uname -n), see bench/HOST.md")
[[ "${UPDATE_BASELINE:-0}" == 1 ]] && ARGS+=(--update-baseline)
$BIN/forgust-bench run "${ARGS[@]}"
