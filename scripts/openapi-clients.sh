#!/usr/bin/env bash
# M7 acceptance (SPEC §10.12, §20 M7): starts rackend with a fixture schema
# that covers every field type, validates the OpenAPI documents (3.1 and
# 3.0, JSON and YAML, with and without admin endpoints), generates
# TypeScript (typescript-fetch) and Python clients with openapi-generator,
# compiles them, and runs a smoke test with each against the instance.
#
# Needs: docker, python3, bun, curl, jq. Usage: scripts/openapi-clients.sh [rackend binary]
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

GENERATOR_IMAGE="openapitools/openapi-generator-cli:v7.17.0"
VALIDATOR_VERSION="0.9.0"
TYPESCRIPT_VERSION="5.9.3"
bin="${1:-target/debug/rackend}"
port=18091
base="http://127.0.0.1:$port"
# KEEP_WORK=<dir> keeps the instance data, documents and clients there.
work="${KEEP_WORK:-$(mktemp -d)}"
mkdir -p "$work"
server_pid=""
cleanup() {
  [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null || true
  [ -n "${KEEP_WORK:-}" ] || rm -rf "$work"
}
trap cleanup EXIT

[ -x "$bin" ] || cargo build -p rackend
echo "== start rackend ($bin)"
RACKEND_DATA_DIR="$work/data" RACKEND_HTTP_ADDR="127.0.0.1:$port" RACKEND_PUBLIC_URL="$base" \
  RACKEND_SUPERUSER_EMAIL=admin@example.com RACKEND_SUPERUSER_PASSWORD=openapi-check-password \
  RACKEND_LOG_LEVEL=warn "$bin" serve &
server_pid=$!
for _ in $(seq 1 60); do curl -fsS "$base/api/health" >/dev/null 2>&1 && break; sleep 0.5; done
curl -fsS "$base/api/health" >/dev/null

su_token=$(curl -fsS -H 'content-type: application/json' \
  -d '{"email":"admin@example.com","password":"openapi-check-password"}' \
  "$base/api/superusers/auth-with-password" | jq -r .access_token)
api() { curl -fsS -H "authorization: Bearer $su_token" -H 'content-type: application/json' "$@"; }

echo "== fixture schema"
authors=$(api -d '{"name":"authors","fields":[{"name":"name","type":"text","required":true}],
  "rules":{"list":"","view":""}}' "$base/api/collections" | jq -r .id)
api -d "$(jq -n --arg a "$authors" '{
  name: "kitchen_sink",
  fields: [
    {name: "title", type: "text", required: true, min: 1, max: 200, pattern: "^[^<>]*$"},
    {name: "contact", type: "email"},
    {name: "homepage", type: "url"},
    {name: "count", type: "number", integer_only: true, min: 0},
    {name: "score", type: "number"},
    {name: "done", type: "bool"},
    {name: "due", type: "date"},
    {name: "stamp", type: "autodate", on_create: true, on_update: true},
    {name: "status", type: "select", values: ["draft", "published"]},
    {name: "labels", type: "select", values: ["a", "b", "c"], max_select: 3},
    {name: "meta", type: "json"},
    {name: "author", type: "relation", collection_id: $a},
    {name: "coauthors", type: "relation", collection_id: $a, max_select: 5},
    {name: "cover", type: "file", mime_types: ["image/png", "image/jpeg"]},
    {name: "documents", type: "file", max_select: 4}
  ],
  rules: {list: "", view: "", create: "@request.auth.id != \"\"", update: "@request.auth.id != \"\"", delete: null}
}')" "$base/api/collections" >/dev/null
# The collection the clients use.
own='@request.auth.id != "" && owner = @request.auth.id'
api -d "$(jq -n --arg own "$own" '{
  name: "notes",
  fields: [
    {name: "title", type: "text", required: true},
    {name: "views", type: "number", integer_only: true},
    {name: "pinned", type: "bool"},
    {name: "tags", type: "select", values: ["work", "home"], max_select: 2},
    {name: "owner", type: "relation", collection_id: "PLACEHOLDER"},
    {name: "attachment", type: "file"}
  ],
  rules: {list: $own, view: $own, create: $own, update: $own, delete: $own}
}' | jq --arg users "$(api "$base/api/collections/users" | jq -r .id)" \
  '.fields |= map(if .name == "owner" then .collection_id = $users else . end)')" \
  "$base/api/collections" >/dev/null

