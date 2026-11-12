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
# The SDK, its type tests and the fixture projects are one npm workspace at
# the root. With --no-fail-fast a failure here does not hide what cargo test
# has to say; the gate still fails.
npm_failed=0
if [ -f package.json ]; then
  echo "== npm ci, npm test"
  if [ ! -d node_modules ] || [ package-lock.json -nt node_modules/.package-lock.json ]; then
    npm ci --no-audit --no-fund || npm_failed=1
  fi
  if [ "$npm_failed" = 0 ]; then
    npm test || npm_failed=1
  fi
  if [ "$npm_failed" = 1 ] && [[ " $* " != *" --no-fail-fast "* ]]; then
    exit 1
  fi
fi

echo "== cargo test"
cargo test --workspace --locked "$@" || exit 1
exit "$npm_failed"
