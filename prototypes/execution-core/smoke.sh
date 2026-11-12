#!/usr/bin/env bash
# Smoke test: start the host, call the guest (which calls the host function),
# then check that a busy loop in the guest is cut by the CPU limit, not the
# wall clock. Runs under bash on Linux, macOS and Windows (Git Bash).
#
#   smoke.sh <path-to-ec-host> [port]
set -u

HOST_BIN="$1"
PORT="${2:-8787}"
HERE="$(cd "$(dirname "$0")" && pwd)"
LOG="${SMOKE_LOG:-smoke-host.log}"

case "$(uname -s)" in
  Linux*) EXPECT_OS=linux ;;
  Darwin*) EXPECT_OS=macos ;;
  MINGW*|MSYS*|CYGWIN*) EXPECT_OS=windows ;;
  *) EXPECT_OS=unknown ;;
esac

# The wall clock is 10 s; the CPU limit is 50 ms. A /spin answered with
# CpuTimeLimit well under the wall clock proves the CPU limit works.
EC_CPU_MS=50 EC_WALL_MS=10000 "$HOST_BIN" serve "$HERE/guest/hello.js" "127.0.0.1:$PORT" >"$LOG" 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null; wait $PID 2>/dev/null' EXIT

for _ in $(seq 1 100); do
  curl -s -o /dev/null "http://127.0.0.1:$PORT/" && break
  sleep 0.1
done

fail=0

echo "== GET /?name=CI"
BODY="$(curl -sS "http://127.0.0.1:$PORT/?name=CI")"
echo "$BODY"
if [[ "$BODY" == *'"message":"hello, CI, from the host"'* && "$BODY" == *"\"os\":\"$EXPECT_OS\""* ]]; then
  echo "PASS host function answered on $EXPECT_OS"
else
  echo "FAIL host function: unexpected body"
  fail=1
fi

echo "== GET /spin"
START=$(date +%s%N 2>/dev/null || echo 0)
SPIN="$(curl -sS -m 30 -w ' status=%{http_code} time=%{time_total}' "http://127.0.0.1:$PORT/spin")"
echo "$SPIN"
if [[ "$SPIN" == *"CpuTimeLimit"* && "$SPIN" == *"status=503"* ]]; then
  echo "PASS busy loop stopped by the CPU limit"
else
  echo "FAIL busy loop not stopped by the CPU limit"
  fail=1
fi

echo "== GET / after /spin (the worker must still serve)"
AGAIN="$(curl -sS "http://127.0.0.1:$PORT/?name=again")"
echo "$AGAIN"
if [[ "$AGAIN" == *'"message":"hello, again, from the host"'* ]]; then
  echo "PASS worker still serves"
else
  echo "FAIL worker broken after /spin"
  fail=1
fi

echo "== host log"
cat "$LOG"
exit $fail
