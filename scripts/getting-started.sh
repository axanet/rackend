#!/usr/bin/env bash
# Runs docs/guide/getting-started.md verbatim (SPEC §20 M7): every ```bash
# block, in order, in one shell, in an empty directory. The guide uses the
# published image name; CI tags the image it just built with that name.
# Usage: scripts/getting-started.sh
set -euo pipefail
root="$(git rev-parse --show-toplevel)"
guide="$root/docs/guide/getting-started.md"
work="$(mktemp -d)"
cleanup() {
  docker rm -f rackend >/dev/null 2>&1 || true
  docker volume rm rackend_data >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

image=$(grep -oE 'ghcr\.io/axanet/rackend:[0-9.]+' "$guide" | head -1)
docker image inspect "$image" >/dev/null 2>&1 || { echo "image $image not found; build and tag it first"; exit 1; }
if docker ps -a --format '{{.Names}}' | grep -qx rackend; then
  echo "a container named rackend exists already; remove it first"; exit 1
fi

awk '/^```bash$/ { on = 1; next } /^```$/ { on = 0; next } on' "$guide" >"$work/guide.sh"
echo "running $(grep -c '' "$work/guide.sh") lines from $guide"
cd "$work"
bash -euo pipefail guide.sh | tee output.txt

# What a reader should see.
grep -q '"posts"' output.txt
grep -qx '{"title":"Hello, rackend"}' output.txt   # anonymous list
! grep -qx '{"title":"A draft"}' output.txt        # the draft stays hidden
grep -q '/api/collections/posts/records' output.txt
echo "getting started guide works"
