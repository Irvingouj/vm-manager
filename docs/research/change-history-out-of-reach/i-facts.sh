#!/usr/bin/env bash
# Part I of docs/research/change-history-out-of-reach.md: what Postgres does
# when a table's rows are written anew, on 16, 17 and 18.
#
#   docs/research/change-history-out-of-reach/i-facts.sh [VERSIONS]
#
# Starts a server of each version from the black-box harness's cache
# (~/.cache/vm-blackbox) in /dev/shm, runs i_facts.py against it as a role
# without superuser that owns its database, and stops it.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
declare -A FULL=([16]=16.15.0 [17]=17.11.0 [18]=18.6.0)
for V in ${1:-18 16 17}; do
  HOME_PG="$HOME/.cache/vm-blackbox/postgres-${FULL[$V]}-linux-amd64"
  BIN="$HOME_PG/bin"
  export PQ_LIB="$HOME_PG/lib/libpq.so.5"
  DIR=$(mktemp -d "/dev/shm/i-facts-pg$V-XXXXXX")
  echo postgres >"$DIR/pw"
  "$BIN/initdb" -D "$DIR/data" -U postgres --pwfile="$DIR/pw" --auth=scram-sha-256 -E UTF8 --locale=C >"$DIR/initdb.log" 2>&1
  PORT=$(python3 -I -B -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')
  "$BIN/pg_ctl" -D "$DIR/data" -l "$DIR/server.log" -w -o "-p $PORT -k $DIR -c listen_addresses=127.0.0.1" start >/dev/null
  echo "######## Postgres $V (${FULL[$V]})"
  python3 -I -B -c "
import sys; sys.path.insert(0, '$HERE'); from pq import Conn
su = Conn($PORT, 'postgres', 'postgres', password='postgres')
su.one(\"CREATE ROLE owner LOGIN PASSWORD 'owner'\"); su.one('CREATE DATABASE d OWNER owner TEMPLATE template0')
"
  python3 -I -B "$HERE/i_facts.py" "$PORT"
  "$BIN/pg_ctl" -D "$DIR/data" -m immediate stop >/dev/null 2>&1 || true
  rm -rf "$DIR"
done
