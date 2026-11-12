#!/usr/bin/env bash
# Builds the host for the container, then runs it under the tightest Docker
# profile we could make and checks that it still serves.
#
#   lockdown.sh            (from prototypes/execution-core/lockdown)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$HERE/.."
TARGET="${CARGO_TARGET_DIR_LOCKDOWN:-/tmp/ec-target-docker}"
CTX="$(mktemp -d)"

(cd "$ROOT" && RUNTIME_SNAPSHOT_PATH=/app/runtime-snapshot.bin CARGO_TARGET_DIR="$TARGET" cargo build --release)
cp "$TARGET/release/ec-host" "$ROOT/guest/hello.js" "$HERE/Dockerfile" "$CTX/"
"$TARGET/release/ec-host" snapshot "$CTX/runtime-snapshot.bin"
docker build -q -t ec-host-lockdown "$CTX"

docker rm -f ec-lockdown >/dev/null 2>&1 || true
docker run -d --name ec-lockdown \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --ipc none \
  --user 65532:65532 \
  --pids-limit 64 \
  --memory 256m \
  -p 127.0.0.1:18787:8787 \
  ec-host-lockdown >/dev/null

for _ in $(seq 1 50); do curl -s -o /dev/null http://127.0.0.1:18787/ && break; sleep 0.1; done
curl -sS -i 'http://127.0.0.1:18787/?name=lockdown'; echo
curl -sS -w ' status=%{http_code}\n' http://127.0.0.1:18787/spin
docker inspect ec-lockdown --format 'ReadonlyRootfs={{.HostConfig.ReadonlyRootfs}} CapDrop={{.HostConfig.CapDrop}} CapAdd={{.HostConfig.CapAdd}} SecurityOpt={{.HostConfig.SecurityOpt}} Mounts={{len .Mounts}} Ipc={{.HostConfig.IpcMode}} Tmpfs={{.HostConfig.Tmpfs}} User={{.Config.User}} Ports={{.HostConfig.PortBindings}}'
docker logs ec-lockdown
docker rm -f ec-lockdown >/dev/null
rm -rf "$CTX"
