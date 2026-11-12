#!/usr/bin/env bash
# The whole spike in one command: Postgres in Docker, drizzle-kit generate /
# pull, the platform's own migrator, the bundle, the host, the checks and the
# measurements. Prints PASS/FAIL lines; exits non-zero if any check failed.
#
#   prototypes/drizzle-in-guest/run.sh
#
# Needs docker, node 22 + npm, cargo. Uses CARGO_TARGET_DIR (default
# /tmp/ec-target), Postgres on 127.0.0.1:$PG_PORT (default 55491), the host on
# :8792/:8793. The container is named vm-drizzle-spike-pg and removed at exit.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/ec-target}"
export PATH="$HOME/.cargo/bin:$PATH"
PG_PORT="${PG_PORT:-55491}"
CONTAINER=vm-drizzle-spike-pg
OUT="$HERE/out"
HOST_BIN="$CARGO_TARGET_DIR/release/dig-host"
rm -rf "$OUT" && mkdir -p "$OUT"
fail=0
pass() { echo "PASS $1"; }
failm() { echo "FAIL $1"; fail=1; }
psql_as() { docker exec -i "$CONTAINER" psql -q -v ON_ERROR_STOP=1 -U "$1" -d "$2"; }
cleanup() {
  [[ -n "${SERVER:-}" ]] && kill "$SERVER" 2>/dev/null
  docker rm -f "$CONTAINER" >/dev/null 2>&1
}
trap cleanup EXIT

echo "== Postgres"
docker rm -f "$CONTAINER" >/dev/null 2>&1
docker run -d --name "$CONTAINER" -e POSTGRES_PASSWORD=spike-local-only -p "127.0.0.1:$PG_PORT:5432" postgres:17 >/dev/null
for _ in $(seq 1 60); do docker exec "$CONTAINER" pg_isready -U postgres -q && break; sleep 0.5; done
sleep 1
psql_as postgres postgres <sql/00-cluster.sql 2>/dev/null
psql_as app_owner app <sql/10-app-grants.sql
psql_as app_owner app_kitcheck <sql/10-app-grants.sql
psql_as devops_owner devops <sql/20-devops.sql
pass "databases and roles created"

echo "== Application toolchain"
cd app
[[ -d node_modules ]] || npm ci --silent
echo "     versions: $(node -e 'for (const p of ["drizzle-orm","drizzle-kit","esbuild","typescript","pg"]) process.stdout.write(p+" "+JSON.parse(require("fs").readFileSync("node_modules/"+p+"/package.json")).version+"  ")'), node $(node -v), postgres:17 image"
if npx tsc -p . >"$OUT/tsc.log" 2>&1; then
  pass "tsc: the Application type-checks, incl. the @ts-expect-error read-only and unknown-Resource lines"
else
  failm "tsc: $(cat "$OUT/tsc.log")"
fi

echo "== 5. own database: generate, apply without drizzle-kit"
before=$(find migrations -type f | sort | xargs sha256sum)
npx drizzle-kit generate --config drizzle.own.config.ts </dev/null >"$OUT/generate-nochange.log" 2>&1
if grep -q "No schema changes" "$OUT/generate-nochange.log" && [[ "$before" == "$(find migrations -type f | sort | xargs sha256sum)" ]]; then
  pass "generate on the unchanged schema writes nothing (committed migrations are current)"
else
  failm "generate on the unchanged schema: $(tail -3 "$OUT/generate-nochange.log")"
fi
cd ..
cargo build --release -q 2>"$OUT/cargo-build.log" || { failm "cargo build: $(tail -20 "$OUT/cargo-build.log")"; exit 1; }
cargo test --release -q >"$OUT/cargo-test.log" 2>&1 && pass "cargo test (numeric decoding)" || failm "cargo test: $(tail -20 "$OUT/cargo-test.log")"
[[ -s runtime-snapshot.bin ]] || "$HOST_BIN" snapshot 2>/dev/null
APP_OWNER="host=127.0.0.1 port=$PG_PORT user=app_owner password=spike dbname=app"
if "$HOST_BIN" migrate "$APP_OWNER" app/migrations 2>"$OUT/migrate-1.log" && "$HOST_BIN" migrate "$APP_OWNER" app/migrations 2>"$OUT/migrate-2.log" &&
  grep -q "1 applied" "$OUT/migrate-1.log" && grep -q "0 applied" "$OUT/migrate-2.log"; then
  pass "platform migrator (Rust, no Node) applies once, then nothing"
else
  failm "platform migrator: $(cat "$OUT"/migrate-*.log)"
