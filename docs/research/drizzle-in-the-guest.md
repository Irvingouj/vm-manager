# Real Drizzle in the guest: findings

Date: 2026-11-12 (UTC). Code: branch `spike/drizzle-in-guest`, under `prototypes/drizzle-in-guest/`. Verifies the proposal in ADR 0004 and feeds the map tickets "Database SDK surface and generated types" and "Application-owned databases and migrations".

Provenance: this is the report of the engineering agent that did the work, saved as returned. The driving session independently re-ran the whole spike with `prototypes/drizzle-in-guest/run.sh` on a fresh checkout of the branch (commit `cf350bd`): exit 0, 50 PASS lines, 0 FAIL, "ALL PASS" (log `/tmp/dig-verify-run.log`). That run confirms what the script checks: the bundle runs in the host, the transaction behaviour, the statement-kind results, the `set_config` finding, the pull and migration behaviour, and the rename prompt. The driving session's own measurements from that run differ a little from the agent's and are given beside them in section 1. Not re-checked: the statements about Drizzle's source, and the agent's judgments.

In the text below: **[ran]** = the agent ran it, every line comes from `run.sh`; **[src]** = read in the source under `app/node_modules`; **[docs]** = read on the Drizzle docs site; **[nv]** = not verified.

## Verdict

Real Drizzle works in our guest. The agent would keep ADR 0004's proposal ("use Drizzle itself, through its proxy driver"), with three caveats and a little work on our side.

- **What ran:** drizzle-orm 0.45.4, bundled by esbuild into one classic script with no shims. In our host it ran select, insert, update, joins, the relational query API (`db.query…with`), and queries against a schema pulled by drizzle-kit. Every value type tested came back the way Drizzle's types promise. That needed a careful host-side row encoder (about 250 lines of Rust).
- **Transactions are the one gap in Drizzle itself.** Its proxy driver throws on `db.transaction` and has no `db.batch` (read in the source). The fix was built: one TypeScript class of about 40 lines that replaces `transaction()`, plus three host operations (`tx.begin`, `tx.commit`, `tx.rollback`). Commit, rollback and isolation settings all work.
- **Caveat 1: raw SQL is always reachable.** `db.execute(sql…)`, `sql` fragments inside typed queries, and data-modifying CTEs are all available to Application code. "Typed queries only" can't be enforced in the guest. Only the host and the database role can enforce it, and a statement-kind filter isn't enough on its own.
- **Caveat 2: renames block automation.** `drizzle-kit generate` needs an interactive answer for a renamed column. Without a TTY it writes nothing and still exits 0.
- **Caveat 3: pulling needs a privileged role.** `drizzle-kit pull` gives a correct definition only with a role that has more than SELECT. With weaker roles it silently drops primary keys and constraints, or turns arrays into plain columns.

None of this is more expensive than writing our own thin typed query layer, and agents keep the API they already know.

## 1. Does drizzle-orm run in the guest?

**Yes, with no shims. [ran]**

