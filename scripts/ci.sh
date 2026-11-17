#!/usr/bin/env bash
# Shared CI entry point for GitHub Actions and Forgejo Actions (SPEC §19).
# Usage: scripts/ci.sh <lint|test|upgrade|clients|docker-smoke IMAGE>
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

case "${1:-}" in
  lint)
    mise run lint
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
    echo "usage: $0 <lint|test|upgrade|clients|docker-smoke IMAGE>" >&2
    exit 2
    ;;
esac
