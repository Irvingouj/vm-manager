#!/usr/bin/env bash
# The install tests (blackbox/CONTRACT.md, "Installing: one image and a
# Compose file"): builds the image from the working tree and runs
# blackbox/tests/install_*.rs against it. They are not part of the gate
# (scripts/gate.sh): they need Docker with Compose, and a build of the image.
# The release workflow runs this before it publishes anything.
#
#   scripts/install-gate.sh                    build the image, run the tests
#   VIBE_INSTALL_IMAGE=<image> scripts/install-gate.sh
#                                              run them against an image that
#                                              is already there (not built)
#
# Arguments are given to `cargo test` (for one: --no-fail-fast).
set -euo pipefail
cd "$(dirname "$0")/.."

if ! docker compose version >/dev/null 2>&1; then
  echo "the install tests need Docker with Compose (docker compose version failed)" >&2
  exit 1
fi

if [ -z "${VIBE_INSTALL_IMAGE:-}" ]; then
  VIBE_INSTALL_IMAGE=vibe:install-gate
  echo "== docker build ($VIBE_INSTALL_IMAGE)"
  docker build --tag "$VIBE_INSTALL_IMAGE" .
fi
export VIBE_INSTALL_IMAGE

# The walk deploys the template's Application with the tool, which needs
# the root npm workspace's packages, as every deploy test of the gate does.
if [ ! -d node_modules ] || [ package-lock.json -nt node_modules/.package-lock.json ]; then
  echo "== npm ci"
  npm ci --no-audit --no-fund
fi

tests=()
for file in blackbox/tests/install_*.rs; do
  [ -e "$file" ] || { echo "no install tests (blackbox/tests/install_*.rs)" >&2; exit 1; }
  tests+=(--test "$(basename "$file" .rs)")
done

echo "== install tests against $VIBE_INSTALL_IMAGE"
# One at a time: each starts an installation of its own with Compose.
cargo test -p blackbox --locked "${tests[@]}" "$@" -- --ignored --test-threads=1
