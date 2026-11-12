#!/usr/bin/env bash
# Which directory names Postgres on Windows accepts: one cold session per case.
# usage: ci/windows-paths.sh <lp.exe>
set -uo pipefail
BIN="$1"
t="$RUNNER_TEMP"
try() {
  local label="$1" root="$2" inst="${3:-}"
  echo "--- $label: root=[$root] install_root=[${inst:-<root>/pg}]"
  if [ -n "$inst" ]; then
    LP_INSTALL_ROOT="$inst" "$BIN" pg "$root" > case.log 2>&1
  else
    "$BIN" pg "$root" > case.log 2>&1
  fi
  local rc=$?
  grep -E "^(FAIL|Error|Caused|    Command error|METRIC (init_time|first_start_time))" case.log | head -6
  [ $rc -eq 0 ] && echo "CASE PASS: $label" || echo "CASE FAIL: $label"
  rm -rf "$root" case.log
}
echo "active code page: $(cmd //c chcp)"
try "ASCII with space"               "$t/lp spike Zoe"
try "Latin-1 (cp1252) e-diaeresis"   "$t/lp spike Zoë"
try "CJK"                            "$t/lp spike 数据"
try "CJK data dir, ASCII binaries"   "$t/lp spike 数据 data" "$t/lp-bin ascii"
rm -rf "$t/lp-bin ascii"
exit 0
