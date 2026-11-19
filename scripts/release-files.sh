#!/usr/bin/env bash
# The release's files beside the tool's archives (blackbox/CONTRACT.md,
# "The first release: What a release holds" and "The scripts that make
# it"):
#
#   scripts/release-files.sh --out <folder>
#
# copies deploy/compose.yaml, deploy/compose.own-proxy.yaml,
# deploy/compose.own-postgres.yaml and deploy/.env.example (as env.example:
# a release cannot hold a file whose name begins with a dot) into the
# folder, unchanged, and writes <folder>/SHA256SUMS over every file then in
# it (the archives packed there before included): one line a file,
# `<sha256>  <name>`, in the names' order, as sha256sum writes it, so that
# `sha256sum --check SHA256SUMS` (Linux) and `shasum -a 256 --check
# SHA256SUMS` (macOS) pass there.
set -euo pipefail

usage() {
  echo "usage: scripts/release-files.sh --out <folder>" >&2
  exit 2
}

out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --out)
      [ "$#" -ge 2 ] && [ -n "$2" ] || usage
      out="$2"
      shift 2
      ;;
    *) usage ;;
  esac
done
[ -n "$out" ] || usage
case "$out" in /*) ;; *) out="$(pwd)/$out" ;; esac

cd "$(dirname "$0")/.."
export LC_ALL=C

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

mkdir -p "$out"
cp deploy/compose.yaml "$out/compose.yaml"
cp deploy/compose.own-proxy.yaml "$out/compose.own-proxy.yaml"
cp deploy/compose.own-postgres.yaml "$out/compose.own-postgres.yaml"
cp deploy/.env.example "$out/env.example"

sums="$(mktemp "${TMPDIR:-/tmp}/release-files.XXXXXX")"
trap 'rm -f "$sums"' EXIT
find "$out" -mindepth 1 -maxdepth 1 -type f ! -name SHA256SUMS -exec basename {} \; \
  | sort \
  | while IFS= read -r name; do
      printf '%s  %s\n' "$(sha256_of "$out/$name")" "$name"
    done >"$sums"
mv -f "$sums" "$out/SHA256SUMS"
chmod 644 "$out/SHA256SUMS"
echo "release-files: wrote $(wc -l <"$out/SHA256SUMS" | tr -d ' ') sums to $out/SHA256SUMS"
