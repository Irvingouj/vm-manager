# The platform's image (blackbox/CONTRACT.md, "Installing: one image and a
# Compose file: The image"; ADR 0015). Built with `docker build .` and
# nothing else on the machine than Docker.
#
# Two stages: the build stage brings the toolchain of rust-toolchain.toml
# and what the build needs (a C compiler and libclang for pg_query; the V8
# prebuilt is downloaded by the first build), builds `vibe` in the release
# profile with the lock file honoured, and writes the notices of what it is
# built from. The final stage holds the one binary, the notices, and the
# C and C++ runtime libraries the binary links (distroless "cc"): no shell,
# no package manager, no compiler, no source.
#
# The registry, the git checkouts and the target folder are BuildKit cache
# mounts, so that a second build is quick and none of it is in a layer.

# The version of crates/vibe, as the first line of `vibe --version` says it
# (`vibe <version>`). The build fails when the two differ, so that the
# image's version label cannot drift from the program.
ARG VIBE_VERSION=0.0.0

FROM rust:1.99.1-trixie AS build
ARG VIBE_VERSION

# libclang for pg_query's bindgen; jq to read `cargo metadata` for the
# notices.
RUN apt-get update \
 && apt-get install --yes --no-install-recommends clang libclang-dev jq \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml package-lock.json ./
COPY .cargo/config.toml .cargo/config.toml
COPY crates crates
COPY sdk sdk
COPY vendor vendor
COPY blackbox/Cargo.toml blackbox/Cargo.toml
COPY blackbox/src blackbox/src

# The binary is copied out of the cache mount to /out.
RUN --mount=type=cache,id=vibe-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=vibe-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=vibe-target,target=/build/target \
    set -eu; \
    cargo build --release --locked -p vibe --bin vibe; \
    mkdir -p /out; \
    cp target/release/vibe /out/vibe; \
    first="$(/out/vibe --version | head -n 1)"; \
    if [ "$first" != "vibe ${VIBE_VERSION}" ]; then \
      echo "the Dockerfile's VIBE_VERSION is ${VIBE_VERSION}, but vibe --version says: ${first}" >&2; \
      exit 1; \
    fi

COPY deploy/third-party-licenses /build/third-party-licenses

# /out/THIRD-PARTY-LICENSES: every crate of the lock file built into the
# program for this target (`cargo tree -p vibe --edges normal,build`),
# but the workspace's own, with its version, the licence its manifest
# states, and the licence texts its source carries. Made here from the
# lock file, never kept by hand. Each crate is a block:
#
#   ## <name> <version>
#   License: <the manifest's statement>
#   ...
#
# and each text of the crate's source follows, every line of it indented by
# four spaces (so that no line of a text can be read as a block's start).
RUN --mount=type=cache,id=vibe-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=vibe-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=vibe-target,target=/build/target \
    bash <<'NOTICES'
set -euo pipefail
host="$(rustc -vV | sed -n 's/^host: //p')"
out=/out/THIRD-PARTY-LICENSES
work="$(mktemp -d)"
sep=$'\x1f'

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
      -not -iregex '.*\.\(c\|cc\|cpp\|h\|hh\|inc\|rs\|sh\|py\|js\|ts\)$' -not -path '*/debian/*'
    if [ -n "$2" ] && [ -f "$1/$2" ]; then echo "$1/$2"; fi
  } | while read -r f; do realpath "$f"; done | sort -u
}

cargo metadata --no-deps --format-version 1 --locked \
  | jq -r '.packages[].name' | sort -u > "$work/own"