fi
cd app
OURS=$(node kit/drizzle-migrate.mjs "postgres://app_owner:spike@127.0.0.1:$PG_PORT/app" ./migrations 2>&1)
THEIRS=$(node kit/drizzle-migrate.mjs "postgres://app_owner:spike@127.0.0.1:$PG_PORT/app_kitcheck" ./migrations 2>&1)
echo "     after ours + drizzle's migrator: $OURS"
echo "     drizzle's migrator alone:        $THEIRS"
[[ "$OURS" == "$THEIRS" && "$OURS" == *hash* ]] && pass "drizzle-orm's migrator agrees with the platform's bookkeeping (no re-run, same hash/created_at)" || failm "migrator bookkeeping differs"
docker exec "$CONTAINER" pg_dump -U postgres -s -n public app | grep -Ev '^(--|\\restrict|\\unrestrict)' >"$OUT/schema-ours.sql"
docker exec "$CONTAINER" pg_dump -U postgres -s -n public app_kitcheck | grep -Ev '^(--|\\restrict|\\unrestrict)' >"$OUT/schema-theirs.sql"
diff -q "$OUT/schema-ours.sql" "$OUT/schema-theirs.sql" >/dev/null && pass "same resulting structure either way (pg_dump -s)" || failm "structure differs: diff $OUT/schema-ours.sql $OUT/schema-theirs.sql"

# Destructive changes, in a scratch copy
rm -rf kit-tmp && mkdir -p kit-tmp && cp -r migrations kit-tmp/migrations
gen() { SCHEMA="./kit-tmp/$1" OUT=./kit-tmp/migrations timeout 60 npx drizzle-kit generate --config drizzle.own.config.ts --name "$2" "${@:3}" </dev/null >"$OUT/generate-$2.log" 2>&1; echo $?; }
grep -v '  note: text("note"),' src/own/schema.ts >kit-tmp/schema.drop.ts
code=$(gen schema.drop.ts drop_note)
echo "     dropped column -> exit $code: $(cat kit-tmp/migrations/0001_drop_note.sql 2>/dev/null)"
[[ "$(cat kit-tmp/migrations/0001_drop_note.sql 2>/dev/null)" == 'ALTER TABLE "tasks" DROP COLUMN "note";' ]] && pass "drop column: generated with no prompt and no warning" || failm "drop column"
sed 's/title: text("title")/title: text("summary")/' kit-tmp/schema.drop.ts >kit-tmp/schema.rename.ts
code=$(gen schema.rename.ts rename_title)
msg=$(tr '\r' '\n' <"$OUT/generate-rename_title.log" | grep -m1 -o 'Interactive prompts require a TTY[^.]*')
echo "     renamed column, no TTY -> exit $code, message: $msg; files: $(ls kit-tmp/migrations | tr '\n' ' ')"
[[ -n "$msg" && ! -e kit-tmp/migrations/0002_rename_title.sql ]] && pass "rename column: drizzle-kit needs an interactive answer; without a TTY it writes nothing (exit code $code)" || failm "rename column"
sed 's/  title: text("title").notNull(),/  title: text("title").notNull(),\n  summary: text("summary"),/' kit-tmp/schema.drop.ts >kit-tmp/schema.add.ts
grep -v '  title: text("title").notNull(),' kit-tmp/schema.add.ts >kit-tmp/schema.after.ts
gen schema.add.ts add_summary >/dev/null
gen schema.add.ts copy_title --custom >/dev/null
gen schema.after.ts drop_title >/dev/null
[[ -e kit-tmp/migrations/0002_add_summary.sql && -e kit-tmp/migrations/0003_copy_title.sql && -e kit-tmp/migrations/0004_drop_title.sql ]] &&
  pass "rename as add / custom copy / drop: three generates, no prompt" || failm "rename in steps: $(ls kit-tmp/migrations)"
cp -r kit-tmp/migrations "$OUT/destructive-migrations"

echo "== 4. shared database: pull"
pull() { PULL_ROLE="$1" PULL_OUT="$OUT/pull-$2" timeout 120 npx drizzle-kit pull --config drizzle.pull.config.ts </dev/null >"$OUT/pull-$2.log" 2>&1; echo $?; }
pull devops_owner owner >/dev/null
pull devops_owner owner-again >/dev/null
if diff -q "$OUT/pull-owner/schema.ts" src/devops/schema.ts >/dev/null && diff -q "$OUT/pull-owner/relations.ts" src/devops/relations.ts >/dev/null &&
  diff -q "$OUT/pull-owner/meta/0000_snapshot.json" "$OUT/pull-owner-again/meta/0000_snapshot.json" >/dev/null; then
  pass "pull is deterministic: schema.ts, relations.ts and snapshot identical across runs and to the committed copy"
else
  failm "pull output differs between runs or from src/devops"
fi
echo "     pull file names: $(ls "$OUT/pull-owner" | tr '\n' ' ') / $(ls "$OUT/pull-owner-again" | tr '\n' ' ')"
pull devops_reader reader >/dev/null
pull devops_structure structure >/dev/null
code=$(pull devops_nothing nothing)
grep -q 'primaryKey' "$OUT/pull-reader/schema.ts" && failm "reader pull has primary keys (expected none)" ||
  pass "pull as a SELECT-only role: runs, but drops primary keys, unique and check constraints"
