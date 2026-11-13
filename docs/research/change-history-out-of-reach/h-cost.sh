#!/usr/bin/env bash
# Part H of docs/research/change-history-out-of-reach.md: what ticket #62
# (the Change history in the platform database, shipped from an outbox)
# costs, measured with the real program on Postgres 18.
#
#   docs/research/change-history-out-of-reach/h-cost.sh [BEFORE [AFTER]]
#
# BEFORE, AFTER: the commits to compare (by default 9255bea, the last
# commit before the ticket's program, and HEAD). Each is checked out in a
# git worktree of its own in a temporary directory and its `vm` built there
# with `cargo build --release -p vm --bin vm`, into one target directory
# kept between runs (target/h-cost). The statement parser's build needs
# libclang: when this checkout has no `.tools/libclang`,
# scripts/fetch-libclang.sh fills it first. Each binary is then driven
# through this checkout's black-box harness (`VM_BIN`), beside the
# harness's own Postgres 18, by `h_cost.rs`, which is copied into
# blackbox/tests/ for the run and removed after it, as are the worktrees.
# Run it on a quiet machine and not beside a gate.
#   writes      one INSERT through `execute`, request to answer, 400 times
#   migrations  a Version with one migration (`SELECT 1`, or a column
#               retyped) at 10, 100 and 500 tables
#   snapshot    (AFTER only) vm.structure_snapshot() at 10, 100, 500 tables
#   after       (AFTER only) a write sent at once after a write of 10,000 rows
#   ship        (AFTER only) one execute of 100,000 rows, shipped
#   reads       (AFTER only) `history` reads of 100 changes with 1,000 and
#               1,000,000 changes kept, with and without a key; the plan
#   start       (AFTER only) a start with 50 own databases, each with an
#               outbox of 1,000 changes
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel)"
BEFORE_COMMIT="${1:-9255bea}"
AFTER_COMMIT="${2:-HEAD}"
TEST="$ROOT/blackbox/tests/h_cost.rs"
WORK="$(mktemp -d)"
export CARGO_TARGET_DIR="$ROOT/target/h-cost"
cleanup() {
  rm -f "$TEST"
  for tree in "$WORK"/src-*; do
    if [ -d "$tree" ]; then git -C "$ROOT" worktree remove --force "$tree"; fi
  done
  rm -rf "$WORK"
}
trap cleanup EXIT INT TERM
if [ ! -e "$ROOT/.tools/libclang/libclang.so" ]; then
  "$ROOT/scripts/fetch-libclang.sh"
fi
build() { # build LABEL COMMIT: echoes the binary's path
  local tree="$WORK/src-$1"
  git -C "$ROOT" worktree add --quiet --detach "$tree" "$2" >&2
  ln -s "$ROOT/.tools" "$tree/.tools"
  (cd "$tree" && cargo build -q --release -p vm --bin vm >&2)
  cp "$CARGO_TARGET_DIR/release/vm" "$WORK/vm-$1"
  echo "$WORK/vm-$1"
}
echo "# building: before $(git -C "$ROOT" rev-parse --short "$BEFORE_COMMIT"), after $(git -C "$ROOT" rev-parse --short "$AFTER_COMMIT")"
BEFORE="$(build before "$BEFORE_COMMIT")"
AFTER="$(build after "$AFTER_COMMIT")"
echo "# built in ${SECONDS} s"
cp "$HERE/h_cost.rs" "$TEST"
run() { # run LABEL BINARY PARTS
  echo "######## $1: vm at $1, parts $3"
  (cd "$ROOT" && VM_BIN="$2" H_PARTS="$3" VM_TEST_PG_VERSION=18 \
    cargo test -q -p blackbox --test h_cost -- --nocapture 2>&1) | grep -E '^H |^test result|panicked'
}
echo "# $(date -u +%FT%TZ) on $(uname -srm), $(nproc) CPUs"
for round in 1 2; do
  echo "#### round $round"
  run before "$BEFORE" writes,migrations
  run after "$AFTER" writes,migrations,snapshot
done
run after "$AFTER" after
run after "$AFTER" ship
run after "$AFTER" reads
run after "$AFTER" start
echo "# whole run: ${SECONDS} s"
