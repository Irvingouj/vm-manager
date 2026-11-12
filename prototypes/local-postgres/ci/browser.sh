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
# macOS has no timeout(1).
with_timeout() {
  local secs=$1; shift
  "$@" & local pid=$!
  ( sleep "$secs"; kill "$pid" 2>/dev/null ) & local w=$!
  wait "$pid"; local rc=$?
  kill "$w" 2>/dev/null
  return $rc
}
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
  case "$b" in
    *.exe) ver=$(powershell -NoProfile -Command "(Get-Item -LiteralPath '$(cygpath -w "$b")').VersionInfo.ProductVersion" | tr -d '\r');;
    *) ver=$("$b" --version 2>/dev/null | head -1);;
  esac
  # Output to files, not a pipe: on Windows the browser's child processes
  # inherit a pipe and keep $(...) waiting forever.
  with_timeout 60 "$b" --headless=new --disable-gpu --no-first-run --no-default-browser-check \
        --user-data-dir="$prof" --dump-dom "$url" >"$prof.out" 2>"$prof.err" </dev/null
  out=$(cat "$prof.out")
  case "$b" in *.exe) taskkill //F //T //IM "$(basename "$b")" >/dev/null 2>&1;; esac
  if echo "$out" | grep -q "hello from Host: my-app.localhost:$port"; then
    echo "PASS browser $(basename "$b") [$ver] $url -> $(echo "$out" | grep -o 'hello from[^<]*')"
  else
    echo "FAIL browser $(basename "$b") [$ver] $url -> $(echo "$out" | head -c 300) stderr: $(tail -c 600 "$prof.err")"
  fi
  rm -rf "$prof" "$prof.err" "$prof.out"
done
[ $found -eq 0 ] && echo "SKIP no Chromium-family browser found"
kill $srv 2>/dev/null
rm -f "$log"
exit 0
