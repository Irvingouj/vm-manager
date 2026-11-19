#!/usr/bin/env bash
# The gate (AGENTS.md, "How code gets written"). Run from anywhere in the
# repository; exits non-zero on the first step that fails.
set -euo pipefail
cd "$(dirname "$0")/.."

# The browser the browser tests drive (blackbox/CONTRACT.md, "A real browser
# in the gate"), looked for first, before anything is built, so that a gate
# without one ends in seconds. The finding and its words are the harness's
# (find_with in blackbox/src/firefox.rs): the program VIBE_TEST_BROWSER
# names (an absolute path, or a name on the PATH; empty counts as unset; a
# relative path is refused, the tests and the gate run in different folders), else
# `firefox` on the PATH; asked `--version` within 30 seconds; it must say
# `Mozilla Firefox <major>...` with a major of 128 or later.
echo "== a browser for the browser tests"
no_browser="No browser for the browser tests"
what_to_do="Install Firefox 128 or later, or set VIBE_TEST_BROWSER to the Firefox to use (an absolute path, or a name on the PATH)."
not_absolute="which is a relative path; a path in VIBE_TEST_BROWSER must be absolute"
min_firefox=128

# An executable file at the path $1.
is_program() {
  [ -f "$1" ] && [ -x "$1" ]
}

# $1 quoted as the harness's messages quote it (Rust's {:?} of a string).
quoted() {
  local text="$1"
  text="${text//\\/\\\\}"
  text="${text//\"/\\\"}"
  text="${text//$'\n'/\\n}"
  text="${text//$'\r'/\\r}"
  text="${text//$'\t'/\\t}"
  printf '"%s"' "$text"
}

# $1 without the white space it begins and ends with.
trimmed() {
  local text="$1"
  text="${text#"${text%%[![:space:]]*}"}"
  text="${text%"${text##*[![:space:]]}"}"
  printf '%s' "$text"
}

browser_named="${VIBE_TEST_BROWSER:-}"
browser=""
if [ -n "$browser_named" ] && [[ "$browser_named" == */* ]] && [[ "$browser_named" != /* ]]; then
  echo "$no_browser: looked for VIBE_TEST_BROWSER=$browser_named, $not_absolute. $what_to_do" >&2
  exit 1
fi
if [ -n "$browser_named" ]; then
  looked_for="VIBE_TEST_BROWSER=$browser_named"
  wanted="$browser_named"
else
  looked_for="\`firefox\` on the PATH (VIBE_TEST_BROWSER is not set)"
  wanted=firefox
fi
if [[ "$wanted" == */* ]]; then
  if is_program "$wanted"; then
    browser="$wanted"
  fi
else
  IFS=: read -r -a search_dirs <<<"${PATH:-}"
  for dir in "${search_dirs[@]}"; do
    if is_program "${dir:-.}/$wanted"; then
      browser="${dir:-.}/$wanted"
      break
    fi
  done
fi
if [ -z "$browser" ]; then
  echo "$no_browser: looked for $looked_for, and found no such program. $what_to_do" >&2
  exit 1
fi

browser_out=$(mktemp)
browser_err=$(mktemp)
trap 'rm -f "$browser_out" "$browser_err"' EXIT
browser_code=0
timeout -k 5 30 "$browser" --version >"$browser_out" 2>"$browser_err" </dev/null || browser_code=$?
browser_said=$(trimmed "$(cat "$browser_out")")
browser_complained=$(trimmed "$(cat "$browser_err")")
rm -f "$browser_out" "$browser_err"
trap - EXIT
case "$browser_code" in
  124 | 137)
    echo "$no_browser: looked for $looked_for, found $browser, which did not answer \`--version\` within 30s. $what_to_do" >&2
    exit 1
    ;;
  125 | 126 | 127)
    echo "$no_browser: looked for $looked_for, found $browser, which could not be asked its version ($(trimmed "${browser_complained%%$'\n'*}")). $what_to_do" >&2
    exit 1
    ;;
esac

# The major version of `Mozilla Firefox <major>.<...>` on the first line
# that is not blank; empty for anything else.
browser_major=""
while IFS= read -r line; do
  line=$(trimmed "$line")
  if [ -z "$line" ]; then
    continue
  fi
  if [[ "$line" =~ ^Mozilla\ Firefox\ ([0-9]+) ]]; then
    browser_major="${BASH_REMATCH[1]}"
  fi
  break
done <<<"$browser_said"
# Without its leading zeros; a major too big for the harness's u32 is no
# Firefox's either.
if [ -n "$browser_major" ]; then
  browser_major="${browser_major#"${browser_major%%[!0]*}"}"
  browser_major="${browser_major:-0}"
fi
if [ -n "$browser_major" ] && { [ "${#browser_major}" -gt 10 ] || [ "$((10#$browser_major))" -gt 4294967295 ]; }; then
  browser_major=""
fi
if [ -z "$browser_major" ]; then
  first="$browser_said$browser_complained"
  first="${first%%$'\n'*}"
  first="${first:0:200}"
  echo "$no_browser: looked for $looked_for, found $browser, which is not Firefox: \`--version\` said $(quoted "$first"). $what_to_do" >&2
  exit 1
fi
if [ "$((10#$browser_major))" -lt "$min_firefox" ]; then
  echo "$no_browser: looked for $looked_for, found $browser, which is $(quoted "$browser_said"), older than Firefox $min_firefox. $what_to_do" >&2
  exit 1
fi
echo "browser: $browser, $(head -n 1 <<<"$browser_said") (looked for $looked_for)"

echo "== blackbox depends on no product crate"
products=$(cargo tree --workspace --locked --depth 0 --prefix none | awk 'NF {print $1}' | grep -vx blackbox || true)
deps=$(cargo tree -p blackbox --locked -e normal,dev,build --prefix none | awk 'NF {print $1}' | sort -u)
for p in $products; do
  if grep -qx "$p" <<<"$deps"; then
    echo "blackbox depends on the product crate $p" >&2
    exit 1
  fi
done

echo "== cargo fmt"
cargo fmt --all --check
echo "== cargo clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings
# The SDK, its type tests and the fixture projects are one npm workspace at
# the root. With --no-fail-fast a failure here does not hide what cargo test
# has to say; the gate still fails.
npm_failed=0
if [ -f package.json ]; then
  echo "== npm ci, npm test"
  if [ ! -d node_modules ] || [ package-lock.json -nt node_modules/.package-lock.json ]; then
    npm ci --no-audit --no-fund || npm_failed=1
  fi
  if [ "$npm_failed" = 0 ]; then
    npm test || npm_failed=1
  fi
  if [ "$npm_failed" = 1 ] && [[ " $* " != *" --no-fail-fast "* ]]; then
    exit 1
  fi
fi

echo "== cargo test"
cargo_failed=0
cargo test --workspace --locked "$@" || cargo_failed=1

# The closing lines: which steps failed, when the gate went on past one
# (--no-fail-fast). scripts/gate-hosted.sh reads them.
if [ "$npm_failed" = 1 ]; then
  echo "== gate: npm test failed"
fi
if [ "$cargo_failed" = 1 ]; then
  echo "== gate: cargo test failed"
fi
if [ "$npm_failed" = 1 ] || [ "$cargo_failed" = 1 ]; then
  exit 1
fi
