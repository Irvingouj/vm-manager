#!/usr/bin/env bash
# Runs the research of docs/research/change-history-out-of-reach.md again.
#
#   docs/research/change-history-out-of-reach/run.sh [VERSIONS] [PARTS]
#
# VERSIONS: any of 16 17 18 (default "16 17 18"); PARTS: any of a b c d e
# (default all). For each version and part: a scratch cluster from the
# binaries in ~/.cache/vm-blackbox (the black-box tests' cache) is made
# under /dev/shm (part c's measurements: under the temporary directory, on
# disk, so commits pay for fsync), set up by setup.sql, used, stopped and
# deleted. Passwords are the roles' names: scratch clusters on 127.0.0.1
# only. Needs bash and python3 (standard library only); part b also runs
# parse/ with cargo if cargo is on PATH. SCEN=file.sql replaces part a's
# scenario, for trying one attack.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
VERSIONS="${1:-16 17 18}"
PARTS="${2:-a b c d e}"
CACHE="$HOME/.cache/vm-blackbox"
declare -A FULL=([16]=16.15.0 [17]=17.11.0 [18]=18.6.0)

DIR=""
BIN=""
cleanup() {
  if [ -n "$DIR" ] && [ -d "$DIR" ]; then
    "$BIN/pg_ctl" -D "$DIR/data" -m immediate stop >/dev/null 2>&1 || true
    rm -rf "$DIR"
  fi
  DIR=""
}
trap cleanup EXIT INT TERM

start() { # start VERSION BASEDIR [extra postgres options...]
  local v=$1 base=$2
  shift 2
  BIN="$CACHE/postgres-${FULL[$v]}-linux-amd64/bin"
  export PQ_LIB="$CACHE/postgres-${FULL[$v]}-linux-amd64/lib/libpq.so.5" PGV=$v
  DIR=$(mktemp -d "$base/chr-pg$v-XXXXXX")
  echo postgres >"$DIR/pw"
  "$BIN/initdb" -D "$DIR/data" -U postgres --pwfile="$DIR/pw" --auth=scram-sha-256 \
    -E UTF8 --locale=C >"$DIR/initdb.log" 2>&1
  PORT=$(python3 -I -B -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
  export PORT
  "$BIN/pg_ctl" -D "$DIR/data" -l "$DIR/server.log" -w -o \
    "-p $PORT -k $DIR -c listen_addresses=127.0.0.1 $*" start >/dev/null
  python3 -I -B "$HERE/scenario.py" "$PORT" "$HERE/setup.sql" | grep -v as-expected || true
}

for v in $VERSIONS; do
  for part in $PARTS; do
    echo "######## Postgres $v (${FULL[$v]}), part $part"
    case $part in
      a)
        start "$v" /dev/shm
        python3 -I -B "$HERE/scenario.py" "$PORT" "${SCEN:-$HERE/a-attacks.sql}"
        ;;
      b)
        start "$v" /dev/shm
        python3 -I -B "$HERE/scenario.py" "$PORT" "$HERE/b-checks.sql"
        python3 -I -B "$HERE/b_counters.py" "$PORT"
        python3 -I -B "$HERE/b_tx.py" "$PORT"
        if [ "$v" = "${VERSIONS##* }" ] && command -v cargo >/dev/null; then
          # The parser does not depend on the server's version: once.
          CARGO_TARGET_DIR="${TMPDIR:-/tmp}/chr-parse-target" cargo run -q --manifest-path "$HERE/parse/Cargo.toml"
        fi
        ;;
      c)
        start "$v" "${TMPDIR:-/tmp}"
        python3 -I -B "$HERE/c_relay.py" "$PORT"
        ;;
      d)
        start "$v" /dev/shm
        python3 -I -B "$HERE/d_read.py" "$PORT"
        ;;
      e)
        start "$v" /dev/shm
        python3 -I -B "$HERE/scenario.py" "$PORT" "$HERE/e-alternatives.sql"
        cleanup
        start "$v" /dev/shm -c wal_level=logical -c max_prepared_transactions=2
        python3 -I -B "$HERE/scenario.py" "$PORT" "$HERE/e-logical.sql"
        ;;
    esac
    cleanup
  done
done
