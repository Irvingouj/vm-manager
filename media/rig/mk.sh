#!/bin/bash
# Makes the recording rig's files for a fictional company, Acme, at the domain
# vibe.acme.test: a certificate authority of the rig's own, a wildcard
# certificate for the installation, a certificate for the identity provider
# (Keycloak), throwaway passwords, and Keycloak's realm with two people:
#   ada@acme.test    Platform administrator and Builder
#   grace@acme.test  the person the Application is shared with
# Every secret stays in a file under $T (mode 700); nothing is printed.
set -eu
T=${T74:-/tmp/t74}; D=vibe.acme.test; IDP_PORT=${T74_IDP_PORT:-18444}
mkdir -p "$T"; chmod 700 "$T"; cd "$T"; umask 077
mkdir -p pki kc
if [ ! -f pki/ca.key ]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout pki/ca.key -out pki/ca.pem -days 30 \
    -subj "/CN=Acme test authority" -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
  for n in wild idp; do
    if [ $n = wild ]; then san="DNS:*.$D"; else san="DNS:idp.$D"; fi
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout pki/$n.key -out pki/$n.csr -subj "/CN=$n.$D" 2>/dev/null
    printf 'subjectAltName=%s\nbasicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n' "$san" > pki/$n.ext
    openssl x509 -req -in pki/$n.csr -CA pki/ca.pem -CAkey pki/ca.key -CAcreateserial -out pki/$n.pem -days 20 -extfile pki/$n.ext 2>/dev/null
  done
  openssl rand -hex 20 > kc/client-secret
  openssl rand -hex 12 > kc/admin-password
  openssl rand -hex 8 > kc/ada-password
  openssl rand -hex 8 > kc/grace-password
fi
python3 -I - "$T" "$D" "$IDP_PORT" <<'PY'
import json, sys
t, d, port = sys.argv[1], sys.argv[2], sys.argv[3]
r = lambda n: open(f"{t}/kc/{n}").read().strip()
def user(name, first, last, groups):
    return {"username": name, "enabled": True, "emailVerified": True, "email": f"{name}@acme.test",
            "firstName": first, "lastName": last, "groups": groups, "realmRoles": ["offline_access"],
            "credentials": [{"type": "password", "value": r(f"{name}-password"), "temporary": False}]}
realm = {
  "realm": "acme", "displayName": "Acme", "enabled": True, "sslRequired": "all", "loginWithEmailAllowed": True,
  "groups": [{"name": "vibe-administrators"}, {"name": "staff"}],
  "users": [user("ada", "Ada", "Lovelace", ["/vibe-administrators", "/staff"]),
            user("grace", "Grace", "Hopper", ["/staff"])],
  "clients": [{
    "clientId": "vibe", "name": "Vibe Manager", "enabled": True, "protocol": "openid-connect", "publicClient": False,
    "secret": r("client-secret"), "standardFlowEnabled": True, "directAccessGrantsEnabled": False,
    "redirectUris": [f"https://auth.{d}:18443/callback", f"https://auth.{d}/callback"], "webOrigins": [],
    "defaultClientScopes": ["profile", "email"], "optionalClientScopes": ["offline_access"],
    "protocolMappers": [{"name": "groups", "protocol": "openid-connect", "protocolMapper": "oidc-group-membership-mapper",
      "config": {"claim.name": "groups", "full.path": "false", "id.token.claim": "true", "access.token.claim": "true", "userinfo.token.claim": "true"}}],
  }],
}
json.dump(realm, open(f"{t}/kc/acme-realm.json", "w"), indent=1)
PY
# Keycloak runs as its own user in its container and must read these (throwaway rig files).
chmod 644 pki/idp.pem pki/idp.key kc/acme-realm.json; chmod 755 pki kc
echo "rig files made under $T"
