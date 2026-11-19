#!/usr/bin/env bash
# Packs the tool for one target (blackbox/CONTRACT.md, "The first release:
# What a release holds" and "The scripts that make it"):
#
#   scripts/package-tool.sh --binary <file> --target <triple> --out <folder>
#
# makes <folder>/vibe-<v>-<target>.tar.gz, a gzip'd tar of one folder
# vibe-<v>-<target>/ holding exactly `vibe` (the given program,
# executable), THIRD-PARTY-LICENSES (scripts/third-party-licenses.sh
# --target <triple>) and LICENSE when the repository has one; <v> is what
# the program's `--version` says on its first line (`vibe <v>`), which must
# be the package vibe's version (crates/vibe/Cargo.toml). Prints the
# archive's path, last. Refuses, saying why and leaving nothing in the
# folder, a file that is not there, one whose `--version` does not say
# `vibe <version>` first, and a version other than the package's.
set -euo pipefail

refuse() {
  echo "package-tool: $*" >&2
  exit 1
}

usage() {
  echo "usage: scripts/package-tool.sh --binary <file> --target <triple> --out <folder>" >&2
  exit 2
}

binary=""
target=""
out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --binary | --target | --out)
      [ "$#" -ge 2 ] && [ -n "$2" ] || usage
      case "$1" in
        --binary) binary="$2" ;;
        --target) target="$2" ;;
        --out) out="$2" ;;
      esac
      shift 2
      ;;
    *) usage ;;
  esac
done
[ -n "$binary" ] && [ -n "$target" ] && [ -n "$out" ] || usage

case "$target" in
  *[!A-Za-z0-9_.-]* | .* ) refuse "$target is not a target triple (such as x86_64-unknown-linux-gnu)" ;;
esac

# The paths as the caller gave them, made absolute before leaving its folder.
case "$binary" in /*) ;; *) binary="$(pwd)/$binary" ;; esac
case "$out" in /*) ;; *) out="$(pwd)/$out" ;; esac

cd "$(dirname "$0")/.."
root="$(pwd -P)"

if [ ! -f "$binary" ]; then
  refuse "$binary is not there (or is not a file): nothing was packed"
fi

# The first line of `--version`, read without a pipe that the program could
# see closed early.
if ! said="$("$binary" --version 2>/dev/null)"; then
  refuse "$binary --version failed: it is not the program vibe; nothing was packed"
fi
first="${said%%$'\n'*}"
case "$first" in
  "vibe "*) version="${first#vibe }" ;;
  *) refuse "$binary --version says \"$first\" first, not \"vibe <version>\": nothing was packed" ;;
esac
case "$version" in
  "" | *[!A-Za-z0-9.+-]*) refuse "$binary --version says \"$first\" first, not \"vibe <version>\": nothing was packed" ;;
esac

package="$(awk '/^\[/ { p = ($0 == "[package]") } p && /^version *=/ { sub(/^version *= *"/, ""); sub(/".*/, ""); print; exit }' crates/vibe/Cargo.toml)"
if [ "$version" != "$package" ]; then
  refuse "$binary says it is vibe $version, but the package vibe's version (crates/vibe/Cargo.toml) is $package: nothing was packed"
fi

name="vibe-$version-$target"
work="$(mktemp -d "${TMPDIR:-/tmp}/package-tool.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir "$work/$name"
cp "$binary" "$work/$name/vibe"
chmod 755 "$work/$name/vibe"
"$root/scripts/third-party-licenses.sh" --target "$target" >"$work/$name/THIRD-PARTY-LICENSES"
if [ -f "$root/LICENSE" ]; then
  cp "$root/LICENSE" "$work/$name/LICENSE"
fi
chmod 644 "$work/$name/THIRD-PARTY-LICENSES"
[ ! -f "$work/$name/LICENSE" ] || chmod 644 "$work/$name/LICENSE"
chmod 755 "$work/$name"

# COPYFILE_DISABLE: macOS's tar adds no ._ files of extended attributes.
COPYFILE_DISABLE=1 tar -czf "$work/$name.tar.gz" -C "$work" "$name"

mkdir -p "$out"
mv -f "$work/$name.tar.gz" "$out/$name.tar.gz"
echo "$(cd "$out" && pwd -P)/$name.tar.gz"
