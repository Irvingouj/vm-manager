#!/usr/bin/env bash
# Screenshots of pages in a real Firefox, without a window, at several widths,
# light and dark, to LOOK at (read the PNGs) before and after a change.
#
#   shots.sh <out-folder-name> <url> [<url> ...]
#
# Writes <name>-<n>-<width>-<light|dark>.png under ~/snap/firefox/common/ui-shots/<out-folder-name>/
# (the snap Firefox can only write under ~/snap/firefox/common/). Widths: 320,
# 768, 1280; set WIDTHS="320 1280" to change, HEIGHT=1400 for the height.
# Pages that need a session or a test authority cannot be opened this way:
# render them to a file or use the black-box harness's browser (blackbox/src/firefox.rs).
set -euo pipefail
[ "$#" -ge 2 ] || { sed -n 2,12p "$0"; exit 2; }
name="$1"; shift
base="$HOME/snap/firefox/common/ui-shots"
out="$base/$name"; mkdir -p "$out"
for scheme in light dark; do
  profile="$base/profile-$scheme"; mkdir -p "$profile"
  # 0 = dark, 1 = light: what the page's prefers-color-scheme sees.
  if [ "$scheme" = dark ]; then v=0; else v=1; fi
  printf 'user_pref("layout.css.prefers-color-scheme.content-override", %s);\nuser_pref("ui.systemUsesDarkTheme", %s);\n' "$v" "$((1 - v))" > "$profile/user.js"
  n=0
  for url in "$@"; do
    n=$((n + 1))
    for width in ${WIDTHS:-320 768 1280}; do
      file="$out/$name-$n-$width-$scheme.png"
      timeout 90 firefox --headless --no-remote --profile "$profile" \
        --screenshot "$file" --window-size="$width,${HEIGHT:-1400}" "$url" >/dev/null 2>&1 || true
      [ -s "$file" ] && echo "$file" || echo "no screenshot for $url at $width ($scheme)" >&2
    done
  done
done
