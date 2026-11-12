#!/usr/bin/env bash
# The gate (AGENTS.md, "How code gets written"). Run from anywhere in the
# repository; exits non-zero on the first step that fails.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== blackbox depends on no product crate"
products=$(cargo tree --workspace --locked --depth 0 --prefix none | awk 'NF {print $1}' | grep -vx blackbox || true)
deps=$(cargo tree -p blackbox --locked -e normal,dev,build --prefix none | awk 'NF {print $1}' | sort -u)
for p in $products; do
  if grep -qx "$p" <<<"$deps"; then
    echo "blackbox depends on the product crate $p" >&2
    exit 1
  fi
done

echo "== cargo fmt"
cargo fmt --all --check
echo "== cargo clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings
echo "== cargo test"
cargo test --workspace --locked "$@"
