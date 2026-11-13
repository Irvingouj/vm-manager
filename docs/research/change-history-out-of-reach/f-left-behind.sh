#!/usr/bin/env bash
# Part F of docs/research/change-history-out-of-reach.md: code a migration
# leaves behind runs later with the migration role's rights.
#
#   docs/research/change-history-out-of-reach/f-left-behind.sh [VERSIONS]
#
# VERSIONS: any of 16 17 18 (default "16 17 18"). For each, three scratch
# clusters under /dev/shm from the binaries in ~/.cache/vm-blackbox, with the
# three roles of ADR 0017 and a stand-in for the capture:
#   vectors      each way such code runs, and what it did to the capture
#   checks       what the fix rests on (f_checks.py)
#   maintenance  what ordinary maintenance does to the catalog rows (f_maintenance.py)
#   locks        which statements leave a lock that reading and writing rows never takes (f_locks.py)
#   extension    whether the migration role can make its own function a member of an extension (f_extension.py)
#   types        with whose rights a user-defined type's own code runs later (f_types.py)
#   cost         what a whole digest and a look at the session's own locks cost per write (f_cost.py)
#   sessions     what that look costs on a busy instance (f_sessions.py); not among the default
#                parts: `f-left-behind.sh "18 16" sessions > f-sessions.log`, on a quiet machine
# Passwords are the roles' names: scratch clusters on 127.0.0.1 only.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
VERSIONS="${1:-16 17 18}"
PARTS=" ${2:-vectors checks maintenance locks extension types cost} "
want(){ [[ "$PARTS" == *" $1 "* ]]; }
declare -A FULL=([16]=16.15.0 [17]=17.11.0 [18]=18.6.0)
DIR=""
BIN=""
cleanup() {
  if [ -n "$DIR" ] && [ -d "$DIR" ]; then
    "$BIN/pg_ctl" -D "$DIR/data" -m immediate stop >/dev/null 2>&1 || true
    rm -rf "$DIR"
  fi
  DIR=""
}
trap cleanup EXIT INT TERM
q() { # q ROLE DB [-c SQL]... (or SQL on stdin)
  local role=$1 db=$2; shift 2
  python3 -I -B "$HERE/f_q.py" "$PORT" "$role" "$db" "$@" 2>&1
}
start() { # start VERSION PART: a cluster, the roles, the stand-in capture, a first migration
  V=$1
  BIN="$HOME/.cache/vm-blackbox/postgres-${FULL[$V]}-linux-amd64/bin"
  export PQ_LIB="$HOME/.cache/vm-blackbox/postgres-${FULL[$V]}-linux-amd64/lib/libpq.so.5"
  DIR=$(mktemp -d /dev/shm/chr-f-pg$V-XXXXXX)
  echo postgres >"$DIR/pw"
  "$BIN/initdb" -D "$DIR/data" -U postgres --pwfile="$DIR/pw" --auth=scram-sha-256 -E UTF8 --locale=C >"$DIR/initdb.log" 2>&1
  PORT=$(python3 -I -B -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
  export PORT
  "$BIN/pg_ctl" -D "$DIR/data" -l "$DIR/server.log" -w -o "-p $PORT -k $DIR -c listen_addresses=127.0.0.1 -c autovacuum_naptime=1s ${PG_OPTS:-}" start >/dev/null
  echo "######## Postgres $V (${FULL[$V]}), part F, $2"
  q postgres postgres -c "CREATE ROLE plat LOGIN CREATEDB CREATEROLE PASSWORD 'plat'"
  q plat postgres -c "CREATE ROLE a1_vm LOGIN PASSWORD 'a1_vm'" -c "CREATE ROLE a1_mig LOGIN PASSWORD 'a1_mig'" -c "CREATE ROLE a1_rt LOGIN PASSWORD 'a1_rt'" \
    -c "GRANT a1_vm TO plat WITH INHERIT FALSE, SET TRUE" -c "GRANT a1_mig TO a1_vm WITH INHERIT TRUE, SET FALSE" 
  q plat postgres -c "CREATE DATABASE a1 OWNER a1_vm TEMPLATE template0"
  q a1_vm a1 <<'SQL'
  REVOKE ALL ON DATABASE a1 FROM PUBLIC;
  GRANT CONNECT ON DATABASE a1 TO a1_mig, a1_rt;
  GRANT CREATE ON DATABASE a1 TO a1_mig;
  REVOKE ALL ON SCHEMA public FROM PUBLIC;
  GRANT USAGE, CREATE ON SCHEMA public TO a1_mig;
  GRANT USAGE ON SCHEMA public TO a1_rt;
  ALTER DEFAULT PRIVILEGES FOR ROLE a1_mig GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO a1_rt;
  CREATE SCHEMA vm;
  GRANT USAGE ON SCHEMA vm TO a1_mig, a1_rt;
  CREATE TABLE vm.log (id bigserial, tbl text, op text, who text, n bigint);
  CREATE FUNCTION vm.capture() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $f$
  BEGIN INSERT INTO vm.log (tbl, op, who) VALUES (TG_TABLE_NAME, TG_OP, session_user); RETURN NULL; END $f$;
  REVOKE ALL ON FUNCTION vm.capture() FROM PUBLIC;
SQL
  # The migration, as a1_mig; the capture put on by a1_vm afterwards (as vm.capture_new_tables() would).
  q a1_mig a1 <<'SQL'
  CREATE TABLE victim (id int PRIMARY KEY, v text);
  INSERT INTO victim VALUES (1, 'honest');
  CREATE TABLE witness (at timestamptz DEFAULT clock_timestamp(), via text, ran_as text, note text);
  GRANT INSERT, SELECT ON witness TO a1_rt;
  -- what the code left behind does when it runs
  CREATE FUNCTION tamper(via text) RETURNS void LANGUAGE plpgsql AS $f$
  BEGIN
    IF EXISTS (SELECT FROM public.witness w WHERE w.via = tamper.via) THEN RETURN; END IF;
    INSERT INTO public.witness (via, ran_as) VALUES (via, current_user);
    BEGIN
      ALTER TABLE public.victim DISABLE TRIGGER vm_capture_update;
      UPDATE public.victim SET v = 'changed unseen via ' || via WHERE id = 1;
      ALTER TABLE public.victim ENABLE TRIGGER vm_capture_update;
      INSERT INTO public.witness (via, ran_as, note) VALUES (via || ' (done)', current_user, 'tampered');
    EXCEPTION WHEN OTHERS THEN
      INSERT INTO public.witness (via, ran_as, note) VALUES (via || ' (done)', current_user, 'refused: ' || SQLSTATE || ' ' || SQLERRM);
    END;
  END $f$;
  -- V1: SECURITY DEFINER
  CREATE FUNCTION v1() RETURNS void LANGUAGE plpgsql SECURITY DEFINER AS $f$ BEGIN PERFORM public.tamper('security definer'); END $f$;
  -- V2: a BEFORE trigger on the child of a cascading foreign key
  CREATE TABLE parent (id int PRIMARY KEY);
  CREATE TABLE child (id int PRIMARY KEY, p int REFERENCES parent ON DELETE CASCADE ON UPDATE CASCADE, x int);
  CREATE FUNCTION v2() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN PERFORM public.tamper('before trigger in fk cascade ' || TG_OP); RETURN coalesce(NEW, OLD); END $f$;
  CREATE TRIGGER child_before BEFORE DELETE ON child FOR EACH ROW EXECUTE FUNCTION v2();
  INSERT INTO parent VALUES (1), (2), (3); INSERT INTO child VALUES (1, 1, 0), (2, 2, 0), (3, 3, 0);
  -- V7: a CHECK constraint on the child, evaluated in ON UPDATE CASCADE
  CREATE TABLE parent7 (id int PRIMARY KEY);
  CREATE FUNCTION v7(int) RETURNS boolean LANGUAGE plpgsql AS $f$ BEGIN IF $1 = 99 THEN PERFORM public.tamper('check constraint in fk cascade'); END IF; RETURN true; END $f$;
  CREATE TABLE child7 (id int PRIMARY KEY, p int REFERENCES parent7 ON UPDATE CASCADE CHECK (v7(p)));
  INSERT INTO parent7 VALUES (1); INSERT INTO child7 VALUES (1, 1);
  -- V3: an index expression, evaluated by ANALYZE
  CREATE TABLE flag (go boolean); INSERT INTO flag VALUES (false); GRANT SELECT, UPDATE ON flag TO a1_rt;
  CREATE FUNCTION v3(int) RETURNS int LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN IF current_user = 'a1_mig' AND (SELECT go FROM public.flag) THEN PERFORM public.tamper('index expression in analyze'); END IF; RETURN $1; END $f$;
  CREATE TABLE idx (id int); INSERT INTO idx SELECT g FROM generate_series(1, 500) g;
  CREATE INDEX idx_e ON idx (v3(id));
SQL
  q a1_vm a1 <<'SQL'
  GRANT EXECUTE ON FUNCTION vm.capture() TO a1_mig; -- as the implementation under review grants it
  CREATE TRIGGER vm_capture_update AFTER UPDATE ON public.victim REFERENCING OLD TABLE AS o NEW TABLE AS n FOR EACH STATEMENT EXECUTE FUNCTION vm.capture();
SQL

}
vectors() {
  state() { q a1_vm a1 -c "SELECT 'trigger row: ' || oid || ' xmin=' || xmin || ' ctid=' || ctid || ' enabled=' || tgenabled::text FROM pg_trigger WHERE tgname = 'vm_capture_update'" -c "SELECT 'victim: ' || v FROM public.victim" -c "SELECT 'captured: ' || count(*) FROM vm.log"; }
  echo "-- at rest, before"; state
  echo "-- control: the runtime role's own update is captured"
  q a1_rt a1 -c "UPDATE victim SET v = 'by the runtime role' WHERE id = 1"; state
  echo "-- control: the runtime role itself may not disable"
  q a1_rt a1 -c "ALTER TABLE victim DISABLE TRIGGER vm_capture_update"
  echo "-- V1 security definer, called by the runtime role"
  q a1_rt a1 -c "SELECT v1()"; state
  echo "-- V2 fk cascade, a delete by the runtime role"
  q a1_rt a1 -c "DELETE FROM parent WHERE id = 1"; state
  echo "-- V7 check constraint in fk cascade, an update by the runtime role"
  q a1_rt a1 -c "UPDATE parent7 SET id = 99 WHERE id = 1"; state
  echo "-- V3 index expression: the runtime role sets the flag and changes rows; autovacuum analyzes"
  q a1_rt a1 -c "UPDATE flag SET go = true" -c "INSERT INTO idx SELECT g FROM generate_series(1, 5000) g" 2>&1 | head -3
  for i in $(seq 1 40); do
    n=$(q a1_vm a1 -c "SELECT count(*) FROM public.witness WHERE via LIKE 'index%done%'")
    [ "$n" != "0" ] && break; sleep 0.5
  done
  state
  echo "-- V4 the migration role lets the runtime role create; a table made at run time"
  q a1_mig a1 -c "CREATE SCHEMA s" -c "GRANT USAGE, CREATE ON SCHEMA s TO a1_rt"
  q a1_rt a1 -c "CREATE TABLE s.loose (id int)" -c "INSERT INTO s.loose VALUES (1)" -c "SELECT 'loose rows: ' || count(*) FROM s.loose"
  if [ "$V" -ge 17 ]; then
    echo "-- V5 MAINTAIN: the runtime role refreshes a materialized view"
    q a1_mig a1 -c "CREATE FUNCTION v5() RETURNS int LANGUAGE plpgsql AS \$f\$ BEGIN PERFORM public.tamper('materialized view refresh'); RETURN 1; END \$f\$" -c "CREATE MATERIALIZED VIEW mv AS SELECT 1 AS x WITH NO DATA" 
    q a1_mig a1 -c "CREATE OR REPLACE FUNCTION mvf() RETURNS int LANGUAGE sql AS 'SELECT public.v5()'" -c "DROP MATERIALIZED VIEW mv" -c "CREATE MATERIALIZED VIEW mv AS SELECT mvf() AS x WITH NO DATA" -c "GRANT MAINTAIN ON mv TO a1_rt"
    q a1_rt a1 -c "REFRESH MATERIALIZED VIEW mv"; state
  fi
  if [ "$V" -ge 18 ]; then
    echo "-- V8 a virtual generated column calling a function of the migration's"
    q a1_mig a1 -c "CREATE FUNCTION v8(int) RETURNS int LANGUAGE plpgsql IMMUTABLE AS \$f\$ BEGIN RETURN \$1; END \$f\$" -c "CREATE TABLE gen (id int, g int GENERATED ALWAYS AS (v8(id)) VIRTUAL)"
    q a1_mig a1 -c "CREATE DOMAIN d8 AS int CHECK (v8(VALUE) > 0)" -c "CREATE TABLE gen2 (id int, g d8 GENERATED ALWAYS AS (id) VIRTUAL)"
    q a1_mig a1 -c "CREATE TABLE gen3 (id int, g int GENERATED ALWAYS AS (id * 2) VIRTUAL)" -c "INSERT INTO gen3 VALUES (1)"
    q a1_vm a1 -c "CREATE FUNCTION vm.cap3() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS \$f\$ BEGIN INSERT INTO vm.log (tbl, op, who) SELECT TG_TABLE_NAME, to_jsonb(r.*)::text, session_user FROM n r; RETURN NULL; END \$f\$" \
       -c "CREATE TRIGGER vm_capture_update AFTER UPDATE ON public.gen3 REFERENCING OLD TABLE AS o NEW TABLE AS n FOR EACH STATEMENT EXECUTE FUNCTION vm.cap3()"
    q a1_rt a1 -c "UPDATE gen3 SET id = 5"
    q a1_vm a1 -c "SELECT 'gen3 row as captured: ' || op FROM vm.log WHERE tbl = 'gen3'"
  fi
  echo "-- the witness"
  q a1_vm a1 -c "SELECT via || ' | ran as ' || ran_as || ' | ' || coalesce(note, '') FROM public.witness ORDER BY at"

}
more() { # what the checks need beside the first migration
  q a1_mig a1 <<'SQL'
  CREATE FUNCTION v1b() RETURNS void LANGUAGE plpgsql SECURITY DEFINER AS $f$
  BEGIN
    ALTER TABLE public.victim DISABLE TRIGGER vm_capture_update;
    UPDATE public.victim SET v = 'changed unseen in a transaction' WHERE id = 1;
    ALTER TABLE public.victim ENABLE TRIGGER vm_capture_update;
  END $f$;
  CREATE TABLE parent3 (id int PRIMARY KEY);
  CREATE TABLE child3 (id int PRIMARY KEY, p int REFERENCES parent3 ON DELETE CASCADE);
  CREATE FUNCTION v2b() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN
    ALTER TABLE public.victim DISABLE TRIGGER vm_capture_update;
    UPDATE public.victim SET v = 'changed unseen at commit by a held cursor ' || OLD.id WHERE id = 1;
    ALTER TABLE public.victim ENABLE TRIGGER vm_capture_update;
    RETURN OLD; END $f$;
  CREATE TRIGGER child3_before BEFORE DELETE ON child3 FOR EACH ROW EXECUTE FUNCTION v2b();
  INSERT INTO parent3 SELECT g FROM generate_series(1, 9) g; INSERT INTO child3 SELECT g, g FROM generate_series(1, 9) g;
  CREATE FUNCTION cascade_now() RETURNS int LANGUAGE plpgsql AS $f$ BEGIN DELETE FROM public.parent3 WHERE id = (SELECT min(id) FROM public.parent3); RETURN 1; END $f$;
  CREATE INDEX idx_p ON idx (id) WHERE v3(id) > 0;
  CREATE FUNCTION plus(int, int) RETURNS int LANGUAGE sql IMMUTABLE AS 'SELECT $1 + $2';
  CREATE OPERATOR @+@ (LEFTARG = int, RIGHTARG = int, FUNCTION = plus);
  CREATE INDEX idx_op ON idx ((id @+@ 1));
  CREATE DOMAIN posint AS int CHECK (v3(VALUE) > 0);
  CREATE INDEX idx_dom ON idx ((id::posint));
  CREATE STATISTICS st_e ON (v3(id)), (id + 1) FROM idx;
SQL

}
for v in $VERSIONS; do
  if want vectors; then start "$v" vectors; vectors; cleanup; fi
  if want checks; then start "$v" checks; more; python3 -I -B "$HERE/f_checks.py" "$PORT" 2>&1; cleanup; fi
  if want maintenance; then start "$v" maintenance; python3 -I -B "$HERE/f_maintenance.py" "$PORT" 2>&1; cleanup; fi
  if want locks; then start "$v" locks; python3 -I -B "$HERE/f_locks.py" "$PORT" 2>&1; cleanup; fi
  if want extension; then start "$v" extension; python3 -I -B "$HERE/f_extension.py" "$PORT" 2>&1; cleanup; fi
  if want types; then start "$v" types; python3 -I -B "$HERE/f_types.py" "$PORT" 2>&1; cleanup; fi
  if want cost; then start "$v" cost; python3 -I -B "$HERE/f_cost.py" "$PORT" 2>&1; cleanup; fi
  if want sessions; then
    PG_OPTS="-c max_connections=100"; start "$v" "sessions, max_connections=100"; python3 -I -B "$HERE/f_sessions.py" "$PORT" throughput 2>&1; cleanup
    PG_OPTS="-c max_connections=500"; start "$v" "sessions, max_connections=500"; python3 -I -B "$HERE/f_sessions.py" "$PORT" scaling 2>&1; python3 -I -B "$HERE/f_sessions.py" "$PORT" throughput 2>&1; cleanup
    PG_OPTS=""
  fi
done
