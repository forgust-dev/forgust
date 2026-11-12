#!/usr/bin/env bash
# Prints the `--test` arguments of shard $1 (1-based) out of $2 for the server's
# integration tests, balanced by how long each file takes (seconds, measured on
# an 8-core machine). A file missing from the table counts as 30 s: new test
# files land in some shard without editing this script.
set -euo pipefail
shard=$1 total=$2
cd "$(dirname "$0")/../crates/forgust-server/tests"

declare -A secs=(
  [m6_everyday]=110 [m3_merge_requests]=107 [m7_v1]=80 [m4_ci]=66
  [m10_ci_keywords]=61 [m7_ci]=59 [m13_admin]=55 [m5_import]=53 [m1_repos]=53
  [m8_review]=42 [m8_hardening]=34 [m11_merge_trains]=32 [m2_browse]=26
  [m16_security]=23 [services]=22 [m9_registry]=14 [m14_archive]=13
  [m15_quotas]=12 [m12_renovate]=10 [m17_mcp]=8 [m5_api_v4]=6 [m5_issues]=6
  [m5_tools]=1 [m4_runner]=1
)

for f in *.rs; do
  name=${f%.rs}
  echo "${secs[$name]:-30} $name"
done | sort -k1,1nr -k2 | awk -v shard="$shard" -v total="$total" '
  {
    best = 1
    for (s = 2; s <= total; s++) if (load[s] < load[best]) best = s
    load[best] += $1
    if (best == shard) printf "--test %s ", $2
  }'
echo
