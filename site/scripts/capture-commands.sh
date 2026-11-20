#!/usr/bin/env bash
# Writes content/commands.md from the help of a built `vibe` (its path is the
# first argument). Run it again after a release changes the tool's commands:
#   site/scripts/capture-commands.sh target/release/vibe
set -euo pipefail
B="$1"
OUT="$(cd "$(dirname "$0")/.." && pwd)/content/commands.md"

sec() { printf '\n## %s\n\n' "$1"; if [ -n "${2:-}" ]; then printf '%s\n\n' "$2"; fi; }
cmd() {
  printf '### `vibe %s`\n\n```text\n' "$*"
  "$B" "$@" --help
  printf '```\n\n'
}

{
  printf "# The tool's commands\n\n"
  printf 'Every command of the tool `vibe` %s, as `vibe <command> --help` prints it. `vibe --help` lists them all; that list and the exit codes are at the end of this page.\n' "$("$B" --version | head -n 1 | cut -d' ' -f2)"
  sec "Signing in"
  for c in sign-in sign-out whoami; do cmd "$c"; done
  sec "Making an Application"
  for c in new check generate; do cmd "$c"; done
  sec "Shared databases"
  for c in resources pull; do cmd "$c"; done
  sec "Running it on your computer"
  cmd dev; cmd request; cmd local
  for c in operation-log change-history reset; do cmd local "$c"; done
  sec "Deploying"
  for c in deploy versions go-back logs; do cmd "$c"; done
  sec "Sharing and Contributors"
  for c in applications show share unshare contributors; do cmd "$c"; done
  cmd contributors add; cmd contributors remove
  sec "For Platform administrators" '`vibe serve` is the program the image runs; `vibe setup` is run inside the installation'"'"'s container: `docker compose exec vibe vibe setup`.'
  cmd serve; cmd setup; cmd admin connect-oidc
  sec "Every command and the exit codes"
  printf '```text\n'
  "$B" --help
  printf '```\n'
} > "$OUT"
