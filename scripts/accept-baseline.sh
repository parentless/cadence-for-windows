#!/usr/bin/env bash
# Accept CI-captured screenshots as the new visual baseline.
#
#   scripts/accept-baseline.sh            dispatch "Regenerate screenshots" on this
#                                         branch, wait, then accept
#   scripts/accept-baseline.sh <run-id>   accept an existing run's artifact
#
# Copies only images whose bytes changed, refreshes manifest.json provenance, and
# prints the changed names so you can review exactly those. Does not commit.
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
cd "$(git rev-parse --show-toplevel)"

repo=$(git remote get-url origin | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##')
baseline=tests/visual/baseline
run=${1:-}

if [ -z "$run" ]; then
  branch=$(git branch --show-current)
  sha=$(git rev-parse HEAD)
  git ls-remote --exit-code origin "refs/heads/$branch" >/dev/null || { echo "push $branch first"; exit 1; }
  [ "$(git ls-remote origin "refs/heads/$branch" | cut -f1)" = "$sha" ] || { echo "push $branch first: remote is not at HEAD"; exit 1; }
  gh workflow run regenerate-screenshots.yml -R "$repo" --ref "$branch" >/dev/null
  for _ in $(seq 1 30); do
    run=$(gh run list -R "$repo" --workflow regenerate-screenshots.yml --branch "$branch" --event workflow_dispatch -L 1 \
      --json databaseId,headSha -q ".[] | select(.headSha==\"$sha\") | .databaseId")
    [ -n "$run" ] && break
    sleep 2
  done
  [ -n "$run" ] || { echo "could not find the dispatched run"; exit 1; }
  echo "waiting for run $run"
  gh run watch "$run" -R "$repo" --exit-status >/dev/null
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
gh run download "$run" -R "$repo" -D "$tmp" >/dev/null
art=$(find "$tmp" -maxdepth 1 -mindepth 1 -type d | head -1)
[ -n "$art" ] || { echo "run $run has no artifact"; exit 1; }

changed=(); added=(); same=0
for image in "$art"/*.png; do
  name=$(basename "$image")
  if [ ! -f "$baseline/$name" ]; then added+=("$name")
  elif cmp -s "$image" "$baseline/$name"; then same=$((same + 1))
  else changed+=("$name"); fi
done

echo "identical: $same   changed: ${#changed[@]}   new: ${#added[@]}"
if [ $((${#changed[@]} + ${#added[@]})) -eq 0 ]; then
  echo "baseline already matches; nothing to do"
  exit 0
fi
# ${arr[@]+...} keeps empty arrays safe under `set -u` on macOS bash 3.2.
for name in ${changed[@]+"${changed[@]}"} ${added[@]+"${added[@]}"}; do
  [ -n "$name" ] || continue
  cp -f "$art/$name" "$baseline/$name"
  echo "  $name"
done

revision=$(sed -n 's/^Source revision: //p' "$art/environment.txt")
lock=$(shasum -a 256 Cargo.lock | cut -d' ' -f1)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
python3 -I - "$baseline/manifest.json" "$version" "$revision" "$lock" "https://github.com/$repo/actions/runs/$run" <<'PY'
import json, sys
path, version, revision, lock, run = sys.argv[1:]
data = json.load(open(path))
def stamp(source):
    source.update(cadence=version, revision=revision, lockfile_sha256=lock)
stamp(data["source"])
data["capture_run"] = run
details = data.get("keyboard_focus_details")
if details:
    stamp(details["source"])
    details["capture_run"] = run
with open(path, "w") as out:
    json.dump(data, out, indent=2, ensure_ascii=False)
    out.write("\n")
PY
echo "updated manifest.json provenance. Review the images above, edit reviewed_changes, then commit."
