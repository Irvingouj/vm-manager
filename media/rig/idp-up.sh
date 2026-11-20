#!/bin/bash
# Starts the rig's identity provider: Keycloak 26 at https://idp.vibe.acme.test:18444,
# on a network of the rig's own (t74-net) under that name, and published on
# 127.0.0.1:18444 for the browser. The installation joins t74-net (compose.t74.yaml).
set -eu
T=${T74:-/tmp/t74}; P=${T74_IDP_PORT:-18444}
docker network inspect t74-net >/dev/null 2>&1 || docker network create t74-net >/dev/null
docker run -d --name t74-idp --network t74-net --network-alias idp.vibe.acme.test \
  -p 127.0.0.1:$P:$P \
  -e KC_BOOTSTRAP_ADMIN_USERNAME=admin -e KC_BOOTSTRAP_ADMIN_PASSWORD="$(cat "$T/kc/admin-password")" \
  -v "$T/pki:/etc/t74-pki:ro" -v "$T/kc/acme-realm.json:/opt/keycloak/data/import/acme-realm.json:ro" \
  quay.io/keycloak/keycloak:26.4 start-dev --import-realm --http-enabled=false --https-port=$P \
  --https-certificate-file=/etc/t74-pki/idp.pem --https-certificate-key-file=/etc/t74-pki/idp.key \
  --hostname=https://idp.vibe.acme.test:$P >/dev/null
for i in $(seq 1 90); do
  if curl -fsS --max-time 3 --cacert "$T/pki/ca.pem" --resolve idp.vibe.acme.test:$P:127.0.0.1 \
      "https://idp.vibe.acme.test:$P/realms/acme/.well-known/openid-configuration" >/dev/null 2>&1; then
    echo "identity provider up"; exit 0; fi
  sleep 2
done
echo "identity provider did not come up: docker logs t74-idp" >&2; exit 1
