#!/usr/bin/env bash
# Tries a packed tool as a Builder would, on the machine it was built for
# (blackbox/CONTRACT.md, "The first release: The workflow"): the release
# workflow runs it on Linux and on macOS before anything is published.
#
#   scripts/try-tool.sh --archive <folder>/vibe-<v>-<target>.tar.gz
#
# 1. unpacks the archive in a folder of its own with the system's tar, and
#    the unpacked vibe must say `vibe <the package's version>` first;
# 2. makes a project from the template (`vibe new`) and installs its
#    packages (`npm install`; needs Node 22 or later and the npm registry);
# 3. sends it one request on this machine (`vibe request --as outsider /`),
#    which downloads the local run's Postgres 18 into the folder of 1;
# 4. starts an installation on loopback for the check to ask, with that
#    Postgres and `vibe serve --stand-in-sign-in` (development only, never
#    an installation of anyone's), signs the tool in to it, and runs
#    `vibe check` on the project, which must pass.
# Everything is made in one temporary folder, which is removed at the end
# with the processes it started; the tool's credential and the downloaded
# Postgres are kept there too (XDG_CONFIG_HOME, XDG_DATA_HOME), so nothing
# of the user's own is read or changed.
set -euo pipefail

usage() {
  echo "usage: scripts/try-tool.sh --archive <vibe-<version>-<target>.tar.gz>" >&2
  exit 2
}

archive=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --archive)
      [ "$#" -ge 2 ] && [ -n "$2" ] || usage
      archive="$2"
      shift 2
      ;;
    *) usage ;;
  esac
done
[ -n "$archive" ] || usage
case "$archive" in /*) ;; *) archive="$(pwd)/$archive" ;; esac
[ -f "$archive" ] || { echo "try-tool: $archive is not there" >&2; exit 1; }

cd "$(dirname "$0")/.."
package="$(awk '/^\[/ { p = ($0 == "[package]") } p && /^version *=/ { sub(/^version *= *"/, ""); sub(/".*/, ""); print; exit }' crates/vibe/Cargo.toml)"

# A short path: Postgres's socket path must stay under about 100 bytes.
work="$(mktemp -d /tmp/vibe-try.XXXXXX)"
serve_pid=""
pg_bin=""
finish() {
  if [ -n "$serve_pid" ]; then
    kill "$serve_pid" 2>/dev/null || true
    wait "$serve_pid" 2>/dev/null || true
  fi
  if [ -n "$pg_bin" ] && [ -f "$work/pg/postmaster.pid" ]; then
    "$pg_bin/pg_ctl" -D "$work/pg" -m immediate -w -t 60 stop >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
}
trap finish EXIT
export XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"

step() { echo "== try-tool: $*"; }
fail() {
  echo "try-tool: $*" >&2
  exit 1
}

step "unpack $(basename "$archive")"
mkdir "$work/unpacked"
tar -xzf "$archive" -C "$work/unpacked"
vibe="$(echo "$work"/unpacked/vibe-*/vibe)"
[ -x "$vibe" ] || fail "the archive holds no executable vibe-*/vibe"
said="$("$vibe" --version)"
first="${said%%$'\n'*}"
echo "$first"
[ "$first" = "vibe $package" ] || fail "the unpacked vibe says \"$first\" first, not \"vibe $package\""

step "vibe new"
cd "$work"
"$vibe" new hello
cd "$work/hello"
step "npm install"
npm install --no-audit --no-fund

step "vibe request --as outsider / (downloads the local run's Postgres)"
"$vibe" request --as outsider / | tee "$work/answer"
grep -q 'Hello, outsider' "$work/answer" || fail "the project's page did not greet outsider"

# A free port of loopback, as the system gives one.
free_port() {
  node -e 'const s = require("net").createServer(); s.listen(0, "127.0.0.1", () => { console.log(s.address().port); s.close(); });'
}

step "an installation on loopback for vibe check (Postgres 18, vibe serve --stand-in-sign-in)"
pg_bin="$(echo "$XDG_DATA_HOME"/vibe/postgres/*/bin)"
[ -x "$pg_bin/initdb" ] || fail "vibe request left no Postgres in $XDG_DATA_HOME/vibe/postgres"
"$pg_bin/initdb" -D "$work/pg" -U postgres -A trust -E UTF8 --no-sync >"$work/initdb.log" 2>&1 \
  || { cat "$work/initdb.log" >&2; fail "initdb failed"; }
pg_port="$(free_port)"
mkdir "$work/sock"
"$pg_bin/pg_ctl" -D "$work/pg" -l "$work/pg.log" -w -t 60 \
  -o "-p $pg_port -k $work/sock -c listen_addresses=127.0.0.1 -c fsync=off" start >/dev/null \
  || { cat "$work/pg.log" >&2; fail "Postgres did not start"; }

domain="vibe-try.test"
port="$(free_port)"
key="$(openssl rand -base64 32)"
VIBE_DATABASE_URL="postgres://postgres@127.0.0.1:$pg_port/postgres" VIBE_SECRET_KEY="$key" \
  "$vibe" serve --domain "$domain" --listen "127.0.0.1:$port" --stand-in-sign-in \
  --public-scheme http 2>"$work/serve.log" &
serve_pid=$!
key=""
for _ in $(seq 1 120); do
  if curl -fsS --max-time 2 -H "Host: api.$domain" "http://127.0.0.1:$port/v1/installation" >/dev/null 2>&1; then
    break
  fi
  kill -0 "$serve_pid" 2>/dev/null || { cat "$work/serve.log" >&2; fail "vibe serve ended"; }
  sleep 1
done
curl -fsS --max-time 5 -H "Host: api.$domain" "http://127.0.0.1:$port/v1/installation" >/dev/null \
  || { cat "$work/serve.log" >&2; fail "vibe serve did not answer within 120 seconds"; }

step "vibe sign-in (the stand-in), vibe check"
"$vibe" sign-in --domain "$domain" --connect "127.0.0.1:$port" --stand-in-user try-tool-builder
"$vibe" check
step "vibe $package of $(basename "$archive") works"