grep -q 'tags: text().notNull()' "$OUT/pull-structure/schema.ts" && ! grep -q 'default(' "$OUT/pull-structure/schema.ts" &&
  pass "pull as a no-privilege role (USAGE on schema only): runs, but text[] becomes text and defaults vanish" || failm "structure-only pull"
[[ "$code" != 0 ]] && pass "pull as a role without USAGE on the schema fails (exit $code, server: permission denied for schema public)" || failm "no-usage pull succeeded"
diff "$OUT/pull-reader/schema.ts" "$OUT/pull-owner/schema.ts" >"$OUT/pull-reader-vs-owner.diff"
diff "$OUT/pull-structure/schema.ts" "$OUT/pull-reader/schema.ts" >"$OUT/pull-structure-vs-reader.diff"

echo "== 1. bundle"
node build.mjs | tee "$OUT/bundle.json"
node build.mjs --minify | tee -a "$OUT/bundle.json"
grep -Eq '^(import|export) ' dist/app.js && failm "bundle has module syntax" || pass "bundle is one classic script (no import/export)"
cd ..

echo "== 1-3. the guest in the host"
export DIG_DB_APP="host=127.0.0.1 port=$PG_PORT user=app_rt password=spike dbname=app"
export DIG_DB_DEVOPS="host=127.0.0.1 port=$PG_PORT user=devops_reader password=spike dbname=devops"
DIG_STRICT=1 "$HOST_BIN" serve app/dist/app.js 127.0.0.1:8792 >"$OUT/host-strict.log" 2>&1 &
SERVER=$!
for _ in $(seq 1 100); do curl -s -o /dev/null http://127.0.0.1:8792/noop && break; sleep 0.1; done
grep "first worker ready" "$OUT/host-strict.log" | sed 's/^/     /'
CAPTURE_OUT="$OUT/capture.jsonl" node checks.mjs http://127.0.0.1:8792 || fail=1
kill $SERVER; wait $SERVER 2>/dev/null
# The nested-relation precision loss seen in /types-nested: Drizzle's or ours?
NODEPG=$(cd app && node kit/rqb-node-postgres.mjs "postgres://app_rt:spike@127.0.0.1:$PG_PORT/app" 2>&1)
echo "     same relational query with drizzle-orm/node-postgres in Node: $NODEPG"
[[ "$NODEPG" == *'"nested":{"estimate":"12345678901234.123","bigCount":"9007199254740992"'* ]] &&
  pass "nested relations lose numeric/bigint precision with Drizzle's own node-postgres driver too (Drizzle's, not the bridge)" ||
  failm "node-postgres nested comparison: $NODEPG"
DIG_STRICT=0 "$HOST_BIN" serve app/dist/app.js 127.0.0.1:8793 >"$OUT/host-lax.log" 2>&1 &
SERVER=$!
for _ in $(seq 1 100); do curl -s -o /dev/null http://127.0.0.1:8793/noop && break; sleep 0.1; done
curl -s http://127.0.0.1:8793/reset >/dev/null
LAX=$(curl -s http://127.0.0.1:8793/raw)
echo "$LAX" >"$OUT/raw-without-strict.json"
node -e '
const r = JSON.parse(process.argv[1]);
for (const k of ["multiStatement", "setRole", "ddl"]) console.log("     without the host check, " + k + ": " + (r[k].cause ?? JSON.stringify(r[k].ok)));
' "$LAX"
[[ "$LAX" == *"42601: cannot insert multiple commands into a prepared statement"* ]] && pass "without the host check, Postgres itself refuses a multi-statement string (extended protocol)" || failm "lax multi-statement: $LAX"
kill $SERVER; wait $SERVER 2>/dev/null
SERVER=

echo "== 1. measurements"
{
  echo "-- bare guest (no Drizzle), $(wc -c <guest/bare.js) bytes"
  "$HOST_BIN" bench-start guest/bare.js 30 2>/dev/null
  echo "-- Drizzle bundle, $(wc -c <app/dist/app.js) bytes"
  "$HOST_BIN" bench-start app/dist/app.js 30 2>/dev/null
  echo "-- Drizzle bundle minified, $(wc -c <app/dist/app.min.js) bytes"
  "$HOST_BIN" bench-start app/dist/app.min.js 30 2>/dev/null
  echo "-- per request, one warm worker"
  "$HOST_BIN" bench-req guest/bare.js /select-raw 500 2>/dev/null
  "$HOST_BIN" bench-req app/dist/app.js /select-raw 500 2>/dev/null
  "$HOST_BIN" bench-req app/dist/app.js /select-drizzle 500 2>/dev/null
  "$HOST_BIN" bench-req app/dist/app.js /noop 500 2>/dev/null
  "$HOST_BIN" bench-req app/dist/app.js /users-with-tasks 500 2>/dev/null
} | tee "$OUT/bench.txt"
grep -q "warm /select-drizzle" "$OUT/bench.txt" && pass "measurements ran (out/bench.txt)" || failm "measurements"

echo
[[ $fail == 0 ]] && echo "ALL PASS" || echo "SOME CHECKS FAILED"
exit $fail
