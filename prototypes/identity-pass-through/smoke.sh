#!/usr/bin/env bash
# Smoke test over HTTP: the User comes from the x-vm-user header (read and
# stripped by the host), the Application from the command line. Runs under
# bash on Linux, macOS and Windows (Git Bash).
#
#   smoke.sh <path-to-idp-host> [port]
set -u

HOST_BIN="$1"
PORT="${2:-8797}"
HERE="$(cd "$(dirname "$0")" && pwd)"
LOG="${SMOKE_LOG:-smoke-host.log}"
URL="http://127.0.0.1:$PORT"

"$HOST_BIN" serve "$HERE/guest/attacks.js" "127.0.0.1:$PORT" crm >"$LOG" 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null; wait $PID 2>/dev/null' EXIT

for _ in $(seq 1 100); do
  curl -s -o /dev/null "$URL/" && break
  sleep 0.1
done

fail=0
check() { # name, haystack, needle...
  local name="$1" hay="$2"; shift 2
  for needle in "$@"; do
    if [[ "$hay" != *"$needle"* ]]; then
      echo "FAIL $name: no $needle in: $hay"
      fail=1
      return
    fi
  done
  echo "PASS $name"
}

check "no user header is refused" "$(curl -s -o /dev/null -w '%{http_code}' "$URL/whoami")" 401
check "two user headers are refused" \
  "$(curl -s -o /dev/null -w '%{http_code}' -H 'x-vm-user: alice' -H 'X-VM-User: bob' "$URL/whoami")" 400

check "whoami answers the header's User and the configured Application" \
  "$(curl -s -H 'x-vm-user: alice' "$URL/whoami")" '"user":"alice"' '"application":"crm"'

HEADERS="$(curl -s -H 'x-vm-user: alice' -H 'x-other: kept' "$URL/headers")"
check "the guest sees other headers" "$HEADERS" '"x-other":"kept"'
if [[ "$HEADERS" == *"x-vm-user"* ]]; then
  echo "FAIL the guest saw the identity header: $HEADERS"
  fail=1
else
  echo "PASS the identity header is stripped"
fi

check "the guest cannot claim another User or Application" \
  "$(curl -s -H 'x-vm-user: alice' "$URL/claim-user")" '"args":{"ok":{"application":"crm","request_id"' '"user":"alice"'

# Two Users at once: each awaits a host op and a timer, then asks again.
curl -s -H 'x-vm-user: alice' "$URL/interleave?tag=alice" >interleave-alice.json &
A=$!
curl -s -H 'x-vm-user: bob' "$URL/interleave?tag=bob" >interleave-bob.json &
B=$!
wait $A $B
ALICE="$(cat interleave-alice.json)"
BOB="$(cat interleave-bob.json)"
echo "alice: $ALICE"
echo "bob:   $BOB"
for who in alice bob; do
  body="$(cat "interleave-$who.json")"
  other=$([[ $who == alice ]] && echo bob || echo alice)
  if [[ "$body" == *"\"user\":\"$who\""* && "$body" != *"\"user\":\"$other\""* ]]; then
    echo "PASS interleaved request of $who attached to $who only"
  else
    echo "FAIL interleaved request of $who: $body"
    fail=1
  fi
done

echo "== host log"
cat "$LOG"
exit $fail
