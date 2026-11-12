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
  if [ $rc -eq 0 ]; then
    grep -E "^METRIC (init_time|first_start_time)" case.log
    echo "CASE PASS: $label"
  else
    sed -n '/^Error/,$p' case.log | head -40
    echo "CASE FAIL: $label"
  fi
  rm -rf "$root" case.log
}
echo "console code page: $(cmd //c chcp)"
echo "ANSI code page (GetACP): $(powershell -NoProfile -Command '[System.Text.Encoding]::Default.CodePage')"
try "ASCII with space"               "$t/lp spike Zoe"
try "Latin-1 (cp1252) e-diaeresis"   "$t/lp spike Zoë"
try "CJK"                            "$t/lp spike 数据"
try "CJK data dir, ASCII binaries"   "$t/lp spike 数据 data" "$t/lp-bin ascii"
rm -rf "$t/lp-bin ascii"
# Workaround candidate: the 8.3 short name of a non-ASCII directory, if the
# volume generates them (often off on non-system volumes).
for base in "$t" "$LOCALAPPDATA"; do
  long="$base/lp spike 数据 short"
  mkdir -p "$long"
  w=$(cygpath -w "$long")
  short=$(powershell -NoProfile -Command "(New-Object -ComObject Scripting.FileSystemObject).GetFolder('$w').ShortPath" | tr -d '\r')
  echo "8.3 short path of [$w]: [$short]"
  try "CJK via 8.3 short path under $base" "$short"
  rm -rf "$long"
done
exit 0
