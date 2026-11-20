#!/bin/bash
# Tears the rig down: the installation (with its volume), the identity
# provider, the network, and the browsers the capture started (by pid).
# Keeps the files under $T.
set -u
T=${T74:-/tmp/t74}
if [ -f "$T/opt/vibe/compose.yaml" ]; then
  (cd "$T/opt/vibe" && COMPOSE_PROJECT_NAME=t74-vibe COMPOSE_FILE=compose.yaml:compose.t74.yaml docker compose down --volumes)
fi
docker rm -f t74-idp 2>/dev/null
docker network rm t74-net 2>/dev/null
for f in "$T"/run/*.pid; do
  [ -f "$f" ] || continue
  pid=$(cat "$f"); kill "$pid" 2>/dev/null; rm -f "$f"
done
echo "containers named t74: $(docker ps -a --format '{{.Names}}' | grep -c t74)"
