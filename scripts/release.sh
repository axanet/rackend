#!/usr/bin/env bash
# Cut a release (SPEC §7.5). The version comes from the Conventional Commits
# since the latest tag (cog.toml). cocogitto runs scripts/release-prepare.sh,
# commits "chore(version): x.y.z" with CHANGELOG.md, tags x.y.z and pushes
# both; the tag builds and publishes the image (.github/workflows/release.yml).
# Usually started from GitHub (Actions, release, Run workflow), which also
# checks that CI passed on the commit.
# Usage: scripts/release.sh [--pre <identifier> | --version <x.y.z>] [--dry-run]
#   --pre alpha       pre-release, e.g. 0.2.0-alpha.1, then 0.2.0-alpha.2
#   --version 1.0.0   this version instead of the one from the commits; the
#                     only way to 1.0.0, as breaking changes in 0.x bump the minor
#   --dry-run         print the next version, change nothing
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

usage() { echo "usage: scripts/release.sh [--pre <identifier> | --version <x.y.z>] [--dry-run]" >&2; exit 2; }
pre="" version="" dry=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pre) [ -n "${2:-}" ] || usage; pre="$2"; shift 2 ;;
    --version) [ -n "${2:-}" ] || usage; version="$2"; shift 2 ;;
    --dry-run) dry=1; shift ;;
    *) usage ;;
  esac
done
[ -z "$pre" ] || [ -z "$version" ] || usage
if [ -n "$version" ]; then args=(--version "$version"); else args=(--auto); fi
if [ -n "$pre" ]; then args+=(--pre "$pre.*"); fi
if [ -n "$dry" ]; then args+=(--dry-run); fi

fail() { echo "release: $*" >&2; exit 1; }
[ "$(git branch --show-current)" = main ] || fail "release from main only"
[ -z "$(git status --porcelain)" ] || fail "the working tree has changes"
git fetch --quiet --tags origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || fail "main differs from origin/main; pull or push first"

cog bump "${args[@]}"
