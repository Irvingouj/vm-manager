#!/usr/bin/env bash
# The gate as a hosted runner runs it (blackbox/CONTRACT.md, "The first
# release: The scripts that make it").
#
# The gate's time limits are sized for the machines it runs on every day
# (12 cores). A hosted runner has 4, and on it a handful of test files that
# start many programs at once run past those limits when every file's tests
# run side by side. So this runs the whole gate once
# (scripts/gate.sh --no-fail-fast), then each test file that failed once
# more, alone and one test at a time, as cargo names it in its own lines
# ("to rerun pass `...`" and "N targets failed:").
#
# It ends with 0 only when the gate's steps before the tests passed, npm's
# tests passed, and every test file passed the first time or alone.
# Nothing is skipped: a file that fails twice fails it, and so does a gate
# that failed with no test file named.
#
# Usage: scripts/gate-hosted.sh   (from any folder)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

say() {
  echo "== gate-hosted: $*"
}

log="$(mktemp "${TMPDIR:-/tmp}/gate-hosted.XXXXXX")"
trap 'rm -f "$log"' EXIT

# The whole gate, once, its output in the job's log as it goes and kept
# to read cargo's lines from.
set +e
scripts/gate.sh --no-fail-fast 2>&1 | tee "$log"
gate_status="${PIPESTATUS[0]}"
set -e

if [ "$gate_status" = 0 ]; then
  say "passed"
  exit 0
fi

npm_failed=0
cargo_failed=0
grep -qx '== gate: npm test failed' "$log" && npm_failed=1
grep -qx '== gate: cargo test failed' "$log" && cargo_failed=1

if [ "$npm_failed" = 0 ] && [ "$cargo_failed" = 0 ]; then
  # gate.sh names the steps that failed at its end; naming none, it
  # stopped before the tests (the blackbox crate's dependencies, fmt or
  # clippy), and no test was run.
  say "the gate failed before its tests (exit status $gate_status); nothing is run again"
  say "failed"
  exit 1
fi

# The test files cargo named, each once, in the order it named them: every
# line "error: test failed, to rerun pass `X`" or "error: doctest failed,
# to rerun pass `X`", and every name in backticks under
# "error: N target(s) failed:".
targets="$(
  tr -d '\r' < "$log" | awk '
    /to rerun pass `.*`/ {
      line = $0
      sub(/^.*to rerun pass `/, "", line)
      sub(/`[^`]*$/, "", line)
      print line
      listing = 0
      next
    }
    /^error: [0-9]+ targets? failed:$/ { listing = 1; next }
    listing && /^[ \t]+`.*`[ \t]*$/ {
      line = $0
      sub(/^[ \t]+`/, "", line)
      sub(/`[ \t]*$/, "", line)
      print line
      next
    }
    { listing = 0 }
  ' | awk 'NF && !seen[$0]++'
)"

if [ "$cargo_failed" = 1 ] && [ -z "$targets" ]; then
  say "cargo test failed naming no test file; nothing is run again"
  say "failed"
  exit 1
fi

# Each file again, alone and one test at a time. Its output goes to the
# job's log as it is.
results=()
twice=0
while IFS= read -r target; do
  [ -n "$target" ] || continue
  read -r -a words <<< "$target"
  say "running again alone: $target"
  if cargo test --locked "${words[@]}" -- --test-threads=1 < /dev/null; then
    results+=("$target, run again alone: passed")
  else
    results+=("$target, run again alone: FAILED again")
    twice=1
  fi
done <<< "$targets"

echo
if [ "${#results[@]}" -gt 0 ]; then
  say "test files run again alone, one test at a time:"
  for result in "${results[@]}"; do
    say "  $result"
  done
fi
if [ "$npm_failed" = 1 ]; then
  say "npm's tests failed (they are not run again)"
fi

if [ "$npm_failed" = 1 ] || [ "$twice" = 1 ]; then
  say "failed"
  exit 1
fi
say "passed, but only with the test files above run again alone"
