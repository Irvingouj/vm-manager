#!/usr/bin/env bash
# The notices of what the program `vibe` is built from (blackbox/CONTRACT.md,
# "Installing: one image and a Compose file: The image" and "The first
# release: The scripts that make it"), written to standard output:
#
#   scripts/third-party-licenses.sh [--target <triple>]
#
# for the given Rust target, or without one for this machine's (rustc -vV).
# The image's build and the release's archives of the tool both use it, so
# the notices of the image and of an archive for the same target are the
# same text. Made from Cargo.lock, never kept by hand.
#
# First every crate built into the program for the target (`cargo tree -p
# vibe --edges normal,build`), but the workspace's own, each a block:
#
#   ## <name> <version>
#   License: <the manifest's statement>
#   Source: <where it comes from>
#
# and each licence text its source carries, every line of it indented by
# four spaces (so that no line of a text stands at the margin as a `## `
# line does). Then a part for the code built in that is not a crate's own,
# whose texts are kept in deploy/third-party-licenses/<name>/: each entry
# `## <name> <version> (built in through <crate>)`, `License: <SPDX>`,
# `Taken from: <where>`, and its texts, checked against the SHA-256 its
# SOURCE names.
#
# Needs cargo and rustc (the toolchain of rust-toolchain.toml), perl (its
# JSON::PP, a core module, reads `cargo metadata`), and the usual tools of
# Linux or macOS. Downloads the sources of crates of the target that are not
# on the machine yet (cargo metadata does).
set -euo pipefail

usage() {
  echo "usage: scripts/third-party-licenses.sh [--target <triple>]" >&2
  exit 2
}

target=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --target)
      [ "$#" -ge 2 ] && [ -n "$2" ] || usage
      target="$2"
      shift 2
      ;;
    *) usage ;;
  esac
done

cd "$(dirname "$0")/.."
root="$(pwd -P)"
# The same order and the same bytes whatever the machine's locale.
export LC_ALL=C

host="$(rustc -vV | sed -n 's/^host: //p')"
if [ -z "$target" ]; then
  target="$host"
fi
release="$(rustc -vV | sed -n 's/^release: //p')"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
checkouts=""
if [ -d "$cargo_home/git/checkouts" ]; then
  checkouts="$(realpath "$cargo_home/git/checkouts")"
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/third-party-licenses.XXXXXX")"
trap 'rm -rf "$work"' EXIT
sep=$'\x1f'

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

# The licence texts of a folder: its files named LICENSE*, LICENCE*,
# COPYING*, NOTICE*, UNLICENSE* or COPYRIGHT* (any case; not source files,
# such as copying-phase.cc, nor a debian/ packaging folder's), in the folder
# and the folders under it (for one, liblzma-sys carries xz's in xz/), and
# the manifest's license-file; one absolute path a line. $3 is find's
# -maxdepth (none: every depth).
licence_files() {
  {
    find "$1" ${3:+-maxdepth "$3"} -type f \( -iname 'licen[cs]e*' -o -iname 'copying*' \
      -o -iname 'notice*' -o -iname 'unlicen[cs]e*' -o -iname 'copyright*' \) \
      -not \( -iname '*.c' -o -iname '*.cc' -o -iname '*.cpp' -o -iname '*.h' -o -iname '*.hh' \
      -o -iname '*.inc' -o -iname '*.rs' -o -iname '*.sh' -o -iname '*.py' -o -iname '*.js' \
      -o -iname '*.ts' \) -not -path '*/debian/*'
    if [ -n "$2" ] && [ -f "$1/$2" ]; then echo "$1/$2"; fi
  } | while read -r f; do realpath "$f"; done | sort -u
}

# The workspace's own crates; every package of the target and of this
# machine (a build script's dependencies are built for this machine, and
# cargo tree lists them), one a line:
# name, version, licence, licence file, folder, source; the crates built
# into the program, `<name> <version>` a line.
cargo metadata --no-deps --format-version 1 --locked \
  | perl -MJSON::PP -0777 -ne 'print "$_->{name}\n" for @{decode_json($_)->{packages}}' \
  | sort -u >"$work/own"
