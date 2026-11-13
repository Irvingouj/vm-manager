#!/usr/bin/env bash
# Part G of docs/research/change-history-out-of-reach.md: what the second
# slice of ticket #57 costs, measured with the real program on Postgres 18.
#
#   docs/research/change-history-out-of-reach/g-cost.sh BEFORE AFTER [MIDDLE]
#
# BEFORE, AFTER, MIDDLE: `vm` binaries, built with `cargo build --release -p
# vm --bin vm` at the commits to compare (for the run in the log: BEFORE at
# 4d6df80, the first slice; MIDDLE at bc0d37f, the second slice before a
# migration's statement and the question of its transaction's key travelled
# together; AFTER at f4d97d9). Each is driven through the black-box
# harness (`VM_BIN`), beside the harness's own Postgres 18, by `g_cost.rs`,
# which is copied into blackbox/tests/ for the run and removed after it.
#   writes      one INSERT through `execute`, request to answer, 400 times
#   migrations  a Version with a migration of 1 `SELECT 1`, and of 301: the
#               difference over 300 is one more statement as sent
#   functions   (AFTER only) vm.capture_guard(), vm.before_commit(), and
#               capture_state, capture_problems, structure_state and a
#               Version with nothing to apply at 10, 100 and 500 tables
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel)"
BEFORE="$1"
AFTER="$2"
MIDDLE="${3:-}"
TEST="$ROOT/blackbox/tests/g_cost.rs"
cp "$HERE/g_cost.rs" "$TEST"
trap 'rm -f "$TEST"' EXIT INT TERM
run() { # run LABEL BINARY PARTS
  echo "######## $1: $(basename "$2"), parts $3"
  (cd "$ROOT" && VM_BIN="$2" G_PARTS="$3" VM_TEST_PG_VERSION=18 \
    cargo test -q -p blackbox --test g_cost -- --nocapture 2>&1) | grep -E '^G |^test result|panicked'
}
echo "# $(date -u +%FT%TZ) on $(uname -srm), $(nproc) CPUs"
for round in 1 2; do
  echo "#### round $round"
  run before "$BEFORE" writes,migrations
  if [ -n "$MIDDLE" ]; then
    run middle "$MIDDLE" writes,migrations
  fi
  run after "$AFTER" writes,migrations
done
run after "$AFTER" functions
