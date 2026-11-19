#!/usr/bin/env bash
# What must hold before a release is built (blackbox/CONTRACT.md, "The
# first release: One version", "The licence" and "The scripts that make
# it"):
#
#   scripts/release-check.sh            a rehearsal: the versions, the notes
#   scripts/release-check.sh v<version> a release: also the tag and LICENSE
#
# Checks, and says each thing that does not hold:
#   - the version of the package vibe (crates/vibe/Cargo.toml) is the one
#     Cargo.lock holds, the Dockerfile's default of VIBE_VERSION, the image
#     deploy/compose.yaml takes without VIBE_IMAGE
#     (ghcr.io/irvingouj/vibe:<version>), every such image of deploy/'s
#     Compose files, .env.example and README.md, and every address of a
#     release's file and every archive's name in docs/getting-started.md;
#   - docs/releases/v<version>.md, the release's notes, exists;
#   - with a tag: the tag is v<version>, its commit is on main (origin/main
#     or an ancestor of it), and LICENSE exists. Without a tag a missing
#     LICENSE is said and does not fail the check.
# Exit code 0 only when everything asked holds.
set -euo pipefail

if [ "$#" -gt 1 ]; then
  echo "usage: scripts/release-check.sh [<tag>]" >&2
  exit 2
fi
tag="${1:-}"

cd "$(dirname "$0")/.."
export LC_ALL=C

image="ghcr.io/irvingouj/vibe"
problems=0
problem() {
  echo "release-check: $*" >&2
  problems=$((problems + 1))
}

# The package's version, from its manifest's [package] table.
version="$(awk '/^\[/ { p = ($0 == "[package]") } p && /^version *=/ { sub(/^version *= *"/, ""); sub(/".*/, ""); print; exit }' crates/vibe/Cargo.toml)"
if [ -z "$version" ]; then
  echo "release-check: crates/vibe/Cargo.toml names no version of the package vibe" >&2
  exit 1
fi
echo "release-check: the package vibe's version is $version"

# Cargo.lock: the package vibe of the workspace (no source line).
locked="$(awk '
  /^\[\[package\]\]/ { name = ""; v = "" }
  /^name = / { name = $3; gsub(/"/, "", name) }
  /^version = / { v = $3; gsub(/"/, "", v) }
  /^source = / { name = "" }
  /^$/ { if (name == "vibe" && v != "") { print v; exit } }
  END { if (name == "vibe" && v != "") print v }' Cargo.lock | head -n 1)"
if [ "$locked" != "$version" ]; then
  problem "Cargo.lock holds the package vibe at ${locked:-no version}, not $version (run cargo build to bring it up to date)"
fi

# The Dockerfile's default of VIBE_VERSION.
defaults="$(sed -n 's/^[[:space:]]*ARG VIBE_VERSION=\([^[:space:]]*\).*/\1/p' Dockerfile)"
if [ -z "$defaults" ]; then
  problem "the Dockerfile has no line ARG VIBE_VERSION=$version"
fi
for d in $defaults; do
  if [ "$d" != "$version" ]; then
    problem "the Dockerfile's default of VIBE_VERSION is $d, not $version"
  fi
done

# The image the Compose file takes without VIBE_IMAGE.
if ! grep -qF "\${VIBE_IMAGE:-$image:$version}" deploy/compose.yaml; then
  problem "deploy/compose.yaml does not take $image:$version without VIBE_IMAGE (\${VIBE_IMAGE:-$image:$version})"
fi
# Every version of the image deploy/ names (placeholders such as
# <the newer version> are no version).
for file in deploy/compose.yaml deploy/compose.own-proxy.yaml deploy/compose.own-postgres.yaml \
  deploy/.env.example deploy/README.md; do
  [ -f "$file" ] || { problem "$file is not there"; continue; }
  for named in $(grep -o "$image:[0-9][A-Za-z0-9._+-]*" "$file" || true); do
    if [ "${named#"$image:"}" != "$version" ]; then
      problem "$file names the image $named, not $image:$version"
    fi
  done
done
if ! grep -q "$image:$version" deploy/.env.example; then
  problem "deploy/.env.example does not show the image $image:$version"
fi

# The getting-started page: every release address and archive.
page=docs/getting-started.md
if [ ! -f "$page" ]; then
  problem "$page is not there"
else
  addresses="$(grep -o '/releases/download/[^/[:space:])]*/' "$page" || true)"
  if [ -z "$addresses" ]; then
    problem "$page gives no address of the release's files (.../releases/download/v$version/...)"
  fi
  for address in $addresses; do
    named="${address#/releases/download/}"
    named="${named%/}"
    if [ "$named" != "v$version" ]; then
      problem "$page names the release $named in an address (.../releases/download/$named/), not v$version"
    fi
  done
  for target in x86_64-unknown-linux-gnu aarch64-apple-darwin; do
    archives="$(grep -o "vibe-[A-Za-z0-9.+_-]*-$target\.tar\.gz" "$page" || true)"
    if [ -z "$archives" ]; then
      problem "$page names no archive vibe-$version-$target.tar.gz"
    fi
    for archive in $archives; do
      if [ "$archive" != "vibe-$version-$target.tar.gz" ]; then
        problem "$page names the archive $archive, not vibe-$version-$target.tar.gz"
      fi
    done
  done
fi

# The release's notes.
notes="docs/releases/v$version.md"
if [ ! -f "$notes" ]; then
  problem "the release's notes $notes are not there"
fi

# The program's own licence: the product owner's to state.
if [ ! -f LICENSE ]; then
  if [ -n "$tag" ]; then
    problem "LICENSE is missing: nothing is published without the program's own licence (the product owner's to state)"
  else
    echo "release-check: LICENSE is missing; a rehearsal goes on without it, a release cannot"
  fi
fi

# The tag: v<version>, on main.
if [ -n "$tag" ]; then
  if [ "$tag" != "v$version" ]; then
    problem "the tag $tag is not v$version, the tag of the package's version"
  fi
  if ! commit="$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}")"; then
    problem "there is no tag $tag in this repository"
  elif ! main="$(git rev-parse --verify --quiet refs/remotes/origin/main)"; then
    problem "there is no origin/main to hold the tag $tag's commit to (fetch main first)"
  elif ! git merge-base --is-ancestor "$commit" "$main"; then
    problem "the tag $tag is on $commit, which is not on main (not origin/main nor an ancestor of it)"
  fi
fi

if [ "$problems" -gt 0 ]; then
  echo "release-check: $problems thing(s) do not hold${tag:+ for $tag}" >&2
  exit 1
fi
echo "release-check: everything asked holds for vibe $version${tag:+ (the tag $tag)}"
