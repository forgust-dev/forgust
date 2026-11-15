#!/usr/bin/env bash
# Prints the `--test` arguments of shard $1 (1-based) out of $2 for the server's
# integration tests, balanced by how long each file takes (seconds: the sum of
# its tests' times, at 3 threads). A file missing from the table counts as 15 s:
# new test files land in some shard without editing this script. Shard 1 starts
# with 80 s: it also builds and runs the unit tests and the other crates.
set -euo pipefail
shard=$1 total=$2
cd "$(dirname "$0")/../crates/forgust-server/tests"

declare -A secs=(
  [m6_everyday]=54 [m7_v1]=53 [m10_ci_keywords]=47 [m3_merge_requests]=45
  [m4_ci]=34 [m1_repos]=34 [m13_admin]=28 [m7_ci]=26 [m11_merge_trains]=25
  [m5_import]=24 [m8_review]=20 [m16_security]=17 [m18_sso]=12 [m9_registry]=12
  [m8_hardening]=11 [m15_quotas]=11 [m12_renovate]=10 [m2_browse]=8
  [services]=8 [m14_archive]=6 [m5_issues]=5 [m5_api_v4]=4 [m17_mcp]=4
  [m5_tools]=1 [m4_runner]=1
)

for f in *.rs; do
  name=${f%.rs}
  echo "${secs[$name]:-15} $name"
done | sort -k1,1nr -k2 | awk -v shard="$shard" -v total="$total" '
  BEGIN { load[1] = 80 }
  {
    best = 1
    for (s = 2; s <= total; s++) if (load[s] < load[best]) best = s
    load[best] += $1
    if (best == shard) printf "--test %s ", $2
  }'
echo
