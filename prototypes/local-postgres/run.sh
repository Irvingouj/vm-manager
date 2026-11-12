#!/usr/bin/env bash
# One command: build, then in a fresh per-run directory whose path has a space
# and non-ASCII characters, run two Postgres sessions (first: download + initdb +
# start; later: reuse) with all checks, then the *.localhost checks. Prints
# METRIC / PASS / FAIL lines and removes the directory at the end.
#   ./run.sh            # or: ./run.sh path/to/prebuilt/lp
set -uo pipefail
cd "$(dirname "$0")"

if [ $# -gt 0 ]; then
  BIN="$1"
else
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/lp-target}"
  cargo build --release --quiet || exit 1
  BIN="$CARGO_TARGET_DIR/release/lp"
  [ -f "$BIN.exe" ] && BIN="$BIN.exe"
fi

base="${LP_BASE:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}}"
ROOT="${LP_ROOT:-$base/lp spike Zoë 数据 $$}"
status=0

echo "=================== session 1 (cold) ==================="
"$BIN" pg "$ROOT" || status=1
echo "=================== session 2 (warm) ==================="
"$BIN" pg "$ROOT" || status=1
echo "=================== *.localhost ==================="
"$BIN" localhost || status=1
if command -v curl >/dev/null; then
  LP_SERVE_SECS=6 "$BIN" localhost > lp-serve.log 2>&1 &
  for _ in $(seq 1 50); do grep -q LOCALHOST_PORT lp-serve.log 2>/dev/null && break; sleep 0.1; done
  port=$(sed -n 's/^LOCALHOST_PORT //p' lp-serve.log | tr -d '\r')
  if out=$(curl -sS --max-time 5 "http://my-app.localhost:$port/" 2>&1); then
    echo "PASS curl http://my-app.localhost:$port/ -> $out"
  else
    echo "FAIL curl http://my-app.localhost:$port/ -> $out"; status=1
  fi
  echo "curl version: $(curl --version | head -1)"
  wait
  rm -f lp-serve.log
fi

[ "${LP_KEEP:-}" = 1 ] || rm -rf "$ROOT"
[ $status -eq 0 ] && echo "RUN: PASS" || echo "RUN: FAIL"
exit $status