cargo metadata --format-version 1 --locked --filter-platform "$target" --filter-platform "$host" \
  | perl -MJSON::PP -0777 -ne '
      for my $p (@{decode_json($_)->{packages}}) {
        (my $dir = $p->{manifest_path}) =~ s{/Cargo\.toml$}{};
        print join("\x1f", $p->{name}, $p->{version}, $p->{license} // "",
          $p->{license_file} // "", $dir, $p->{source} // ""), "\n";
      }' >"$work/packages"
version="$(awk -F "$sep" '$1 == "vibe" && $6 == "" { print $2; exit }' "$work/packages")"
cargo tree -p vibe --edges normal,build --prefix none --locked --target "$target" \
  | awk 'NF && $1 !~ /^\[/ { v = $2; sub(/^v/, "", v); print $1 " " v }' \
  | sort -u >"$work/built"

notices="$work/notices"
{
  echo "Third-party licences of the program vibe ${version}"
  echo
  echo "Every crate built into the program for ${target}, but the program's own,"
  echo "with its version, the licence its manifest states, and the licence texts"
  echo "its source carries. Made from Cargo.lock when the program was built."
  while read -r name version; do
    if grep -qxF "$name" "$work/own"; then continue; fi
    row="$(awk -F "$sep" -v n="$name" -v v="$version" '$1 == n && $2 == v { print; exit }' "$work/packages")"
    if [ -z "$row" ]; then
      echo "cargo metadata knows no package $name $version" >&2
      exit 1
    fi
    IFS="$sep" read -r _ _ license license_file dir source <<<"$row"
    echo
    echo "## $name $version"
    if [ -n "$license" ]; then
      echo "License: $license"
    elif [ -n "$license_file" ]; then
      echo "License: the manifest states no licence expression; it names the file $license_file"
    else
      echo "License: the manifest states none"
    fi
    case "$source" in
      registry+*) echo "Source: crates.io" ;;
      git+*) echo "Source: ${source#git+}" ;;
      "") echo "Source: in this repository (vendored)" ;;
      *) echo "Source: $source" ;;
    esac
    texts="$(licence_files "$dir" "$license_file")"
    from="$dir"
    # A crate of a git checkout or of this repository whose own folder
    # carries no licence text: the text of the folder that holds it (its
    # repository's), up to the checkout's or the repository's root.
    if [ -z "$texts" ] && [ "${source#registry+}" = "$source" ]; then
      up="$(realpath "$dir")"
      while [ -z "$texts" ]; do
        up="$(dirname "$up")"
        if [ "$up" = / ] || [ "$up" = "$root" ] || [ "$(dirname "$up")" = "$checkouts" ]; then
          break
        fi
        texts="$(licence_files "$up" "" 1)"
        from="$up"
      done
    fi
    if [ -z "$texts" ]; then
      echo "Its source carries no licence text."
      continue
    fi
    if [ "$from" != "$dir" ]; then
      echo "Its own folder carries no licence text; the folder that holds it (${from##*/}) carries:"
    fi
    from_real="$(realpath "$from")"
    while read -r file; do
      echo
      echo "  ${file#"$from_real/"}:"
      echo
      sed 's/^/    /' "$file"
    done <<<"$texts"
  done <"$work/built"

  # The second part: code built into the program that is not a crate's own
  # and whose licence texts no crate's source carries (V8 and the libraries
  # linked into its prebuilt library, Postgres's parser, the Rust standard
  # library and its own dependencies). The texts are kept in
  # deploy/third-party-licenses/<name>/, each fetched from its project's own
  # repository at the version built in; its SOURCE says from where, at which
  # tag or commit, and each file's SHA-256, which is checked here. It fails
  # when the crate an entry is built in through is not built in at the
  # version the entry names.
  echo
  echo "Code built into the program that is not a crate's own, whose licence"
  echo "texts no crate's source carries. Taken from each project's own"
  echo "repository at the version built in."
  for source_file in "$root"/deploy/third-party-licenses/*/SOURCE; do
    folder="$(dirname "$source_file")"
    field() { sed -n "s/^$1: //p" "$source_file" | head -n 1; }
    name="$(field Name)"; version="$(field Version)"; license="$(field License)"
    through="$(field 'Built in through')"; taken="$(field 'Taken from')"
    if [ -z "$name" ] || [ -z "$version" ] || [ -z "$license" ] || [ -z "$through" ] || [ -z "$taken" ]; then
      echo "$source_file lacks Name, Version, License, Built in through or Taken from" >&2
      exit 1
    fi
    # The standard library and its own dependencies come with the compiler:
    # those entries name `rustc <release>`, held to the compiler building
    # the program; every other entry names a crate cargo tree has built in.
    if [ "${through%% *}" = rustc ]; then
      if [ "$through" != "rustc $release" ]; then
        echo "$source_file: $name is built in through $through, which is not the compiler building the program (rustc $release)" >&2
        exit 1
      fi
    elif ! grep -qxF "$through" "$work/built"; then
      echo "$source_file: $name is built in through $through, which is not built into the program for $target (cargo tree)" >&2
      exit 1
    fi
    echo
    echo "## $name $version (built in through ${through% *})"
    echo "License: $license"
    echo "Taken from: $taken"
    files="$(sed -n 's/^File: //p' "$source_file")"
    if [ -z "$files" ]; then
      echo "$source_file names no file" >&2
      exit 1
    fi
    while read -r file address sum; do
      if [ "$(sha256_of "$folder/$file")" != "${sum#sha256:}" ]; then
        echo "$folder/$file is not the file $source_file names (sha256)" >&2
        exit 1
      fi
      echo
      echo "  $file ($address):"
      echo
      sed 's/^/    /' "$folder/$file"
    done <<<"$files"
  done
} >"$notices"

iconv -f UTF-8 -t UTF-8 "$notices" >/dev/null
count="$(grep -c '^## ' "$notices")"
if [ "$count" -lt 50 ]; then
  echo "only $count entries in the notices for $target" >&2
  exit 1
fi
cat "$notices"
