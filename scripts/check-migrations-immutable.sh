#!/usr/bin/env sh
# Released migrations MUST NEVER be edited (SPEC §7.2).
# Compares every migration file that exists at the latest release tag with HEAD.
set -eu

cd "$(git rev-parse --show-toplevel)"
# versionsort.suffix: 0.2.0-alpha.1 sorts before 0.2.0, not after it.
tag=$(git -c versionsort.suffix=- tag --list --sort=-v:refname | grep -E '^v?[0-9]+\.[0-9]+\.[0-9]+' | head -n 1 || true)
if [ -z "$tag" ]; then
  echo "check-migrations-immutable: no release tag yet, nothing to compare"
  exit 0
fi

status=0
for dir in crates/store-sqlite/migrations/data crates/store-sqlite/migrations/logs; do
  for file in $(git ls-tree --name-only "$tag" "$dir/" 2>/dev/null); do
    if ! git diff --quiet "$tag" -- "$file"; then
      echo "ERROR: $file was changed after release $tag. Add a new migration instead." >&2
      status=1
    fi
  done
done
[ "$status" -eq 0 ] && echo "check-migrations-immutable: migrations of $tag are unchanged"
exit "$status"
