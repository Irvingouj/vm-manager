#!/usr/bin/env bash
# Serve on 127.0.0.1:<port> and ask each installed Chromium-family browser,
# headless, for http://my-app.localhost:<port>/. Prints PASS/FAIL per browser.
# usage: ci/browser.sh <lp-binary>
set -uo pipefail
BIN="$1"
log="$(mktemp)"
LP_SERVE_SECS=90 "$BIN" localhost > "$log" 2>&1 &
srv=$!
for _ in $(seq 1 50); do grep -q LOCALHOST_PORT "$log" && break; sleep 0.1; done
port=$(sed -n 's/^LOCALHOST_PORT //p' "$log" | tr -d '\r')
url="http://my-app.localhost:$port/"
found=0
for b in google-chrome chromium microsoft-edge \
         "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
         "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" \
         "/c/Program Files/Google/Chrome/Application/chrome.exe" \
         "/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe" \
         "/c/Program Files/Microsoft/Edge/Application/msedge.exe"; do
  command -v "$b" >/dev/null 2>&1 || [ -x "$b" ] || continue
  found=1
  prof="$(mktemp -d)"
  ver=$("$b" --version 2>/dev/null | head -1)
  out=$(timeout 60 "$b" --headless=new --disable-gpu --no-first-run --no-default-browser-check \
        --user-data-dir="$prof" --dump-dom "$url" 2>/dev/null)
  if echo "$out" | grep -q "hello from Host: my-app.localhost:$port"; then
    echo "PASS browser $(basename "$b") [$ver] $url -> $(echo "$out" | grep -o 'hello from[^<]*')"
  else
    echo "FAIL browser $(basename "$b") [$ver] $url -> $(echo "$out" | head -c 300)"
  fi
  rm -rf "$prof"
done
[ $found -eq 0 ] && echo "SKIP no Chromium-family browser found"
kill $srv 2>/dev/null
rm -f "$log"
exit 0