cargo metadata --format-version 1 --locked --filter-platform "$host" \
  | jq -r '.packages[]
      | [.name, .version, (.license // ""), (.license_file // ""),
         (.manifest_path | sub("/Cargo.toml$"; "")), (.source // "")]
      | join("\u001f")' > "$work/packages"
cargo tree -p vibe --edges normal,build --prefix none --locked --target "$host" \
  | awk 'NF && $1 !~ /^\[/ { v = $2; sub(/^v/, "", v); print $1 " " v }' \
  | sort -u > "$work/built"

{
  echo "Third-party licences of the program vibe ${VIBE_VERSION:-}"
  echo
  echo "Every crate built into the program for ${host}, but the program's own,"
  echo "with its version, the licence its manifest states, and the licence texts"
  echo "its source carries. Made from Cargo.lock when the image was built."
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
    # repository's), up to the checkout's or the vendored copy's root.
    if [ -z "$texts" ] && [ "${source#registry+}" = "$source" ]; then
      up="$dir"
      while [ -z "$texts" ]; do
        up="$(dirname "$up")"
        if [ "$up" = / ] || [ "$up" = /build ] || [ "$(dirname "$up")" = /usr/local/cargo/git/checkouts ]; then
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
      echo "Its own folder carries no licence text; the folder that holds it (${from#"$(dirname "$from")/"}) carries:"
    fi
    while read -r file; do
      echo
      echo "  ${file#"$(realpath "$from")/"}:"
      echo
      sed 's/^/    /' "$file"
    done <<<"$texts"
  done < "$work/built"

  # The second part: code built into the program that is not a crate's own
  # and whose licence texts no crate's source carries (V8 and the libraries
  # linked into its prebuilt library, Postgres's parser, the Rust standard
  # library and its own dependencies). The texts are kept in deploy/third-party-licenses/<name>/,
  # each fetched from its project's own repository at the version built in;
  # its SOURCE says from where, at which tag or commit, and each file's
  # SHA-256, which is checked here. The build fails when the crate an entry
  # is built in through is not built in at the version the entry names.
  echo
  echo "Code built into the program that is not a crate's own, whose licence"
  echo "texts no crate's source carries. Taken from each project's own"
  echo "repository at the version built in."
  for source_file in /build/third-party-licenses/*/SOURCE; do
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
    # this image; every other entry names a crate cargo tree has built in.
    if [ "${through%% *}" = rustc ]; then
      if [ "$through" != "rustc $(rustc -vV | sed -n 's/^release: //p')" ]; then
        echo "$source_file: $name is built in through $through, which is not the compiler building the program" >&2
        exit 1
      fi
    elif ! grep -qxF "$through" "$work/built"; then
      echo "$source_file: $name is built in through $through, which is not built into the program (cargo tree)" >&2
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
      if ! echo "${sum#sha256:}  $folder/$file" | sha256sum --check --quiet - >&2; then
        echo "$folder/$file is not the file $source_file names (sha256)" >&2
        exit 1
      fi
      echo
      echo "  $file ($address):"
      echo
      sed 's/^/    /' "$folder/$file"
    done <<<"$files"
  done
} > "$out"

iconv -f UTF-8 -t UTF-8 "$out" > /dev/null
count="$(grep -c '^## ' "$out")"
if [ "$count" -lt 50 ]; then
  echo "only $count crates in $out" >&2
  exit 1
fi
NOTICES

# The image: the binary, its notices, nothing else of ours. distroless's
# "cc" for Debian 13 holds glibc and the C and C++ runtime libraries (the
# build stage's Debian), no shell and no package manager; pinned by digest.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:cb87acc574abe719b6e5c547384defa2f7b1f24af9605d3f618cf852495d8be2
ARG VIBE_VERSION
LABEL org.opencontainers.image.source="https://github.com/Irvingouj/vm-manager" \
      org.opencontainers.image.version="${VIBE_VERSION}" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.title="vibe" \
      org.opencontainers.image.description="The platform program: vibe serve"
COPY --from=build /out/vibe /usr/local/bin/vibe
COPY --from=build /out/THIRD-PARTY-LICENSES /usr/share/doc/vibe/THIRD-PARTY-LICENSES
# A user that is not root, given as a number (distroless's "nonroot").
USER 65532:65532
ENTRYPOINT ["vibe"]
CMD ["serve"]
