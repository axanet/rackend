#!/usr/bin/env bash
# Shared CI entry point for GitHub Actions and Forgejo Actions (SPEC §19).
# Usage: scripts/ci.sh <lint|commits [FROM..TO]|pr-title TITLE BASE..HEAD|test|upgrade|clients|collab|docker-smoke IMAGE>
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

case "${1:-}" in
  lint)
    mise run lint
    ;;
  commits)
    # Conventional Commits (cog.toml) in FROM..TO, by default since the latest
    # tag. Unlike git, cog checks FROM itself too. Merge commits are skipped;
    # fixup!/squash! commits must not land. Before the first release the
    # default starts after "Initial commit", which predates the convention.
    range="${2:-}"
    # A push that creates the branch has no "before" commit, and after a
    # force push "before" is no longer in the history (nor in a fresh clone).
    if [ -n "$range" ] && ! git merge-base --is-ancestor "${range%%..*}" "${range##*..}" 2>/dev/null; then
      echo "commits: ${range%%..*} is not in the history of ${range##*..} (new branch or force push), checking the default range" >&2
      range=""
    fi
    if [ -z "$range" ]; then
      from=$(git describe --tags --abbrev=0 2>/dev/null || git rev-list --reverse HEAD | sed -n 2p)
      range="$from..HEAD"
    fi
    cog check "$range"
    if git log --format=%B "$range" | grep -q "$(printf '\342\200\224')"; then
      echo "a commit message in $range uses the em dash (CONTRIBUTING.md)" >&2
      exit 1
    fi
    ;;
  pr-title)
    # A squash merge commits the PR title to main, so it must be a
    # Conventional Commit too, and must be marked breaking (feat!: ...) when
    # a commit in the PR is: the squash keeps no other marker.
    usage="usage: scripts/ci.sh pr-title TITLE BASE..HEAD"
    title="${2:?$usage}"
    range="${3:?$usage}"
    # cog verify reads the author from git; CI checkouts have no identity.
    git config user.name >/dev/null || git config user.name ci
    git config user.email >/dev/null || git config user.email ci@localhost
    if ! out=$(cog verify "$title" 2>&1); then
      echo "PR title: $out" >&2
      exit 1
    fi
    if printf '%s' "$title" | grep -q "$(printf '\342\200\224')"; then
      echo "PR title: never use the em dash (CONTRIBUTING.md)" >&2
      exit 1
    fi
    breaking='^[a-z]+(\([^)]*\))?!:'
    if git log --format=%s "$range" | grep -qE "$breaking" ||
      git log --format=%b "$range" | grep -qE '^BREAKING[ -]CHANGE:'; then
      printf '%s' "$title" | grep -qE "$breaking" || {
        echo "PR title: a commit in this PR is a breaking change; mark the title with ! (e.g. feat!: ...)" >&2
        exit 1
      }
    fi
    ;;
  test)
    mise run test
    ;;
  upgrade)
    mise run test:upgrade
    ;;
  clients)
    mise run test:clients
    ;;
  collab)
    mise run test:collab
    ;;
  docker-smoke)
    image="${2:?image required}"
    vol="rackend-smoke-$$"
    name="rackend-smoke-$$"
    cleanup() { docker rm -f "$name" "$name-2" >/dev/null 2>&1 || true; docker volume rm "$vol" >/dev/null 2>&1 || true; }
    trap cleanup EXIT

    docker run -d --name "$name" -v "$vol:/data" -p 127.0.0.1:18080:8080 \
      -e RACKEND_SUPERUSER_EMAIL=smoke@example.com -e RACKEND_SUPERUSER_PASSWORD=smoke-test-password \
      "$image"
    for _ in $(seq 1 60); do
      status=$(docker inspect -f '{{.State.Health.Status}}' "$name")
      [ "$status" = healthy ] && break
      sleep 1
    done
    [ "$status" = healthy ] || { docker logs "$name"; echo "container never became healthy"; exit 1; }
    curl -fsS http://127.0.0.1:18080/api/health | grep -q '"status":"ok"'
    curl -sS http://127.0.0.1:18080/api/nope -o /dev/null -w '%{content_type}' | grep -q 'application/problem+json'

    # Headless setup via env, then a simple authenticated API call.
    token=$(curl -fsS -H 'content-type: application/json' \
      -d '{"email":"smoke@example.com","password":"smoke-test-password"}' \
      http://127.0.0.1:18080/api/superusers/auth-with-password | sed -E 's/.*"access_token":"([^"]+)".*/\1/')
    curl -fsS -H "authorization: Bearer $token" -H 'content-type: application/json' \
      -d '{"name":"smoke","fields":[{"name":"note","type":"text"}],"rules":{"create":"","list":""}}' \
      http://127.0.0.1:18080/api/collections >/dev/null
    curl -fsS -H 'content-type: application/json' -d '{"note":"hello"}' \
      http://127.0.0.1:18080/api/collections/smoke/records >/dev/null
    curl -fsS http://127.0.0.1:18080/api/collections/smoke/records | grep -q '"note":"hello"'
    curl -fsS -H "authorization: Bearer $token" http://127.0.0.1:18080/api/openapi.json | grep -q '"smoke_list"'

    # A second container on the same volume must refuse to start (exit 75).
    set +e
    docker run --name "$name-2" -v "$vol:/data" "$image"
    code=$?
    set -e
    [ "$code" -eq 75 ] || { echo "second container exited $code, expected 75"; exit 1; }

    # Graceful stop exits 0.
    docker stop -t 30 "$name" >/dev/null
    code=$(docker inspect -f '{{.State.ExitCode}}' "$name")
    [ "$code" -eq 0 ] || { docker logs "$name"; echo "stop exited $code"; exit 1; }
    echo "docker smoke test passed"
    ;;
  *)
    echo "usage: $0 <lint|test|upgrade|clients|collab|docker-smoke IMAGE>" >&2
    exit 2
    ;;
esac
