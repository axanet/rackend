#!/usr/bin/env bash
# Runs the OpenID Foundation conformance suite (Basic OP and Config OP) against
# rackend in Docker (SPEC §11.8.7, §20 M8). Needs Docker and network access.
#
# Usage: scripts/conformance/run.sh            (builds rackend:conformance if missing)
#        RACKEND_IMAGE=rackend:ci scripts/conformance/run.sh
#        KEEP=1 scripts/conformance/run.sh     (leaves the containers running)
set -euo pipefail
cd "$(dirname "$0")"
root="$(git rev-parse --show-toplevel)"

# The suite's runner scripts, fetched at a pinned commit (not vendored).
SUITE_COMMIT=e8bd0c092efb9e19427d97160171d222b8bd9d5b
PLANS=(
  "oidcc-basic-certification-test-plan[server_metadata=discovery][client_registration=static_client]"
  "oidcc-config-certification-test-plan"
)
export RACKEND_IMAGE="${RACKEND_IMAGE:-rackend:conformance}"
compose=(docker compose -f compose.yml)
work="$(pwd)/.run"

cleanup() {
  if [ -z "${KEEP:-}" ]; then
    "${compose[@]}" --profile run down -v >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

if ! docker image inspect "$RACKEND_IMAGE" >/dev/null 2>&1; then
  docker build -t "$RACKEND_IMAGE" "$root"
fi

mkdir -p "$work/certs-keys"
for f in run-test-plan.py conformance.py test_plan_parser.py; do
  [ -f "$work/$f" ] || curl -fsSL -o "$work/$f" "https://gitlab.com/openid/conformance-suite/-/raw/$SUITE_COMMIT/scripts/$f"
done

"${compose[@]}" up -d mongodb caddy rackend suite
base=http://127.0.0.1:18093
for _ in $(seq 1 60); do curl -fsS "$base/api/health" >/dev/null 2>&1 && break; sleep 1; done
# Signing keys are generated in the background at first start.
for _ in $(seq 1 60); do curl -fsS "$base/oauth2/jwks" >/dev/null 2>&1 && break; sleep 1; done

echo "== rackend: test user and two clients"
token=$(curl -fsS -H 'content-type: application/json' \
  -d '{"email":"admin@example.com","password":"conformance-admin-password"}' \
  "$base/api/superusers/auth-with-password" | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')
api() { curl -fsS -H "authorization: Bearer $token" -H 'content-type: application/json' "$@"; }
# Every suite login comes from the proxy's address: turn the per-IP login
# limit off for this throwaway instance (the suite signs in ~40 times a minute).
api -X PATCH -d '{"security":{"rate_limits":{"login_ip_per_min":0}}}' "$base/api/settings" >/dev/null
api -d '{"email":"conformance@example.com","password":"conformance user password","name":"Conformance User","email_verified":true}' \
  "$base/api/collections/users/records" >/dev/null
callback="https://conformance.test:8443/test/a/rackend/callback"
client() {
  api -d "{\"name\":\"$1\",\"kind\":\"confidential\",\"collection\":\"users\",\"first_party\":true,\"require_pkce\":false,
    \"redirect_uris\":[\"$callback\"],\"post_logout_redirect_uris\":[\"https://conformance.test:8443/test/a/rackend/post_logout_redirect\"]}" \
    "$base/api/oauth-clients"
}
c1=$(client "Conformance 1")
c2=$(client "Conformance 2")
field() { python3 -c "import json,sys; print(json.loads(sys.argv[1])[sys.argv[2]])" "$1" "$2"; }
sed -e "s|{CLIENT1_ID}|$(field "$c1" id)|" -e "s|{CLIENT1_SECRET}|$(field "$c1" client_secret)|" \
    -e "s|{CLIENT2_ID}|$(field "$c2" id)|" -e "s|{CLIENT2_SECRET}|$(field "$c2" client_secret)|" \
    config.template.json >"$work/config.json"

echo "== conformance suite"
plan_args=()
for p in "${PLANS[@]}"; do plan_args+=("$p" config.json); done
"${compose[@]}" --profile run run --rm -w /work/.run runner sh -c '
  pip install -q httpx pyparsing >/dev/null &&
  python - <<PY
import time, httpx
for _ in range(180):
    try:
        if httpx.get("https://conformance.test:8443/api/runner/available", verify=False).status_code < 500:
            break
    except Exception:
        pass
    time.sleep(2)
PY
  python run-test-plan.py --expected-failures-file ../expected-failures.json --expected-skips-file ../expected-skips.json "$@"
' sh "${plan_args[@]}"
