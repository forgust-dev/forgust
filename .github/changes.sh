#!/usr/bin/env bash
# Prints, as step outputs, what a push or a pull request touched, so CI skips the
# jobs it cannot affect:
#   rust   the server, its tests or anything not listed below as unrelated
#   web    the web UI
#   image  what the release image is built from, beyond the sources
# A push to main runs every Rust and web job (it also saves their caches); only
# `image` follows the files there. EVENT and BEFORE come from the workflow.
set -euo pipefail

if [ "$EVENT" = pull_request ]; then
  # the checkout is the merge commit: its first parent is the base branch
  files=$(git diff --name-only HEAD^1 HEAD)
elif git fetch -q --depth=1 origin "$BEFORE" 2>/dev/null; then
  files=$(git diff --name-only "$BEFORE" HEAD)
else
  # nothing to compare with (a first push): as if CI itself had changed
  files=.github/workflows/ci.yml
fi

rust=false web=false image=false
while IFS= read -r f; do
  case "$f" in
    "") ;;
    .github/workflows/ci.yml | .github/changes.sh) rust=true web=true image=true ;;
    # checked against the server by a unit test (api::openapi)
    web/openapi.json) rust=true web=true ;;
    web/package.json | web/package-lock.json) web=true image=true ;;
    web/*) web=true ;;
    *.md | LICENSE | .github/images/* | .github/ISSUE_TEMPLATE/* | .github/codeql/* | .github/workflows/* | .github/dependabot.yml | .github/release-notes.sh) ;;
    Dockerfile | .dockerignore | Cargo.lock | Cargo.toml | */Cargo.toml) rust=true image=true ;;
    *) rust=true ;;
  esac
done <<<"$files"
[ "$EVENT" = pull_request ] || rust=true web=true

echo "rust=$rust"
echo "web=$web"
echo "image=$image"
