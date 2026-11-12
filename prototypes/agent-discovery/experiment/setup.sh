#!/usr/bin/env bash
# Creates a trial project at <target-dir>: a fresh copy of the template as its own
# Git repository with one commit, dependencies installed, and the `vm` tool wired
# to the catalog (signed in as the catalog's "signedIn" user).
#
#   experiment/setup.sh <target-dir> [--name <app-name>]
#
# Environment:
#   CATALOG   catalog file to use (default: experiment/catalog.json next to this script)
#   VM_HOME   where the tool keeps its sign-in and installation state (default: ~/.vm).
#             Shared by every trial that uses the same VM_HOME; setup resets its deploys.
set -euo pipefail

usage() { echo "usage: $0 <target-dir> [--name <app-name>]" >&2; exit 64; }
[ $# -ge 1 ] || usage
TARGET="$1"; shift
APP_NAME=""
while [ $# -gt 0 ]; do
  case "$1" in
    --name) APP_NAME="$2"; shift 2 ;;
    *) usage ;;
  esac
done

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROTO="$(dirname "$HERE")"
CATALOG="$(cd "$(dirname "${CATALOG:-$HERE/catalog.json}")" && pwd)/$(basename "${CATALOG:-$HERE/catalog.json}")"
VM_HOME="${VM_HOME:-$HOME/.vm}"

if [ -e "$TARGET" ] && [ -n "$(ls -A "$TARGET" 2>/dev/null)" ]; then
  echo "error: $TARGET exists and is not empty" >&2; exit 1
fi
[ -f "$CATALOG" ] || { echo "error: no catalog at $CATALOG" >&2; exit 1; }

mkdir -p "$TARGET"
TARGET="$(cd "$TARGET" && pwd)"

echo "==> copying the template to $TARGET"
cp -R "$PROTO/template/." "$TARGET/"
mkdir -p "$TARGET/.vm/packages"
( cd "$PROTO/packages/sdk" && npm pack --silent --pack-destination "$TARGET/.vm/packages" >/dev/null )
( cd "$PROTO/packages/cli" && npm pack --silent --pack-destination "$TARGET/.vm/packages" >/dev/null )

echo "==> writing vm.dev.json and the Application name"
node - "$CATALOG" "$TARGET" "$APP_NAME" <<'NODE'
const fs = require("fs"), path = require("path");
const [catalogFile, target, appName] = process.argv.slice(2);
const c = JSON.parse(fs.readFileSync(catalogFile, "utf8"));
const id = c.signedIn, u = c.users[id];
const devFile = path.join(target, "vm.dev.json");
const dev = JSON.parse(fs.readFileSync(devFile, "utf8"));
dev.users = [{ id, displayName: u.displayName, email: u.email, groups: u.groups }];
dev.grants = [];
fs.writeFileSync(devFile, JSON.stringify(dev, null, 2) + "\n");
if (appName) {
  for (const f of ["manifest.ts", "package.json", "README.md"]) {
    const p = path.join(target, f);
    fs.writeFileSync(p, fs.readFileSync(p, "utf8").replaceAll("my-app", appName));
  }
}
NODE

echo "==> signing the tool in (VM_HOME=$VM_HOME)"
HOST="$(node -e 'console.log(new URL(JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).installation.url).host)' "$CATALOG")"
ACCOUNT="$(node -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).signedIn)' "$CATALOG")"
URL="$(node -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).installation.url)' "$CATALOG")"
mkdir -p "$VM_HOME/installations/$HOST"
ln -sfn "$CATALOG" "$VM_HOME/installations/$HOST/catalog.json"
rm -f "$VM_HOME/installations/$HOST/state.json"
printf '{\n  "installation": "%s",\n  "account": "%s"\n}\n' "$URL" "$ACCOUNT" > "$VM_HOME/config.json"

echo "==> installing dependencies"
( cd "$TARGET" && npm install --no-audit --no-fund --loglevel=error >/dev/null )

echo "==> creating the Git repository"
(
  cd "$TARGET"
  git init -q -b main
  git config user.name "$(node -e 'const c=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));console.log(c.users[c.signedIn].displayName)' "$CATALOG")"
  git config user.email "$(node -e 'const c=JSON.parse(require("fs").readFileSync(process.argv[1],"utf8"));console.log(c.users[c.signedIn].email)' "$CATALOG")"
  git add -A
  git commit -q -m "New Application from the platform template"
)

if [ "$VM_HOME" != "$HOME/.vm" ]; then
  echo "note: VM_HOME is not the default; the trial agent's shell must have VM_HOME=$VM_HOME set."
fi
echo "Ready: $TARGET"
