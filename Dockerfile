# The platform's image (blackbox/CONTRACT.md, "Installing: one image and a
# Compose file: The image"; ADR 0015). Built with `docker build .` and
# nothing else on the machine than Docker.
#
# Two stages: the build stage brings the toolchain of rust-toolchain.toml
# and what the build needs (a C compiler and libclang for pg_query; the V8
# prebuilt is downloaded by the first build), builds `vibe` in the release
# profile with the lock file honoured, and writes the notices of what it is
# built from (scripts/third-party-licenses.sh). The final stage holds the
# one binary, the notices, the program's own LICENSE when the repository
# has one, and the C and C++ runtime libraries the binary links (distroless
# "cc"): no shell, no package manager, no compiler, no source.
#
# The registry, the git checkouts and the target folder are BuildKit cache
# mounts, so that a second build is quick and none of it is in a layer.

# The version of crates/vibe, as the first line of `vibe --version` says it
# (`vibe <version>`). The build fails when the two differ, so that the
# image's version label cannot drift from the program.
ARG VIBE_VERSION=0.1.2

FROM rust:1.99.1-trixie AS build
ARG VIBE_VERSION

# libclang for pg_query's bindgen. (The notices' script reads `cargo
# metadata` with perl's JSON::PP, which the image has.)
RUN apt-get update \
 && apt-get install --yes --no-install-recommends clang libclang-dev \
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

# /out/doc/THIRD-PARTY-LICENSES: the notices of what the program is built
# from, for this target (scripts/third-party-licenses.sh says what they
# hold; the release's archive of the tool for the same target holds the
# same text). /out/doc/LICENSE: the program's own licence, when the
# repository has one. It is read through a read-only mount of the build's
# context, since a COPY of a file that is not there fails the build;
# .dockerignore lets LICENSE in.
COPY deploy/third-party-licenses deploy/third-party-licenses
COPY scripts/third-party-licenses.sh scripts/third-party-licenses.sh
RUN --mount=type=cache,id=vibe-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=vibe-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=vibe-target,target=/build/target \
    --mount=type=bind,source=.,target=/context,readonly \
    set -eu; \
    mkdir -p /out/doc; \
    scripts/third-party-licenses.sh > /out/doc/THIRD-PARTY-LICENSES; \
    if [ -f /context/LICENSE ]; then cp /context/LICENSE /out/doc/LICENSE; fi

# The image: the binary, its notices and LICENSE, nothing else of ours. distroless's
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
# THIRD-PARTY-LICENSES, and LICENSE when the repository has one.
COPY --from=build /out/doc/ /usr/share/doc/vibe/
# A user that is not root, given as a number (distroless's "nonroot").
USER 65532:65532
ENTRYPOINT ["vibe"]
CMD ["serve"]
