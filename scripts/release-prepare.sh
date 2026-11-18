#!/usr/bin/env bash
# cocogitto pre-bump hook (cog.toml): everything the release commit carries
# besides CHANGELOG.md. Runs on the commit being released.
# Usage: scripts/release-prepare.sh <version>
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
version="${1:?usage: scripts/release-prepare.sh <version>}"

# 1. Workspace version; every crate inherits it. Cargo.lock follows with the
#    build in step 3.
perl -pi -e 'if (/^\[/) { $in = /^\[workspace\.package\]/ } s/^version = ".*"/version = "'"$version"'"/ if $in' Cargo.toml
grep -q "^version = \"$version\"\$" Cargo.toml || { echo "release-prepare: could not set the version in Cargo.toml" >&2; exit 1; }

# 2. Stable releases move the image tag the docs pin (x.y) to this release.
#    Pre-releases only get their full tag (release.yml), so the docs stay.
if [[ "$version" != *-* ]]; then
  minor="${version%.*}"
  # shellcheck disable=SC2016 # ${1} is the Perl capture, not a shell variable
  git grep -lE 'ghcr\.io/axanet/rackend:[0-9]+\.[0-9]+' -- ':!CHANGELOG.md' ':!.github/ISSUE_TEMPLATE' |
    xargs perl -pi -e 's{(ghcr\.io/axanet/rackend:)\d+\.\d+(?![.\d])}{${1}'"$minor"'}g'
fi

# 3. Upgrade fixture of this release (SPEC §7.7): the schema exactly as shipped.
cargo run --quiet -p rackend-store-sqlite --example generate_fixture -- tests/upgrade/fixtures "$version"
grep -A1 '^name = "rackend"$' Cargo.lock | grep -q "^version = \"$version\"\$" ||
  { echo "release-prepare: Cargo.lock does not have $version" >&2; exit 1; }
# CI does not run on the release commit when the workflow pushes it, so the
# new fixture must pass the upgrade tests before anything is committed.
cargo test --quiet -p rackend-store-sqlite --test upgrade_paths

git add -A Cargo.toml Cargo.lock tests/upgrade/fixtures
git add -u
