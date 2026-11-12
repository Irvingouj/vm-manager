#!/usr/bin/env bash
# One command: for each Postgres major, start a throwaway container, run every
# check and the cost measurement, print PASS/FAIL per check, remove the container.
#   ./run.sh            # Postgres 18 and 16
#   ./run.sh 17         # one or more majors of your choice
set -euo pipefail
cd "$(dirname "$0")"
[ $# -gt 0 ] && VERSIONS=("$@") || VERSIONS=(18 16)

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/pg-target}"
# pg_query builds libpg_query with bindgen, which needs libclang. If the system
# has none, point LIBCLANG_PATH at one (e.g. from the `libclang` PyPI wheel).
if [ -n "${LIBCLANG_PATH:-}" ] && [ -z "${BINDGEN_EXTRA_CLANG_ARGS:-}" ]; then
  inc=$(ls -d /usr/lib/gcc/x86_64-linux-gnu/*/include 2>/dev/null | tail -1 || true)
  [ -n "$inc" ] && export BINDGEN_EXTRA_CLANG_ARGS="-I$inc"
fi
cargo build --release --quiet
BIN="$CARGO_TARGET_DIR/release/pg-access-spike"

status=0
for v in "${VERSIONS[@]}"; do
  name="vm-pg-spike-$v"
  port=$((55400 + v))
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --name "$name" -e POSTGRES_PASSWORD=spike -p "127.0.0.1:$port:5432" "postgres:$v" >/dev/null
  trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
  for _ in $(seq 1 60); do
    docker exec "$name" pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1 && break
    sleep 0.5
  done
  sleep 1
  echo "=================== postgres:$v ==================="
  "$BIN" "$port" || status=1
  docker rm -f "$name" >/dev/null
done
exit $status
