#!/usr/bin/env bash
# Create a standard (non-admin, no sudo) local user and run two Postgres
# sessions as that user, in its own per-user directory.
# usage: ci/nonadmin-unix.sh <lp-binary> <pg-version> <zonky|theseus>
set -uo pipefail
BIN="$1"; VER="$2"; SRC="$3"
U=lpuser
case "$(uname)" in
  Linux)
    sudo useradd -m -s /bin/bash "$U"
    HOMEDIR=/home/$U
    ;;
  Darwin)
    sudo sysadminctl -addUser "$U" -password "$(openssl rand -hex 16)" -home /Users/$U 2>&1 | tail -2
    sudo mkdir -p /Users/$U && sudo chown "$U":staff /Users/$U
    HOMEDIR=/Users/$U
    echo "admin group member? $(dseditgroup -o checkmember -m $U admin 2>&1)"
    ;;
esac
id "$U"
echo "sudo rights: $(sudo -l -U "$U" 2>&1 | tail -1)"
cp "$BIN" /tmp/lp-nonadmin && chmod 755 /tmp/lp-nonadmin
status=0
for i in 1 2; do
  echo "--- non-admin session $i"
  sudo -u "$U" -H env HOME="$HOMEDIR" /tmp/lp-nonadmin pg "~/lp spike Zoë 数据" "$VER" "$SRC" || status=1
done
sudo -u "$U" ls -la "$HOMEDIR/.local/share/lp spike Zoë 数据" || true
sudo rm -rf "$HOMEDIR/.local/share/lp spike Zoë 数据" /tmp/lp-nonadmin
[ $status -eq 0 ] && echo "NONADMIN: PASS" || echo "NONADMIN: FAIL"
exit $status
