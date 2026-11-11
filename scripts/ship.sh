#!/usr/bin/env bash
# Push and wait for CI in one step, printing a single summary line.
#
#   scripts/ship.sh           push the current branch, wait for its CI
#   scripts/ship.sh --main    fast-forward main to HEAD (publishes a release if
#                             the Cargo version is new), wait for CI + release
#
# Exits non-zero and prints the failing step's log tail on failure.
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
cd "$(git rev-parse --show-toplevel)"

repo=$(git remote get-url origin | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##')
branch=$(git branch --show-current)
sha=$(git rev-parse HEAD)
target=$branch
[ "${1:-}" = "--main" ] && target=main

git push -q origin "$branch:$target"

runs=""
for _ in $(seq 1 30); do
  runs=$(gh run list -R "$repo" --commit "$sha" --event push -L 10 --json databaseId,workflowName -q '.[] | "\(.databaseId) \(.workflowName)"')
  [ -n "$runs" ] && break
  sleep 2
done
[ -n "$runs" ] || { echo "no workflow runs started for ${sha:0:7} (pushes to feature branches may not trigger CI)"; exit 0; }

# Let runs start so the full set is listed, then watch them all.
sleep 5
runs=$(gh run list -R "$repo" --commit "$sha" --event push -L 10 --json databaseId,workflowName -q '.[] | "\(.databaseId) \(.workflowName)"')
failed=0; summary=""
while read -r id name; do
  [ -n "$id" ] || continue
  if gh run watch "$id" -R "$repo" --exit-status >/dev/null 2>&1; then
    summary="$summary $name:ok"
  else
    summary="$summary $name:FAILED"; failed=1
    gh run view "$id" -R "$repo" --log-failed 2>/dev/null | tail -40
  fi
done <<< "$runs"

if [ "$target" = "main" ] && [ $failed -eq 0 ]; then
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
  assets=$(gh release view "v$version" -R "$repo" --json isDraft,assets -q 'if .isDraft then "draft" else "\(.assets | length) assets" end' 2>/dev/null || echo "none")
  summary="$summary release v$version:$assets"
fi
echo "${sha:0:7} ->$target |$summary"
exit $failed
