#!/usr/bin/env bash
# Puts a libclang where the build looks for one, for a Linux machine that has
# none installed and no administrator rights (README.md, "Build and test").
#
# The statement parser (the `pg_query` crate) generates its bindings with
# bindgen at build time, and bindgen needs libclang. `.cargo/config.toml`
# adds `.tools/libclang` to the places it searches, beside the system's own,
# and names `.tools/libclang/system-cc` as the compiler bindgen asks for its
# system include directories (libclang alone has no `stdbool.h`). This script
# fills that directory:
#
# - `libclang.so`, from the `libclang` wheel on PyPI (fetched with pip, no
#   administrator rights needed);
# - `system-cc`, a link to this machine's C compiler (gcc), which answers
#   bindgen's question about include directories the way clang would.
#
# Run it once per checkout. Nothing else is changed.
set -euo pipefail
cd "$(dirname "$0")/.."

version=18.1.1
dest=.tools/libclang

cc=$(command -v gcc || command -v cc || true)
if [ -z "$cc" ]; then
  echo "no C compiler (gcc or cc) on PATH: the build needs one anyway" >&2
  exit 1
fi

# The download is untrusted data: it goes to an empty directory of its own and
# is only ever read as a zip, by path.
download=$(mktemp -d)
trap 'rm -rf "$download"' EXIT
python3 -m pip download --quiet --no-deps --only-binary=:all: \
  --dest "$download" "libclang==$version"
wheel=$(ls "$download"/libclang-*.whl)

mkdir -p "$dest"
python3 -I - "$wheel" "$dest/libclang.so" <<'PY'
import shutil, sys, zipfile
wheel, target = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(wheel) as archive:
    names = [n for n in archive.namelist() if n.endswith("/native/libclang.so")]
    if len(names) != 1:
        sys.exit(f"expected one native/libclang.so in the wheel, found {names}")
    with archive.open(names[0]) as source, open(target, "wb") as out:
        shutil.copyfileobj(source, out)
PY
ln -sfn "$cc" "$dest/system-cc"
echo "libclang $version is in $dest; system-cc -> $cc"
