#!/usr/bin/env bash
# Part J of docs/research/change-history-out-of-reach.md: what ticket #63
# (a rewrite records each row it changed) costs, measured with the real
# program on Postgres 18.
#
#   docs/research/change-history-out-of-reach/j-cost.sh [BEFORE [AFTER [MIDDLE]]]
#
# BEFORE, AFTER: the commits to compare (by default 9643cb1, the last
# commit before the ticket's program, and HEAD); MIDDLE (by default
# e9a6ce8, the ticket's first round; "" leaves it out) is measured in
# `statements` and `rewrite` too. Each is checked out in a
# git worktree of its own in a temporary directory and its `vm` built there
# with `cargo build --release -p vm --bin vm`, into one target directory
# kept between runs (target/j-cost); `.tools/libclang` is filled by
# scripts/fetch-libclang.sh where it is missing. Each binary is then driven
# through this checkout's black-box harness (`VM_BIN`), beside the
# harness's own Postgres 18, by `j_cost.rs`, which is copied into
# blackbox/tests/ for the run and removed after it, as are the worktrees.
# J_STAGES (default "1 2 breakdown big") runs only some of what follows,
# so that the run can be made in parts. Run it on a quiet machine and not
# beside a gate. J_ROWS (default 100000)
# is the rewritten table's size; at the end, `rewrite` runs once more on
# each binary at 1,000,000 rows, one Version of each kind.
#   statements  a Version that rewrites nothing: one statement's cost and
#               one migration's, at 10 and 500 tables
#   rewrite     a Version retyping a column of a table of J_ROWS rows:
#               every row changed, none changed, every row changed with the
#               statement in a DO block; and a column's `varchar(50)`
#               changed to `text`, which Postgres makes without a rewrite
#   breakdown   (AFTER only) the same three once more, with Postgres's
#               function statistics: where the time went, and how many runs
#   partitions  (not in the default stages) a Version whose migration makes
#               100, then 200 partitions, one a statement, under a
#               partitioned table with history that had none, then one that
#               had 1,000, each beside a Version of as many `SELECT 1`; and
#               one `SELECT 1`'s cost beside the table of 1,000
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel)"
BEFORE_COMMIT="${1:-9643cb1}"
AFTER_COMMIT="${2:-HEAD}"
MIDDLE_COMMIT="${3-e9a6ce8}"
TEST="$ROOT/blackbox/tests/j_cost.rs"
WORK="$(mktemp -d)"
export CARGO_TARGET_DIR="$ROOT/target/j-cost"
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
cp "$HERE/j_cost.rs" "$TEST"
run() { # run LABEL BINARY PARTS [ROWS ROUNDS]
  local rows="${4:-${J_ROWS:-100000}}" rounds="${5:-3}"
  echo "######## $1: vm at $1, parts $3, J_ROWS $rows, J_ROUNDS $rounds"
  (cd "$ROOT" && VM_BIN="$2" J_PARTS="$3" J_ROWS="$rows" J_ROUNDS="$rounds" VM_TEST_PG_VERSION=18 \
    cargo test -q -p blackbox --test j_cost -- --nocapture 2>&1) | grep -E '^J |^test result|panicked'
}
echo "# $(date -u +%FT%TZ) on $(uname -srm), $(nproc) CPUs"
for stage in ${J_STAGES:-1 2 breakdown big}; do
  case "$stage" in
    1|2)
      echo "#### round $stage"
      run before "$BEFORE" statements,rewrite
      if [ -n "$MIDDLE" ]; then
        run middle "$MIDDLE" statements,rewrite
      fi
      run after "$AFTER" statements,rewrite
      ;;
    breakdown) run after "$AFTER" breakdown ;;
    partitions)
      run before "$BEFORE" partitions
      if [ -n "$MIDDLE" ]; then
        run middle "$MIDDLE" partitions
      fi
      run after "$AFTER" partitions
      ;;
    big)
      run before "$BEFORE" rewrite 1000000 1
      run after "$AFTER" rewrite 1000000 1
      ;;
  esac
done
echo "# whole run: ${SECONDS} s"
