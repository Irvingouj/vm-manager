#!/usr/bin/env bash
# Part G of docs/research/change-history-out-of-reach.md: what the second
# slice of ticket #57 costs, measured with the real program on Postgres 18.
#
#   docs/research/change-history-out-of-reach/g-cost.sh [BEFORE [AFTER [MIDDLE]]]
#
# BEFORE, AFTER, MIDDLE: the commits to compare (by default 4d6df80, the
# first slice; HEAD; and bc0d37f, the second slice before a migration's
# statement and the question of its transaction's key travelled together;
# MIDDLE may be given as "" to leave it out). Each is checked out in a git
# worktree of its own in a temporary directory and its `vm` built there with
# `cargo build --release -p vm --bin vm`, into one target directory kept
# between runs (target/g-cost, so a second run only builds what changed).
# The statement parser's build needs libclang: when this checkout has no
# `.tools/libclang`, scripts/fetch-libclang.sh fills it first (README.md,
# "Build and test"), and each worktree is given a link to it. Each binary
# is then driven through this checkout's black-box harness (`VM_BIN`),
# beside the harness's own Postgres 18 (downloaded once to the per-user
# cache), by `g_cost.rs`, which is copied into blackbox/tests/ for the run
# and removed after it, as are the worktrees. Run it on a quiet machine and
# not beside a gate: the gate would find g_cost.rs among the tests.
# The last line says how long the whole run took.
#   writes      one INSERT through `execute`, request to answer, 400 times
#   migrations  a Version with a migration of 1 `SELECT 1`, and of 301: the
#               difference over 300 is one more statement as sent
#   functions   (AFTER only) vm.capture_guard(), vm.before_commit(), and
#               capture_state, capture_problems, structure_state and a
#               Version with nothing to apply at 10, 100 and 500 tables
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel)"
BEFORE_COMMIT="${1:-4d6df80}"
AFTER_COMMIT="${2:-HEAD}"
MIDDLE_COMMIT="${3-bc0d37f}"
TEST="$ROOT/blackbox/tests/g_cost.rs"
WORK="$(mktemp -d)"
export CARGO_TARGET_DIR="$ROOT/target/g-cost"
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
echo "# building: before $(git -C "$ROOT" rev-parse --short "$BEFORE_COMMIT"), after $(git -C "$ROOT" rev-parse --short "$AFTER_COMMIT")${MIDDLE_COMMIT:+, middle $(git -C "$ROOT" rev-parse --short "$MIDDLE_COMMIT")}"
BEFORE="$(build before "$BEFORE_COMMIT")"
AFTER="$(build after "$AFTER_COMMIT")"
MIDDLE=""
if [ -n "$MIDDLE_COMMIT" ]; then
  MIDDLE="$(build middle "$MIDDLE_COMMIT")"
fi
echo "# built in ${SECONDS} s"
cp "$HERE/g_cost.rs" "$TEST"
run() { # run LABEL BINARY PARTS
  echo "######## $1: vm at $1, parts $3"
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
echo "# whole run: ${SECONDS} s"
