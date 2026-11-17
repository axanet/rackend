#!/bin/sh
# Waits for Caddy's local CA, makes the suite trust it, starts the suite.
set -e
until [ -f /caddy/caddy/pki/authorities/local/root.crt ]; do sleep 1; done
mkdir -p /certificates
cp /caddy/caddy/pki/authorities/local/root.crt /certificates/caddy.crt
exec /__cacert_entrypoint.sh java \
  -Dfintechlabs.base_url="$BASE_URL" \
  -Dfintechlabs.base_mtls_url="$BASE_MTLS_URL" \
  -Dspring.mongodb.uri="mongodb://$MONGODB_HOST:27017/test_suite" \
  $JAVA_EXTRA_ARGS \
  -jar /server/fapi-test-suite.jar -Djdk.tls.maxHandshakeMessageSize=65536