- **Bundling:** esbuild with `bundle`, `format: "iife"`, `platform: "neutral"`, `mainFields: ["module","main"]`, `conditions: ["import","default"]`, `target: es2022`. The entry file sets `globalThis.default = { fetch }`. The output has no `import`/`export` [ran].
- **Bundle size** (`app/build.mjs`) [ran; same in the driving session's run]:
  - 254,161 bytes unminified, 46,050 gzip; 226,787 of those bytes are drizzle-orm.
  - 99,517 bytes minified, 26,218 gzip.
- **Globals and Node APIs:** none were missing. The bundle's only notable globals are `crypto.subtle.digest`, `TextEncoder` and `TextDecoder`, all inside `hashQuery`, which only runs when Drizzle's query cache is configured [src: grep of `dist/app.js`]. OpenTelemetry tracing is a no-op unless it is configured [src].
- **Startup cost** (`dig-host bench-start`, n=30, fresh isolate, V8 runtime snapshot on):

  | Script | Worker ready, mean / p50 (agent's run) | Mean / p50 (driving session's run) |
  |---|---|---|
  | Bare guest (668 bytes) | 1.88 / 1.82 ms | 2.15 / 2.05 ms |
  | Drizzle bundle | 6.23 / 6.13 ms | 7.06 / 6.95 ms |
  | Drizzle bundle, minified | 5.74 / 5.65 ms | 7.32 / 7.06 ms |

  So Drizzle adds about 4–5 ms per new isolate. Minifying made no consistent difference. The cost of a fresh context (not a fresh isolate) that has to run the Drizzle bundle again was not measured.
- **Per-request cost** (`dig-host bench-req`, one warm worker, n=500, Postgres in Docker on the same machine) [ran]:

  | Route | Mean (agent's run) | p50 (agent's run) | p50 (driving session's run) |
  |---|---|---|---|
  | `/select-raw` (one host call, no Drizzle) | 378 µs | 374 µs | 392 µs and 441 µs (two passes) |
  | `/select-drizzle` (the same query through Drizzle) | 416 µs | 408 µs | 457 µs |
  | `/noop` | 66 µs | — | — |
  | `/users-with-tasks` (relational query) | 631 µs | — | — |

  Drizzle costs a few tens of microseconds per query.
- **Type fidelity** (`/types` route, `checks.mjs`) [ran]. The host sends what node-postgres plus Drizzle's type parsers would give:
  - **What the guest receives, and it matches Drizzle's types:**
    - numeric: exact string (`"12345678901234.123456"`)
    - bigint with `mode:"bigint"`: `BigInt` (9007199254740993n, exact); with `mode:"number"`: number
    - timestamp and timestamptz: `Date` objects, same milliseconds; with `mode:"string"`: `"2026-01-02 03:04:05.123456"`
    - date: `"2026-01-02"`
    - json and jsonb: objects
    - `text[]`: an array, including commas, quotes, backslashes and empty strings; `int[]`: an array
    - null: `null`; uuid: string
  - **bytea:** drizzle-orm 0.45.4 has no bytea column [src: no `bytea.js` in `pg-core/columns`]. A `customType` with `\x` hex text was used, which comes back as a `Uint8Array`.
  - **Parameters:** the host sends every parameter in Postgres text format and lets the server infer its type. The guest turns `bigint`, `Date` and `Uint8Array` into strings first. This is the host design, not a problem in Drizzle or the runtime.
  - **Precision loss in relational queries.** Rows nested under `with:` come back through Postgres `json_build_array`, so numeric becomes `"12345678901234.123"`, bigint becomes 9007199254740992 (off by one), and string timestamps switch to ISO format with a `T`. Drizzle's own node-postgres driver in Node gives the same result (`app/kit/rqb-node-postgres.mjs`) [ran], so this is Drizzle 0.45's behaviour, not our bridge's.

## 2. Transactions through the proxy

- **What pg-proxy offers:** in 0.45.4 its `PgRemoteSession.transaction` and `PgProxyTransaction.transaction` throw "Transactions are not supported by the Postgres Proxy driver". It has no `batch` [src: `node_modules/drizzle-orm/pg-proxy/session.js`, `driver.js`]. `batch` exists only on other drivers, such as neon-http [src]. The proxy docs page mentions neither transactions nor batch [docs: https://orm.drizzle.team/docs/connect-drizzle-proxy].
- **What was built** (`app/src/vm.ts`):
  - `HostSession extends PgRemoteSession` overrides `transaction()`: it calls `tx.begin`, runs the callback against a session whose statements carry the transaction id, then calls `tx.commit`, or `tx.rollback` on any error.
  - The host (`host/src/db.rs`) takes a pooled connection per transaction. It ties the transaction to the request that began it and rolls back anything still open when the request ends.
  - Isolation level and access mode are accepted only as fixed words.
  - Size: about 40 lines of TypeScript and about 120 lines of Rust.
- **Results** [ran]:
  - Commit works; the transaction reads its own writes.
  - A thrown error rolls back both writes (`/reassign?fail=1`: row count unchanged).
  - `tx.rollback()` rolls back.
  - A foreign-key error rejects with code 23503.
  - `serializable` + `read only` reach Postgres.
  - A transaction the Application leaves open is rolled back by the host.
- **Not built:** nested transactions (savepoints) are refused; host-side savepoint operations would be about 20 more lines [nv].
- **One quirk to know:** if the Application catches a failed statement inside a transaction and returns normally, COMMIT on the aborted transaction silently rolls back [ran]; the host could turn it into an error.
- A non-interactive `batch` like neon-http's is possible but was not built [nv].

## 3. What SQL does Drizzle send?

- **Shape:** 62 statements were captured. Every one Drizzle generated is a single statement whose bound parameters match its `$n` placeholders. `limit` is bound too. Inserts list every column, using `default` for unset ones. Relational queries use `left join lateral … json_agg(json_build_array…)`. `sqlparser` 0.59 parsed all of them [ran].
- **Literals:** the only inlined literals were ones written by hand in `sql` templates or `sql.raw`, plus `'[]'::json` in relational queries.
- **Statement-kind restriction:** with the host refusing everything but one SELECT/INSERT/UPDATE/DELETE, no Drizzle-generated statement tripped it [ran].
- **Raw SQL is always reachable from Application code:**
  - `db.execute(sql…)` and `sql` fragments inside `where` both worked.
  - Strict mode refused `SET ROLE`, DDL and multi-statement strings. Without the host check, Postgres still refuses multi-statement strings over the extended protocol (42601), and the role's privileges refuse SET ROLE and DDL (42501) [ran].
  - Things that pass the kind check:
    - `select set_config('application_name', …)` passes, and the setting **persisted into the next request on the shared connection** [ran]. If identity is ever set per session on pooled connections (SET ROLE or settings), this is a direct hole; the host must reset session state between requests [nv].
    - A DELETE inside a CTE parses as a SELECT. Only the database role stopped it (42501) [ran].
- **Conclusion:** "typed queries only" cannot be enforced in the guest. Enforcement has to come from the database role, plus the host's checks.

## 4. Pulled structure for a shared database

drizzle-kit `pull` against the sample `devops` database (`sql/20-devops.sql`) [ran]:

- **What it generates:** `schema.ts` and `relations.ts`. They worked unchanged in the guest for typed joins, relational queries and a view.
- **What it needs, by role:**

  | Role | Result |
  |---|---|
  | Owner | Complete definition |
  | SELECT-only | Runs, but drops primary keys, unique and check constraints (probably because `information_schema` hides constraints from SELECT-only roles [nv]) |
  | USAGE on the schema only, no table privileges | Runs, but `text[]` becomes `text` and all defaults vanish |
  | No USAGE on the schema | Fails, exit 1 with no message; the server logs "permission denied for schema public" |

  So the platform has to pull with a privileged role. The "structure" Access level can't simply be a weak database role.
- **What we may not want in a Builder's project:**
  - `pgPolicy` blocks with `to: ["public"]`, check constraints, indexes with operator classes, and the view's SQL.
  - Timestamps are pulled with `mode:'string'` and bigserial with `mode:"bigint"`.
  - A commented-out `0000_*.sql` file plus `meta/` files.
  - A bug: the default `'{}'` on `text[]` became `.default([""])`.
  - Other schemas are pulled only if listed in `schemaFilter`.
- **Determinism:** two runs produced byte-identical `schema.ts`, `relations.ts` and `meta/0000_snapshot.json`, also identical to the committed copy. Only the SQL file name and the journal's `when`/tag are random [ran]. So the generated `.ts` files are a usable basis for "the structure changed", across two runs on one database; not tested across Postgres versions.

## 5. Own-database migrations

- **The platform can apply them itself, without drizzle-kit or Node.** `dig-host migrate` (`host/src/migrate.rs`, about 70 lines of Rust) mirrors drizzle-orm's migrator [src: `migrator.js`, `pg-core/dialect.js`]:
  - It reads `meta/_journal.json`, splits each file on `--> statement-breakpoint`, runs all pending migrations in one transaction, and records the file's sha256 and the journal's `when` in `drizzle.__drizzle_migrations`.
  - It applied once, then 0 on a second run. drizzle-orm's own migrator, run afterwards, did nothing. Both produced the same hash/`created_at` and the same `pg_dump -s` [ran].
  - Note: a migration is applied only if its `when` is later than the last one applied; Drizzle uses the hash for nothing else [src].
- **Destructive changes:**
  - A dropped column generates `ALTER TABLE "tasks" DROP COLUMN "note";` with no prompt and no warning [ran].
  - A renamed column triggers an interactive prompt. Without a TTY, drizzle-kit prints "Interactive prompts require a TTY terminal", writes nothing, and **exits 0** [ran]. A pipeline has to check the output, not the exit code.
  - Workaround [ran]: add the new column, then `generate --custom` (an empty file for the agent to fill with the data copy), then drop the old column. Three generates, no prompt.
  - Table renames were not tested [nv].
- **File names:** generated migration names are random unless `--name` is passed [ran].

## 6. The typed surface for a Coding agent

- `platform({ app: { schema: own, access: "write" }, devops: { schema: pulled, access: "read" } }).user().db("devops")` returns Drizzle's real `PgRemoteDatabase<Schema>` for the "write" Resource. For the "read" one it returns a type keeping only `select`, `selectDistinct`, `selectDistinctOn`, `$count`, `query`, `$with`, and a `with()` restricted to selects.
- `tsc` confirms through `@ts-expect-error` lines [ran]: no insert, update, delete, execute or transaction on the read-only Resource; an unknown column fails; an undeclared Resource `"payroll"` fails.
- The read-only type is cheap but cosmetic: the object still has `execute`, and a delete built on another database object compiles into a CTE [ran]. Only the role holds.
- The two schemas live together in one Application; each `db(...)` is typed with its own Resource's schema.

## Sample handler as written (`app/src/app.ts`)

```ts
async "/reassign"(url) {
  const db = vm.user().db("app");
  return db.transaction(async (tx) => {
    const [grace] = await tx.select().from(users).where(eq(users.email, "grace@example.com"));
    const moved = await tx.update(tasks).set({ ownerId: grace.id })
      .where(eq(tasks.title, "write the spec")).returning({ id: tasks.id });
    await tx.insert(tasks).values({ ownerId: grace.id, title: "reassigned note" });
    if (url.searchParams.get("fail") === "1") throw new Error("something went wrong after two writes");
    return { moved: moved.length };
  });
},
async "/devops/services"() {
  const devops = vm.user().db("devops");
  return devops.select({ service: services.name, team: teams.name, tier: services.tier, cost: services.monthlyCost })
    .from(services).innerJoin(teams, eq(teams.id, services.teamId)).orderBy(services.name);
},
```

The same file also has the select/join, insert-returning, update-returning, aggregate and `db.query.users.findMany({ with: { tasks } })` handlers. The bundling recipe is `app/build.mjs` (settings in section 1).

## What was built, and how to repeat it

- **Branch:** `spike/drizzle-in-guest`, from `spike/identity-pass-through`, pushed to origin. One commit: `cf350bd`.
- **Runtime:** the vendored runtime is used by path and unmodified.
- **Code** is under `prototypes/drizzle-in-guest/`:
  - `host/`: Rust, `dig-host` with `serve`, `bench-start`, `bench-req`, `migrate`, `snapshot`.
  - `app/`: the Application, `vm.ts`, `build.mjs`, the drizzle-kit configs, the committed migrations, the pulled `src/devops/`.
  - `sql/`, `checks.mjs`, `guest/bare.js`, `run.sh`.
- **One command:** `prototypes/drizzle-in-guest/run.sh`. It starts Postgres in a container named `vm-drizzle-spike-pg` on 127.0.0.1:55491, uses ports 8792 and 8793 for the host, prints PASS/FAIL and removes the container at exit.
- **Versions:** drizzle-orm 0.45.4, drizzle-kit 0.31.11 (npm `latest`; `1.0.0-rc.5` exists and was not tried), esbuild 0.28.2, typescript 5.9.3, pg 8.23.1, Node 22.23.0, Postgres 17 (`postgres:17`), tokio-postgres 0.7.18, sqlparser 0.59.

## Still unknown

- **Drizzle 1.0:** relational queries v2, a built-in bytea, and whether it changes proxy transactions or the nested-JSON precision loss. Not tried.
- **Postgres types not covered** by the host encoder: interval, inet, point, vector, arrays of numeric or timestamps, domains, and type OIDs from extensions.
- **Performance:** measured on one local machine only, and the host prepares every statement with no cache.
- **Pull at scale:** behaviour on large or odd shared databases (many schemas, partitioned tables), and whether output stays stable across Postgres and drizzle-kit versions.
- **Session leak:** how to stop session state like `set_config` leaking between requests on pooled connections (for example `DISCARD ALL`, or a connection per User) and what that costs.
- **Nested transactions** via host-side savepoints, and a `batch()` for the proxy driver: not built.
- **Rename prompt:** whether any drizzle-kit flag or config answers it non-interactively. None found in 0.31's `generate` options; not exhaustively checked.
