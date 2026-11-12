#!/usr/bin/env bash
# Release notes from the Conventional Commit subjects between two refs, grouped by
# type, newest first. Squash merges make every pull request one such commit.
#   .github/release-notes.sh v0.1.0 v0.2.0    # since v0.1.0
#   .github/release-notes.sh "" HEAD          # from the start
set -euo pipefail
from=${1:-}
to=${2:-HEAD}
range=${from:+$from..}$to

order=(feat fix perf docs deps internal other)
declare -A title=([feat]=Features [fix]="Bug Fixes" [perf]=Performance [docs]=Documentation
  [deps]=Dependencies [internal]=Internal [other]="Other Changes")
declare -A lines=()

while IFS=$'\t' read -r sha subject; do
  if [[ $subject =~ ^([a-z]+)(\(([^\)]*)\))?(!)?:\ (.+)$ ]]; then
    type=${BASH_REMATCH[1]} scope=${BASH_REMATCH[3]} bang=${BASH_REMATCH[4]} text=${BASH_REMATCH[5]}
  else
    type=other scope="" bang="" text=$subject
  fi
  # the version bump itself is not news
  [[ $type == chore && $text =~ ^release\  ]] && continue
  case $type in
    feat | fix | perf | docs) group=$type ;;
    chore) [[ $scope == deps* ]] && group=deps || group=internal ;; # deps, deps-dev
    refactor | test | ci | build | style) group=internal ;;
    *) group=other ;;
  esac
  [[ -n $bang ]] && text="**breaking:** $text"
  lines[$group]+="* $text ($sha)"$'\n'
done < <(git log --no-merges --format=$'%h\t%s' "$range")

for g in "${order[@]}"; do
  [[ -n ${lines[$g]:-} ]] || continue
  printf '### %s\n%s\n' "${title[$g]}" "${lines[$g]}"
done
