# What Postgres sees: roles, identity, limits and transactions

Date: 2026-11-12 (UTC). Code: branch `spike/postgres-access`, under `prototypes/postgres-access/`. Resolves the map ticket "What Postgres sees: roles, access levels, limits and transactions".

Provenance: this is the report of the engineering agent that did the work, saved as returned. The driving session independently re-ran the whole spike with `prototypes/postgres-access/run.sh 18 16 17` on a fresh checkout of the branch (commit `dba87b7`): exit 0, and for each of Postgres 18, 16 and 17 the summary line `# checks: 114 total, 0 FAIL` (log `/tmp/pg-verify-run.log`). In that log the driving session read the lines for the key claims: the `RESET ROLE` escape under `SET ROLE`, the `ts_stat` path past the parser, the forged and replayed identity being blocked, the plain setting being overwritten, the server-side timeout being lifted, the session state leaking without `DISCARD ALL`, and the cost medians (which were within a few percent of the agent's). Not re-checked: the statements about Postgres documentation, and the agent's judgments.

Everything below is **ran by the agent** unless marked otherwise. A check name is the label `run.sh` prints. "Full design" means the recommended host with the parser on.

## 1. Recommended design

**Roles.** The host never uses `SET ROLE`. It logs in directly as a low role matched to the access level it needs, and keeps one connection pool per (database, role).

- A shared database needs logins from IT: `vm_read` (USAGE + SELECT), `vm_write` (adds INSERT/UPDATE/DELETE), and optionally `vm_struct` (USAGE only). Nothing is installed in that database.
- An Application-owned database has two roles. The platform's `app1_migrator` owns every object and applies migrations. The Application's `app1_rt` login owns nothing and has no CREATE or TEMP.

**Each operation is one transaction.**

- Read level opens with `BEGIN READ ONLY`, write level with `BEGIN`. The host always runs a query of its own right after `BEGIN` (the identity attach), and that query is what stops Application SQL from switching the transaction back to read-write.
- Application SQL then goes over the extended protocol: one statement, run through a pg_query check that allows only certain statement kinds and refuses a list of functions and relations.
- `COMMIT` and `DISCARD ALL` are then sent together (pipelined) as the connection goes back to the pool.
- The parser is hygiene, not the boundary. The boundary is the login role, the READ ONLY transaction, and the signed identity.

**Identity on an Application-owned database.**

- The platform installs a `vm` schema. `vm.attach(user, app, secret)` is SECURITY DEFINER, refuses a wrong secret, and sets `vm.ctx` to `{user, app, txkey, tag}`.
  - `txkey` = backend pid / virtual transaction id / `transaction_timestamp()`.
  - `tag` = `sha256(k2 || sha256(k1 || m))`, computed with keys in `vm.keys`, which the runtime role cannot read.
- `vm.identity()` checks the tag and that `txkey` matches the current transaction. If either fails it raises an error, so nothing is written without a valid identity.
- The Change history trigger (SECURITY DEFINER, owned by `app1_migrator`) and any row-level-security policy read identity through `vm.identity()`.
- The host sends the secret as a bind parameter.

**Identity on a shared database with nothing installed:** only a label that Application SQL can overwrite. Unforgeable identity there needs IT to install the same `vm` schema. This is the earlier assumption that does not hold (see section 2, point 7).

**Limits are enforced by the host, not by Postgres settings:**

- Statement timeout: the host cancels the query, then after a grace period terminates the backend with `pg_terminate_backend` from another connection on the same role. A role may do that to its own backends, so no extra privilege is needed.
- Rows and bytes returned: the host counts while streaming and cancels when a cap is passed.
- Transaction duration: a host-side watchdog rolls back.

**What IT hands over:** two logins (read, read-write), optionally a third (structure, no SELECT), nothing installed.

## 2. Per point

**1. Roles and Access levels.** On the read login each attack was run three times: privileges only, then adding READ ONLY, then adding the parser.

| Attack (read login) | Privileges only | + READ ONLY | Full design |
|---|---|---|---|
| UPDATE; data-modifying CTE; `SELECT … FOR UPDATE` | blocked | blocked | blocked |
| IT's SECURITY DEFINER `hr.give_raise()` | **allowed** | blocked | blocked |
| `query_to_xml('select hr.give_raise()')`; `ts_stat(...)` running SQL text | **allowed** | blocked | blocked |
| `DO` block; `CREATE TEMP TABLE`; `CREATE FUNCTION pg_temp.f` | **allowed** | blocked | blocked |
| large object create (`lo_from_bytea`) | **allowed** | blocked | blocked |
| `set_config('transaction_read_only','off')` then write | **allowed** | blocked | blocked |
| `NOTIFY` / `pg_notify` to IT's listener | **allowed** | **allowed** | blocked (parser) |
| `pg_notify` hidden inside `ts_stat(...)` | allowed | allowed | **allowed: IT's listener received it** |
| `pg_terminate_backend` hidden inside `ts_stat(...)` | allowed | allowed | **allowed: reached the function** (probed with pid 0 only) |
| `LISTEN` | **allowed** | **allowed** | blocked (parser) |
| `pg_stat_activity` shows another backend's SQL text on the same login | **allowed** | — | blocked by parser |
| `pg_terminate_backend` on another request's backend, same login | **allowed** (the other backend was killed) | — | blocked by parser |
| `SET ROLE it_owner`, `SET SESSION AUTHORIZATION`, `set_config('role',…)`; reading `pg_authid`; reading the ungranted `it_private` schema | blocked | blocked | blocked |
| Catalogs list every table, including the ungranted schema | allowed | allowed | allowed (known gap) |
| Two statements in one string | blocked (extended protocol) | blocked | blocked |

More results:

- The same two-statement string over the **simple** protocol runs both statements. So the host must never send Application SQL that way.
- `SET TRANSACTION READ WRITE` sent after the host's first query fails with `25001`.
- `COMMIT` from Application SQL in the middle of an interactive transaction leaves READ ONLY. A SECURITY DEFINER write then succeeds with the parser off and is blocked with it on. **This is the one boundary-relevant job the parser does.**
- Write login with privileges only:
  - UPDATE and a data-modifying CTE are allowed, as granted.
  - TRUNCATE, ALTER, DROP and CREATE in `hr` or `public` are blocked.
  - `GRANT … TO PUBLIC` reports success but grants nothing.
  - TEMP is allowed, because Postgres grants it to PUBLIC by default.

Can Application SQL get back to a more powerful role?

- Logging in directly as `vm_read`: `RESET ROLE` succeeds but `current_user` and `session_user` stay `vm_read`. There is nowhere to go.
- One host login plus `SET ROLE app1_rt`: `RESET ROLE` makes `vm.keys` readable again. **That design is broken.**
- The same with the parser: the parser refuses `RESET ROLE`, but `ts_stat($$select to_tsvector(set_config('role','none',false))$$)` passes it and `current_user` becomes `host_login`. **Still broken.**
- Running Application SQL inside a SECURITY DEFINER function owned by `app1_rt` does stop `RESET ROLE` and `set_config('role')`. Rejected anyway, for the reasons in section 3.

Recommendation: log in directly per access level, use a READ ONLY transaction, and keep the parser as hygiene. Cost: two or three pools per shared database.

**2. Identity in the session.**

- **A, plain setting** (all that is possible with nothing installed): Application SQL overwrites `vm.user`. Forgeable.
- **`GRANT SET ON PARAMETER vm.locked` to another role** does not stop `app1_rt` setting `vm.locked`. **Read in Postgres documentation by the agent:** there is no `CREATE VARIABLE` or session-variable command, even in the devel docs (https://www.postgresql.org/docs/devel/sql-commands.html). So nothing built in helps.
- **B, signed (recommended).** Attacks that were blocked:
  - a forged `vm.ctx`, then a write;
  - **replaying alice's captured `vm.ctx` inside bob's transaction**;
  - `vm.attach` with a guessed secret;
  - calling the signing function `vm._tag` (no EXECUTE);
  - reading `vm.keys`;
  - a temp table or `pg_temp` shadow (no TEMP);
  - a write with no identity attached;
  - a forged `vm.ctx` before reading a row-level-security table.
- B results that worked as intended:
  - The secret never appeared in `pg_stat_activity` during 300 attaches watched from another connection. It showed `select vm.attach($1, $2, $3)::text`.
  - A row-level-security policy using `vm.identity()` shows alice only her own row.
- Why the transaction key includes the virtual transaction id: two transactions sent in one simple-protocol message got the **same** `transaction_timestamp()` but different vxids.
- **C, platform-owned table keyed by pid and transaction:** forging is blocked, but attaching is a write, so it **fails inside a READ ONLY transaction**. Per-operation cost is about the same as B.
- **D, a role per User:** not built. The `SET ROLE` form escapes, as shown in point 1. The login-per-User form means one connection pool per User and IT creating a role per User. **Not verified.**

**3. Change history trigger** (as `app1_rt`, parser off).

- Insert by alice, update by bob and delete by alice are recorded with old and new values, User and Application.
- Blocked:
  - `DISABLE TRIGGER`, `DROP TRIGGER` (not owner);
  - `session_replication_role` by `SET` and by `set_config` ("permission denied to set parameter");
  - INSERT, UPDATE, DELETE and TRUNCATE on `vm.history`; TRUNCATE on `app.orders`;
  - ALTER or DROP of `app.orders`, CREATE in `app`, replacing `vm.identity`;
  - calling `vm.capture()` directly.
- A `DO` block runs, because PL/pgSQL is usable by PUBLIC. It does no harm here, since every write still fires the trigger.
- Anti-pattern: if `app1_rt` owns a table, it can disable that table's trigger (allowed).
- Role setup that makes this hold: `app1_migrator` owns the tables, `vm`, and the SECURITY DEFINER functions. `app1_rt` gets only DML, no TRUNCATE, no TEMP, no CREATE, and EXECUTE on `attach` and `identity` only.

**4. Transactions.**

- **Interactive transaction:** the host holds the connection behind an opaque handle. Identity is attached once, at `BEGIN`.
  - Two statements were recorded as alice under one txid.
  - When the request ended without a commit, the work rolled back and the connection went back to the pool.
  - A 500 ms duration cap made the watchdog roll back, and later statements were refused.
  - A statement is cut at the transaction's remaining time.
  - A forged or replayed identity mid-transaction fails, per point 2.
- **Batch:** three statements are pipelined in one transaction and all recorded as alice. If one fails, everything rolls back.
- Recommendation: offer batch first. An interactive transaction holds a pooled connection for its whole life, so it needs the duration cap and the parser's ban on transaction-control statements.

**5. Limits.**

- The host timeout (1.5 s) cut `pg_sleep(5)` at 1.50 s, and still did so after Application SQL set `statement_timeout = 0`.
- A role-level `statement_timeout = 500ms` was **lifted** by `set_config` (`pg_sleep(1.2)` completed). So a Postgres setting on its own is not enough.
- A PL/pgSQL handler that swallows the cancel: the host terminated the backend after about 2.0 s.
- Row cap 1000 against a 50M-row result: stopped in about 13–18 ms.
- Byte cap 4 MiB: stopped in about 7 ms.
- **A single 200 MB value is only refused after the whole row has reached the host** (about 1.1–1.2 s). This is a gap.
- `transaction_timeout` can be set by Application SQL on 17 and 18; it does not exist on 16. This is the only difference between versions.
- Session state that leaks to the next User on a pooled connection without `DISCARD ALL`: `search_path`, temp tables, session advisory locks, `statement_timeout`. All are cleared with `DISCARD ALL`.

**6. Structure level.**

- `vm_struct` (USAGE, no SELECT) reading `pg_catalog` gets every table, column, type and constraint.
- `SELECT` on rows is denied.
- `information_schema.columns` shows **0** columns to `vm_struct`, so it cannot be the source.
- `pg_stats` shows 0 rows to `vm_struct`, but through `vm_read` it shows data values (`{active}`).
- Leak paths visible even to `vm_struct`:
  - view definitions, which can embed literals (`'Ms CEO'`);
  - CHECK constraint text and column comments;
  - `reltuples` (a row-count estimate).
- The structure path is a fixed query in host code; Application SQL never runs on it (from code, `STRUCTURE_SQL`). If IT gives no structure login, the read login can run that fixed query. Rows then stay out only because the host code says so, and the host must never pull `pg_stats`.

**7. What IT must hand over.** The recommended minimum is two logins (read, read-write) with privileges on what it exposes, plus an optional structure login. The platform creates nothing in IT's database.

With that "nothing installed" option, the following are lost or remain open:

- **There is no unforgeable identity in the session.** So there is no trustworthy row-level security by User, and no Change history on the shared database (already roadmap in ADR 0005).
- Every Application shares the same logins. Any Application can see other Applications' SQL text, terminate their backends and NOTIFY IT's listeners. The parser blocks the direct calls but **not when they are wrapped in `ts_stat`**.
- Possible fixes, all **not verified**:
  - a function allow-list in the parser instead of a deny-list;
  - IT revoking EXECUTE on `pg_notify`, `pg_terminate_backend`, `pg_cancel_backend` and `pg_stat_get_activity` from PUBLIC, and TEMP on the database;
  - per-Application logins.
- Adding unforgeable identity later means IT installs the `vm` schema: the keys table plus four functions, owned by a role IT trusts.

**8. Cost** (localhost Docker, 2000 operations each, medians; Postgres 18, 16 and 17 all within about 5%):

| Operation | Median |
|---|---|
| Bare read, prepared statement | 125–133 µs |
| Bare read, `query_typed` (unprepared) | 151–158 µs |
| Recommended read operation: BEGIN RO + label, query, COMMIT + `DISCARD ALL` + parser | 374–375 µs |
| Same without `DISCARD ALL` | 335–338 µs |
| Same without the parser | 344–347 µs |
| Parser alone, in process | 22 µs |
| Bare autocommit update | 1114 µs |
| Signed-identity update, no trigger | ~1480 µs |
| Signed-identity update with Change history trigger | 1601–1625 µs |
| Updating all 1000 rows, no trigger | ~3.0 ms |
| Updating all 1000 rows, trigger with identity not verified | ~11 ms |
| Updating all 1000 rows, trigger with identity verified | 39–49 ms |

So verifying identity costs about 28–38 µs **per row**, and bulk writes need verification once per statement or cached. A read costs two network round trips instead of one, which will matter more on a real network than these localhost numbers show.

## 3. Alternatives rejected

- One host login plus `SET ROLE`: escaped by `RESET ROLE`, and still escaped through `ts_stat` with the parser on.
- The SECURITY DEFINER wrapper ("sandwich"): it does block role changes, but it forces all Application SQL through dynamic `EXECUTE`, which mangles results, parameters and protocol. Suspected escapes (deferred triggers, cursors) are **not verified**.
- A parser as the boundary: `ts_stat` and `query_to_xml` run SQL text it never sees.
- A plain custom setting for identity: forgeable.
- `GRANT SET ON PARAMETER`: does not protect a custom setting.
- Identity table (C): fails in READ ONLY transactions.
- A role per User: costs a pool per User and roles that IT must create.
- Postgres-side limits: Application SQL can lift them.

## 4. What was built

- Branch `spike/postgres-access`, commit `dba87b7`, pushed to origin. `main` was not touched.
- Code in `prototypes/postgres-access/`: `src/host.rs` (pools, operation, batch, interactive transaction, limits), `src/policy.rs` (pg_query 6.2, which uses the Postgres 17 grammar), `src/main.rs` (checks and cost measurement), `sql/*.sql`, `run.sh`.
- The one command: `PATH=$HOME/.cargo/bin:$PATH LIBCLANG_PATH=<dir containing libclang.so> prototypes/postgres-access/run.sh 18 16 17`. With no arguments it runs 18 and 16. It starts `vm-pg-spike-<v>` containers and removes them.
- pg_query needs libclang to build. The development machine has none installed, so `libclang.so` was taken from the PyPI `libclang` 18.1.1 wheel.
- Postgres 18.6, 17.11 and 16.15.

## 5. Still unknown

- Cost over a real network, and with PgBouncer (`DISCARD ALL` and the pinned interactive transaction will affect it).
- Whether server logging records the attach secret, since it is a bind parameter. **Checked afterwards by the driving session on Postgres 18:** with `log_statement=all` the server log contains the bind parameter's value (`DETAIL:  Parameters: $1 = '...'`); after `log_parameter_max_length=0` the same statement is logged without it. So on a database where statement logging is on, the secret reaches the server log unless that setting is applied, or the secret is passed another way. The duration log and `log_parameter_max_length_on_error` were not checked.
- Statement-level or cached identity checking to cut the per-row cost.
- A function allow-list and IT-side REVOKEs to close the `ts_stat` path.
- A cap on single huge values that applies before the value is fully received.
- Key rotation, and how the SDK types parameters (the spike used typed parameters only).
- Whether pg_query's Postgres 17 grammar refuses newer syntax that Postgres 18 accepts.
- Whether the pid + vxid + timestamp transaction key can repeat across backend lifetimes. Believed not in practice; **not proven**.