echo "== documents"
docs="$work/docs"
mkdir -p "$docs"
for v in 3.1 3.0; do
  api "$base/api/openapi.json?version=$v" >"$docs/openapi-$v.json"
  api "$base/api/openapi.yaml?version=$v" >"$docs/openapi-$v.yaml"
  api "$base/api/openapi.json?version=$v&include=admin" >"$docs/openapi-$v-admin.json"
done
# The CLI reads the database of the running server (read only) and agrees.
RACKEND_DATA_DIR="$work/data" RACKEND_PUBLIC_URL="$base" "$bin" openapi export --version 3.1 >"$docs/cli-3.1.json"
cmp "$docs/cli-3.1.json" "$docs/openapi-3.1.json" || { echo "CLI and HTTP documents differ"; exit 1; }
# Deterministic output.
api "$base/api/openapi.json" | cmp - "$docs/openapi-3.1.json"

echo "== validate"
python3 -m venv "$work/venv"
"$work/venv/bin/pip" install -q "openapi-spec-validator==$VALIDATOR_VERSION" pyyaml
for f in "$docs"/*.json "$docs"/*.yaml; do
  "$work/venv/bin/openapi-spec-validator" "$f" >/dev/null || { echo "invalid: $f"; exit 1; }
  echo "valid: $(basename "$f")"
done
"$work/venv/bin/python" - "$docs" <<'PY'
import json, sys, yaml
d = sys.argv[1]
for v in ("3.1", "3.0"):
    assert yaml.safe_load(open(f"{d}/openapi-{v}.yaml")) == json.load(open(f"{d}/openapi-{v}.json")), v
print("yaml matches json")
PY

echo "== generate clients"
gen() {
  docker run --rm --user "$(id -u):$(id -g)" -v "$work:/work" "$GENERATOR_IMAGE" generate \
    -i "/work/docs/$1" -g "$2" -o "/work/$3" "${@:4}" >"$work/$3.log" 2>&1 || { cat "$work/$3.log"; exit 1; }
}
gen openapi-3.1.json typescript-fetch ts --additional-properties=supportsES6=true,npmName=rackend-client
gen openapi-3.1.json python py --additional-properties=packageName=rackend_client,projectName=rackend-client

echo "== typescript: compile and smoke"
(
  cd "$work/ts"
  cp "$OLDPWD/scripts/clients/smoke.ts" .
  bun add -d --silent --ignore-scripts "typescript@$TYPESCRIPT_VERSION" >/dev/null
  bun --bun x tsc --noEmit --strict --target es2022 --module esnext --moduleResolution bundler --lib es2022,dom smoke.ts
  RACKEND_URL="$base" bun run smoke.ts
)

echo "== python: install and smoke"
"$work/venv/bin/pip" install -q "$work/py"
"$work/venv/bin/python" -m compileall -q "$work/py/rackend_client"
RACKEND_URL="$base" "$work/venv/bin/python" scripts/clients/smoke.py

echo "== clients from the 3.0 document compile too"
gen openapi-3.0.json typescript-fetch ts30 --additional-properties=supportsES6=true,npmName=rackend-client
gen openapi-3.0.json python py30 --additional-properties=packageName=rackend_client30,projectName=rackend-client30
(
  cd "$work/ts30"
  cp "$OLDPWD/scripts/clients/smoke.ts" .
  bun add -d --silent --ignore-scripts "typescript@$TYPESCRIPT_VERSION" >/dev/null
  bun --bun x tsc --noEmit --strict --target es2022 --module esnext --moduleResolution bundler --lib es2022,dom smoke.ts
)
"$work/venv/bin/pip" install -q "$work/py30"
"$work/venv/bin/python" -c "import rackend_client30; rackend_client30.NotesApi; rackend_client30.UsersApi"

echo "openapi clients check passed"
