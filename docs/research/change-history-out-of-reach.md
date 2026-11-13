# Keeping the Change history out of a Contributor's reach: findings

Date: 2026-11-12 (UTC). Code: `docs/research/change-history-out-of-reach/` on `main`, run again with `docs/research/change-history-out-of-reach/run.sh` (all of it on Postgres 16.15, 17.11 and 18.6 from `~/.cache/vm-blackbox`, about three minutes). Resolves the spike of the issue "The Change history is kept out of a Contributor's reach" (#57).

Provenance: written by the research agent that ran the scripts. Every number and every "allowed/refused" below comes from one run of `run.sh` on all three versions, whose whole output is `docs/research/change-history-out-of-reach/run.log` (exit 0, no step other than expected, no Postgres process or scratch directory left afterwards). Markers: **[verified]** = ran it, with the script's step id and the versions; **[read from source]** = Postgres 18 documentation, quoted from the page named; **[not verified]** = reasoning or memory, not run.

Names used throughout (the scripts' set-up, `setup.sql`): `plat` is the platform's role (only `CREATEDB` and `CREATEROLE`); for one Application, `a1` is its own database, `a1_vm` the third role (the platform's own for that database), `a1_mig` the role migrations run as, `a1_rt` the runtime role; `platdb` is the platform database.

## 1. The answer

**The design is sound in its shape and can be built with `CREATEDB` and `CREATEROLE` alone, but five of its parts are wrong or weaker than claimed, and two faults found on the way are in today's code already.**

What holds [verified, all three versions]: `plat` can set up the third role so that it owns the database and schema `vm` and can still put the capture on every table the migration role makes; the migration role then cannot read the keys, read or write the outbox, change or drop anything of `vm`, take the third role's rights, or reach the platform database (part A, 103 steps of `a-attacks.sql`). The outbox commits and rolls back with the change, the relay is idempotent and survives being killed between its insert and its delete (part C). Shaping history rows in the present row type with `jsonb_populate_record` works for every type in the contract's Values table except two edge cases (part D).

What must change:

1. **The capture must not convert rows with `to_jsonb` while the Application owns a cast to `json`.** The capture runs as the third role (it must, to write the outbox), and `to_jsonb` calls a cast to `json` of a user-defined type. A migration that makes an enum, a cast `(enum AS json)` with a PL/pgSQL function, and writes one row, has that function run as `a1_vm`: in the run it read the keys and deleted the whole outbox (A81). Refusing the Version afterwards is too late: a sequence keeps a value through a rollback (A88), so the keys can be carried out of a refused migration. Fix, tested: the capture refuses (`VM003`) before converting while such a cast exists (A86j); casts written in C, which only a superuser or a trusted extension can make, are let through (A86n).
2. **"Every capture trigger present and enabled" is not a check of the capture: the migration can switch it off, change rows and switch it back on.** The final catalog then looks whole (B12). The proposed counter check is unfit (point B2 below): it accumulates earlier transactions, miscounts savepoints and partitions, and was defeated outright through an inheritance child (`b_counters.py`). What works: the platform takes, before each migration on the same session, a digest of the `pg_trigger` tuples of every capture trigger (oid, `xmin`, `ctid`), keeps it in the host's memory, and requires it unchanged afterwards. Any disable, drop, rename or replace changes it, even when undone (B13, B15c, B18b); ordinary migrations leave it alone (B19b).
3. **A migration's own code can run at COMMIT, after any check.** A deferred constraint trigger (B31) and a cursor `WITH HOLD` (B33) each switched the capture off, changed a row and switched it back on during `COMMIT`; nothing reached the outbox. Before its checks the platform must run `SET CONSTRAINTS ALL IMMEDIATE` (B32) and `CLOSE ALL` (B34), and refuse while any deferrable user trigger exists (B30b; the queued event of a dropped deferred trigger did not run, B30h).
4. **Migration text must be sent one statement at a time over the extended protocol, and no statement whose kind is `TransactionStmt` may be sent.** Today the host sends each breakpoint part with `batch_execute` (simple protocol, several statements). `COMMIT; BEGIN`, `COMMIT AND CHAIN` and `END; START TRANSACTION` commit everything before them and leave the session "in transaction", so a check of the status afterwards sees nothing (`b_tx.py`). The extended protocol refuses several commands (`42601`), and the host's own parser (`pg_query` 18.1.0) names every transaction-ending text as `TransactionStmt` (`parse/`). `DO` and `CALL` cannot commit inside the platform's transaction (`2D000`).
5. **The migration role must not keep a login between migrations.** Any role may change its own password: a migration did, and its author could then log in as `a1_mig` from outside (A43, L1). This also holds of today's owner role. `plat` can set `NOLOGIN` between migrations, which the role cannot undo (A44a to A44c).

Also: changes of data that fire no row trigger (a table rewritten with `ALTER COLUMN ... TYPE ... USING`, a table dropped, or replaced under its name) leave no history row. The capture checks cannot see them (B50e, B50f, B51c), and a digest of the tables' structure can (B50h, B51d). `TRUNCATE` is refused by a fourth capture trigger, `BEFORE TRUNCATE`, also through `CASCADE` and through a partition (A64, B40, B41e).

**What a Contributor can still do to the history once all of the above is in place:**

- **Change values without a history row per changed row, through DDL that rewrites or replaces data**: `ALTER COLUMN ... TYPE ... USING <anything>`, `DROP TABLE`, `DROP COLUMN`, `DROP OWNED`, a table renamed and re-created under its name, `ATTACH`/`DETACH PARTITION`, `INHERIT`/`NO INHERIT`, `ALTER TYPE ... RENAME VALUE` on an enum. The platform can detect each before commit from the catalogs (the tables seen in B50h and B51d; the enum and partition cases are [not verified]) and then refuse the Version or record one structural entry naming the migration, whose text is kept. It cannot record per-row old and new values for these without reading the Application's tables as the third role (see open choice 2).
- **Make the history say what the Application's own code made it say.** A trigger a migration puts on its table runs in every later User's transaction and its changes are recorded under that User. This is the Application's behaviour, as today, not a forgery of identity.
- **Put a trusted extension's objects into schema `vm`.** `CREATE EXTENSION hstore SCHEMA vm` succeeds for a role with `CREATE` on the database: 60 superuser-owned functions plus types and operators land in `vm` (A48, A48a). The migration role cannot add the platform's own objects to its extension, replace what the extension put there, (A48b, A48c). No function of `vm` resolves an unqualified name in `vm`, so this harms nothing found, but `vm` is no longer only the platform's. The catalog check names it, so the Version can be refused (A48d).
- **Fill the platform database.** Every changed row becomes a row there. A migration that updates ten million rows writes ten million. This needs a quota (open choice 6).

Nothing else was found: the attempts of part A and the attacks of part B failed or were caught by the checks above.

## 2. A. Ownership without superuser

### The set-up that works [verified, `setup.sql`, 16/17/18]

1. `plat` creates `a1_vm`, `a1_mig` and `a1_rt` (`LOGIN`). It grants itself `a1_vm WITH INHERIT FALSE, SET TRUE`, which a non-superuser needs in order to create a database owned by another role. It grants `a1_mig TO a1_vm WITH INHERIT TRUE, SET FALSE`. Then it creates `a1 OWNER a1_vm TEMPLATE template0`. It never connects to `a1`.
2. As `a1_vm`, in `a1`: `PUBLIC` loses everything on the database and on `public`. `a1_mig` gets `CONNECT` and `CREATE` on the database and `USAGE, CREATE` on `public`; `a1_rt` gets `CONNECT` and `USAGE`. Then schema `vm` (`design.sql`), and default privileges `FOR ROLE a1_mig` giving `a1_rt` rows of what `a1_mig` makes. The inheriting membership is what lets `a1_vm` set these.
3. **Who owns the database: the third role.** Owning it gives `ALTER DATABASE ... SET`, which reaches every session in it (the relay's included), and `RENAME`, `OWNER` and `DROP DATABASE` [not verified]. As non-owner, `a1_mig` is refused all three that were tried (A35 to A37). In PG 15 and later, `public` belongs to the database's owner [not verified], so here it is `a1_vm`'s, and `a1_mig` creates in it by grant.
4. **Who creates the capture triggers: `a1_vm`, through `vm.capture_new_tables()` (`SECURITY DEFINER`), called on the migration's session after each migration.** `CREATE TRIGGER` needs ownership or `TRIGGER` on the table, and `EXECUTE` on the function. `a1_vm` passes the ownership check on `a1_mig`'s tables through its inheriting membership (S11 to S12, triggers made and firing). `a1_mig` has no `EXECUTE` on `vm.capture()`, so it cannot make a capture trigger of its own, nor replace one with old and new swapped (A58, A59). Nor can it take `a1_vm`'s membership away (A34).
5. **With whose rights the outbox is written: `a1_vm`'s.** `vm.capture()` is `SECURITY DEFINER`, owned by `a1_vm`, with a pinned `search_path`.
6. A migration's Drizzle-style work in `public` and in schemas it creates works: create, alter, retype, rename, index, constrain and drop columns, add a foreign key, rename a table, create a schema and move a table into it (B19b). Partitioned tables also work (B41a).

### The attacks, as `a1_mig` [verified, `a-attacks.sql`, 16/17/18; same answer on each]

Refused with `42501` unless noted. None harmed anything: the fingerprint of `vm` (the keys' digest, the outbox, every function's text, owner, rights and settings, every relation's owner and rights, the schema, the capture triggers' tuples) is unchanged from F2 to F9, and from F0 to F1 except for the one write A71 committed on purpose.

| Attempt | Result |
|---|---|
| read the keys (A01), read/insert/update/delete/truncate the outbox (A02 to A06), its sequence (A07), `LOCK` it (A08), read `vm.migrations` (A09) | refused |
| replace, drop, take over, or change the settings of a `vm` function (A10 to A13); call `vm.attached()` (A14) | refused |
| create a table or function in `vm`, move a table into it (A15 to A17), a rule, trigger or child table on the outbox (A18 to A20) | refused |
| rename, take over, drop, comment on schema `vm` (A21 to A24) | refused |
| `REASSIGN OWNED BY a1_vm`, `DROP OWNED BY a1_vm` (A25, A26) | refused |
| `SET ROLE a1_vm`, `SET SESSION AUTHORIZATION`, `set_config('role', ...)` (A27 to A29) | refused |
| `ALTER DEFAULT PRIVILEGES FOR ROLE a1_vm` (A30); `GRANT SELECT ON vm.keys` (A31); `GRANT a1_vm TO a1_mig`, `GRANT a1_mig TO a1_rt`, `REVOKE a1_mig FROM a1_vm` (A32 to A34) | refused |
| `ALTER DATABASE a1 SET / OWNER / RENAME` (A35 to A37) | refused |
| `ALTER ROLE a1_vm`, its own `CREATEROLE`, `VALID UNTIL`; `CREATE ROLE` (A38 to A41) | refused |
| its own role settings `ALTER ROLE a1_mig SET ...` (A42) | **allowed**; affects only its own later sessions |
| its own password (A43), then log in with it (L1) | **allowed: a login of its own**. With `NOLOGIN` set by `plat`, the role may still change its password but cannot log in or get `LOGIN` back (A44a to A44c). `a1_rt` may change its own password too (A44d) |
| `SET session_replication_role = replica` (A45) | refused |
| temporary objects (A46) | refused (no `TEMPORARY`) |
| `CREATE EXTENSION dblink` (A47); `CREATE LANGUAGE plpython3u` (A50) | refused |
| `CREATE EXTENSION hstore SCHEMA vm` (A48) | **allowed**, see section 1 |
| event trigger (A51); `CREATE PUBLICATION ... FOR ALL TABLES` (A52) | refused |
| end `a1_vm`'s sessions (A53); `pg_stat_reset()` and per-table reset (A54, A55); `pg_read_file` (A56) | refused |
| give a table to `a1_vm` (A57) | refused |
| a capture trigger of its own; replace one with old/new swapped (A58, A59) | refused (no `EXECUTE` on `vm.capture()`) |
| disable, drop, `ENABLE REPLICA`, rename a capture trigger on its own table (A60 to A63) | **allowed** (the table owner's right); see part B |
| `TRUNCATE` a captured table (A64) | refused, `VM002` by the `BEFORE TRUNCATE` capture trigger |
| `SET UNLOGGED` (A65); `DROP OWNED BY a1_mig` (A66) | allowed; the first harms nothing, the second drops every table unrecorded (part B5) |
| a schema named `vm` (A67, `42P06`) or `pg_vm` (A68, `42939`) | refused |
| shadows of `to_jsonb`, `clock_timestamp`, `jsonb_object_agg` in `public`, first on the path, then a captured write (A70 to A75) | no effect: captured with the real functions |
| a cast to `json` for its own enum, then a captured write (A80, A81) | **its function ran as `a1_vm`, read the keys and deleted 6 outbox rows**; the catalog check names the cast (A82); hardened capture refuses, `VM003` (A86j, A86l) |
| the same with a cast to `jsonb` only (A84, A85) | not called by `to_jsonb` |
| a value kept in a sequence through a rollback (A87, A88) | kept |
| row-level security (`FORCE`), a default, a check, a stored generated column, an index expression and a statement trigger, all calling its functions, on a captured table, then insert/update/delete (A90, A91) | all ran as `a1_mig`, none as `a1_vm`: the capture reads transition tables, not the table |

[read from source] `to_jsonb`: "if there is a cast from the SQL data type to `json`, the cast function will be used to perform the conversion" (PG 18 docs, JSON Creation Functions). Trusted extensions: "can be installed by any user who has `CREATE` privilege on the current database ... the contained objects will be owned by the bootstrap superuser". The page names no privilege on the target schema (PG 18 docs, `CREATE EXTENSION`), which matches A48.

### Design rules that follow

- The third role must never run code a migration wrote. The capture reads only transition tables and catalogs, all names pinned. The guard of `hardening.sql` keeps out the one path found (casts to `json`). The relay touches only `vm.outbox`. **The platform must never read an Application's table as the third role or as `plat`**: row-level security with `FORCE` runs the policy's function as the reader (A91 shows it running as the writer). `plat` holds `ADMIN` over every Application's roles, which makes it the worst reader to trick.
- Everything the platform does inside a migration's transaction runs on the migration role's own session. Only that session sees the transaction's uncommitted catalog. So each such step is a call of a `vm` function that is `SECURITY DEFINER` with a pinned path, and those functions are callable by `a1_mig` too. Hence the platform keeps its baselines (digests) in the host's memory, not in the database, and reads back what it wrote (`vm.migrations`) before commit. A migration that calls these functions itself gains nothing. Shadows put first on the path do not reach a function with a pinned path (B20a).

## 3. B. What a table's owner can do to the capture, and how the platform knows before commit

### B1. The catalog check, and the trigger-tuple digest [verified, `b-checks.sql` B10 to B19, 16/17/18]

`vm.capture_problems()` (`design.sql`) lists every table outside `vm` lacking any of its four capture triggers with exactly the expected `tgtype`, `tgfoid`, transition-table names, `tgenabled` in (`O`, `A`), no `WHEN`, no column list, no arguments, not deferrable. It also lists any other trigger calling a `vm` function, any deferrable user trigger, any cast to `json`/`jsonb` from a user type whose function is not C or internal, and any extension or object in `vm` not owned by `vm`'s owner.

- A trigger of the same name calling the migration's own function is named (B17a), so the catalog cannot be made to lie by name.
- It sees only the final state: disable, update 3 rows, enable leaves it empty (B12).
- `vm.capture_digest()`, the md5 of (`tgrelid`, `tgname`, `oid`, `xmin`, `ctid`) of every capture trigger, differs after that (B13). It also differs after the same inside a released savepoint (B15c) and after a rename and back (B18b). It is equal after the same inside a savepoint rolled back, where the update did not happen either (B16c, B16d).
- Ordinary migration work leaves it equal (B19b, list in part A).
- So: take the digest before each migration, keep it in the host, require it equal after. Tables the migration made get their triggers after it and enter the next baseline. [not verified] Any Postgres operation not tried here that rewrites `pg_trigger` rows of the capture as a side effect would be refused falsely, not let through.

### B2. The transaction's own counters are unfit [verified, `b_counters.py`, 16/17/18]

- **They are not the transaction's own.** At the start of a fresh transaction, `pg_stat_xact_user_tables` already showed the previous transaction's 10 inserts. The docs say the view "counts actions taken so far within the current transaction" [read from source, PG 18 docs, Statistics Views]. The same page says a backend flushes its statistics "just before going idle, but not more frequently than once per `PGSTAT_MIN_INTERVAL` milliseconds" [read from source]. The view shows what is not yet flushed, so earlier transactions count until a flush. A reading at the start of the transaction, subtracted, fixes this. The rest below uses that difference.
- Equal to the outbox rows: a plain insert/update/delete, `ON CONFLICT DO UPDATE` (counted as 1 update), `ON CONFLICT DO NOTHING` (0), a cascading delete, `COPY`, a HOT update, `MERGE`.
- **Differ although everything was captured:** an insert in a savepoint rolled back, or in a caught `EXCEPTION` block (counted 1, captured 0); an update through a partitioned parent (counted on the partition, captured under the parent); an update moving a row between partitions (counted as a delete and an insert, captured as 1 update).
- **Blind:** `TRUNCATE` and a rewrite by `ALTER COLUMN TYPE ... USING` count nothing.
- **Defeated:** the migration creates a child of an existing table (`INHERITS`) and puts a row in it. It updates that row through the parent, which is captured under the parent and counted on the child. It then disables the parent's capture and updates the parent's own row, which is counted on the parent and not captured. The parent's numbers balance (1 update counted, 1 captured), and the child is a new table, so it is not checked.
- A non-superuser cannot reset them (A54, A55). `pg_stat_clear_snapshot()` and `pg_stat_force_next_flush()` did not change them inside the transaction.

`xmin` of rows was not pursued. It sees inserts and updates, not deletes. Reading the Application's tables as the platform would run their policies (design rules above).

### B3. Code that runs at COMMIT [verified, B30 to B34, 16/17/18]

- A `DEFERRABLE INITIALLY DEFERRED` constraint trigger, queued by an update, ran at `COMMIT`. It disabled the capture on `notes`, changed them, and re-enabled it: `notes` changed, 0 outbox rows (B31c). The catalog check names the deferrable trigger (B30b). The digest taken before `COMMIT` cannot see what runs after it.
- The same trigger dropped after its event was queued: at `COMMIT` its event did not run (B30h). So refusing while a deferrable user trigger exists is enough for queued events of such triggers.
- `SET CONSTRAINTS ALL IMMEDIATE` before the check makes the trigger run then, and the digest shows it (B32c).
- `DECLARE c CURSOR WITH HOLD FOR SELECT public.sneak2()`: at `COMMIT` the query ran and did the same to `notes`, 0 outbox rows (B33e). The check before saw nothing (B33c). `CLOSE ALL` before the check: never ran (B34b).
- [not verified] A deferred trigger that, fired by `SET CONSTRAINTS ALL IMMEDIATE`, sets constraints deferred again and queues a new event: the trigger still exists at the check and is refused. Deferred unique or exclusion rechecks with expression indexes calling the Application's functions also run at commit, but `SET CONSTRAINTS ALL IMMEDIATE` runs them before the check. Refusing every `DEFERRABLE` constraint as well as trigger closes the rest at the cost of a feature Drizzle-made schemas rarely use.

So the order inside a migration's transaction, after the last migration: `SET CONSTRAINTS ALL IMMEDIATE`; `CLOSE ALL`; the checks (trigger digest unchanged, `capture_problems()` empty, structure digest explained); `vm.migrations` written and read back; `COMMIT`. Between the checks and `COMMIT` nothing of the Application's runs: `vm` tables carry no user triggers (A19).

### B4. TRUNCATE [verified, A64, B40, B41, 16/17/18]

A fourth capture trigger, `BEFORE TRUNCATE ... FOR EACH STATEMENT EXECUTE FUNCTION vm.capture_truncate()`, raises `VM002`. This holds for `TRUNCATE` of the table, through `CASCADE` from another table (B40), and of a partitioned parent whose own trigger was dropped, because the partition's fires (B41e). Dropping the trigger first changes the digest, which is caught. Recording instead of refusing is possible: the trigger would write one `truncate` entry without reading rows (open choice 3).

### B5. Changes with no row trigger [verified, B50, B51, 16/17/18]

- `ALTER TABLE items ALTER COLUMN amount TYPE numeric USING 0` set every amount to 0 (B50d). The capture digest was unchanged, the catalog check empty, and the outbox empty (B50e to B50g). The structure digest (`vm.structure_digest()`: per table oid, `relfilenode`, columns with types, parents) differed (B50h).
- Rename `notes`, create a new `notes`, copy rewritten rows, drop the old one: the capture checks pass (B51c), and the structure digest shows a new oid under the old name (B51d).

### B6. Ending the platform's transaction [verified, `b_tx.py` 16/17/18; `parse/` once, the parser being independent of the server]

Today: `own_db.rs` begins a transaction and sends each `--> statement-breakpoint` part with `batch_execute`, i.e. the simple protocol, several statements per message [read from source, `crates/vm/src/own_db.rs`, `migrate`].

In a transaction on `a1_mig`'s session, after a captured insert, the text was sent. "Answer" is what Postgres replied, "status" the session's transaction status afterwards, and "committed" whether another session could see the earlier insert:

| Protocol, text | Answer | Status after | Earlier insert committed |
|---|---|---|---|
| simple, `COMMIT; BEGIN` | ok | in transaction | **yes** |
| simple, `COMMIT AND CHAIN` | ok | in transaction | **yes** |
| simple, `END; START TRANSACTION` | ok | in transaction | **yes** |
| simple, `ROLLBACK AND CHAIN` / `ABORT; BEGIN` | ok | in transaction | no (rolled back, migration goes on in a new one) |
| simple, `PREPARE TRANSACTION 'x'; BEGIN` | `55000` prepared transactions are disabled | idle | no |
| simple, `DO $$ BEGIN COMMIT; END $$`; `CALL` of a procedure that commits | `2D000` invalid transaction termination | in error | no |
| simple, `SELECT query_to_xml('ALTER TABLE ...')` / `query_to_xml('COMMIT')` | `0A000` not allowed in a non-volatile function / `22000` | in error | no |
| extended, `COMMIT` | ok | idle | **yes** |
| extended, `COMMIT AND CHAIN` | ok | in transaction | **yes** |
| extended, `SELECT 1; COMMIT` | `42601` cannot insert multiple commands into a prepared statement | in error | no |

- With `max_prepared_transactions` above 0 (IT's choice, not the default), a migration can `PREPARE TRANSACTION` and later `COMMIT PREPARED` it itself (L06 to L07, run with that setting).
- `pg_query` 18.1.0, `split_with_parser` then `parse`, names `COMMIT`, `END`, `COMMIT AND CHAIN`, `ABORT`, `ROLLBACK AND CHAIN`, `START TRANSACTION`, `PREPARE TRANSACTION`, `COMMIT PREPARED`, `SAVEPOINT`/`RELEASE`/`ROLLBACK TO` each `TransactionStmt`. It leaves `COMMIT` inside strings, comments and dollar quotes alone. `DO` is `DoStmt`, `CALL` is `CallStmt`, `query_to_xml(...)` is `SelectStmt`. A `drizzle-kit` breakpoint line is a comment to it.
- A PL/pgSQL body's dynamic `EXECUTE` of a computed string is opaque to it: `parse_plpgsql` shows `PLpgSQL_stmt_dynexecute` with the expression, not what it will run. So text checks cannot refuse `DISABLE TRIGGER`; only B1's digest can.
- Therefore: one statement per extended-protocol message, refuse `TransactionStmt` by the parser before sending, and check `vm.transaction_key()` unchanged after each statement as a second line. A status check alone misses the chained forms (table above).

## 4. C. The outbox and the relay [verified, `c_relay.py`, 16/17/18, cluster on disk with fsync on]

The relay of the run: read at most N rows of `vm.outbox` (oldest first, and past the first row at most B bytes of `old`+`new`) as one jsonb array. Insert them into `platdb.history` with `ON CONFLICT (application, change_id) DO NOTHING` and commit. Then delete those ids from the outbox. `change_id` is the outbox's id.

- **C1**: while the write is open another session sees 0 outbox rows; after `ROLLBACK` 0; after `COMMIT` 1.
- **C2**: transaction A writes (outbox id 7) and stays open; B writes (id 8) and commits. The relay ships 1 row (8). A commits; the relay ships 7. History holds 7 and 8. No watermark is needed: the relay ships whatever committed row is left.
- **C3**: the same batch inserted twice: `INSERT 0 1`, then `INSERT 0 0`; 1 row.
- **C4**: the relay process killed with SIGKILL after its platform-side commit, before its delete: history 3, outbox 3. Run again: shipped 3, history 3, outbox 0.
- **C5**: platform database unreachable: the outbox keeps the row; once reachable, shipped and present once.
- **C7**: two relays reading the same batch at once: inserts `2` then `0`, deletes `2` then `0`; each change once.
- **C6**: `a1_mig`, `a1_rt` and `a1_vm` are each refused `CONNECT` on `platdb`.

Measurements. Numbers by version as 16 / 17 / 18. They are noisy: one shared VM, one run each.

| What | Result |
|---|---|
| Capture cost over no capture, `UPDATE` of 100,000 rows, median of 3 | today's `vm.history` trigger 11.1 / 19.3 / 10.9 µs per row; outbox trigger (hardened, guard included) 9.8 / 14.9 / 7.8 µs per row |
| Same, `INSERT` of 100,000 rows | today 13.7 / 12.0 / 8.0 µs; outbox 10.1 / 10.6 / 5.2 µs |
| Shipping 100,000 changes of one statement (9.1 MB of old+new jsonb text, about 37 MiB of batches with metadata) | 9,400 to 20,400 rows/s over all versions and batch bounds (1,000 rows/8 MiB, 10,000 rows/8 MiB, 10,000 rows/256 KiB). No batch size was consistently best. About half the time went to the insert and commit in `platdb` |
| A single-row write (BEGIN, attach, UPDATE, COMMIT), 200 runs | median 4.0 / 4.0 / 3.9 ms |
| Shipping that write before answering (read, insert+commit in `platdb`, delete+commit in `a1`) | median 15.5 / 9.5 / 9.6 ms more, p95 18.4 / 13.9 / 12.9 ms |

So shipping before answering roughly triples a single-row write's time in this set-up (two more fsynced commits and three round trips). The outbox trigger is cheaper than today's history trigger: it has no index on the key's hash.

**Bounds** [not verified beyond the run]: each batch is bounded by rows (1,000 is enough; larger bought nothing) and by bytes (a few MiB). The first row always goes, so one large row cannot stall the relay; a row's size is already bounded by the host's write limits. The relay runs at most one batch loop per own database at a time, with a timeout on each of its three statements. The outbox itself is bounded only by disk, like any table, and grows while `platdb` is unreachable. A migration's 100,000-row change is shipped in about 5 to 10 s at the measured rates, which must fit under the deploy's time limit or be shipped after the deploy's answer (open choice 4).

## 5. D. Reading [verified, `d_read.py`, 16/17/18]

Rows are fetched from `platdb` as `array_agg(new)::text`, sent to `a1` as one `jsonb[]` parameter, and shaped as `a1_rt` in `BEGIN READ ONLY`, under another `TimeZone` than the writer's:

```sql
SELECT r.* FROM unnest($1::jsonb[]) WITH ORDINALITY AS u(j, n)
CROSS JOIN LATERAL jsonb_populate_record(NULL::public.vals, u.j) AS r ORDER BY u.n
```

- **D1, correctness.** 4 rows by 22 columns. The types: `integer`, `smallint`, `serial`, `bigint` (including 2^53+1 and its minimum), `numeric` (30 digits, trailing zeros, `NaN`, `-0.0`), `real`, `double` (`0.1`, `NaN`, `-Infinity`, `1e-300`), `boolean`, `text` (quotes, backslash, Unicode), `varchar`, `uuid`, `timestamp` (`infinity`, microseconds), `timestamptz` (`-infinity`, offsets), `date` (BC), `json`, `jsonb`, `text[]` (commas, quotes, backslash, braces, empty string, null), `integer[]` (2-D), `bytea`, `interval`, an enum, and nulls. Against `select()`, every value equal but two:
  - a `json` column comes back normalized: `{"b": 1,  "a": [1, "x"], "a": 2}` became `{"a": 2, "b": 1}`. The capture's `to_jsonb` already loses whitespace, key order and duplicate keys, today too;
  - a `jsonb` column holding JSON `null` came back SQL `NULL`. That is `jsonb_populate_record`'s rule, and it contradicts the contract's "each as itself". Taking `json`/`jsonb` columns as `u.j -> 'col'` instead of through `jsonb_populate_record` would keep it [not verified].
- **D2, present shape.** After `ADD COLUMN added`, `RENAME tx TO tx2`, `DROP COLUMN vc`, old rows read with `added` and `tx2` null and no `vc`. After `u uuid → integer`, a read holding an old uuid fails whole with `22P02`. The same read without those rows succeeds. This is as the contract's "Reading the Change history" says.
- **D3, time.** 1,000 rows (572 KiB of `jsonb[]` text): fetch from `platdb` median 5.8 / 5.6 / 5.6 ms. Shaping in `a1` (BEGIN READ ONLY, SET LOCAL, the query, COMMIT) median 31.7 / 30.2 / 30.0 ms. 20 runs each.
- **D4, the row's key.** The platform database knows no column types; the SDK sends a key as JSON. Matched as given, `{"id": 7}` and a uuid match. A `bigint` given as a string, `numeric` `"1.5"` against a stored `1.50`, and a `timestamptz` in another notation do not. Shaped first in `a1` with `jsonb_populate_record(NULL::t, key)` and turned back with `to_jsonb(r) -> col`, all five match. For `timestamptz`, only if the shaping session and the capture both use `TimeZone = 'UTC'`: under Tokyo time it printed `+09:00` and did not match. jsonb equality compares numbers by value (`1.5` = `1.50`). So a keyed read takes one more round trip to the own database, or the key is shaped in the same round trip as the rows.
- **The capture must pin its settings.** Today's capture, with the writer's `extra_float_digits = -15`, recorded `0.1234567890123` as `0.1`. It recorded a timestamp's offset in the writer's zone and an interval in the writer's `IntervalStyle` (D5, second line). `hardening.sql` pins `TimeZone`, `extra_float_digits`, `IntervalStyle`, `bytea_output` and `DateStyle` on `vm.capture()`. With it, D1's writer had `extra_float_digits = -15` and the values still round-tripped.

## 6. E. Stronger alternatives

| Fact | Result |
|---|---|
| defaults: `wal_level` | `replica`; `max_replication_slots` 10; `max_wal_senders` 10; `max_prepared_transactions` 0 [verified, E01, 16/17/18] |
| a `CREATEROLE` role creates a role with `REPLICATION`, gives it to a role it made, or `BYPASSRLS` | refused `42501` [verified, E02 to E04] |
| `plat` changes `wal_level` | refused [verified, E05] |
| a logical slot without `REPLICATION` | refused "permission denied to use replication slots" [verified, E06] |
| publication `FOR ALL TABLES` / `FOR TABLES IN SCHEMA`, as the database's owner | "must be superuser" [verified, E07, E08] |
| publication `FOR TABLE` of its own table, as its owner | allowed, with a warning about `wal_level` [verified, E09] |
| `ALTER TABLE ... REPLICA IDENTITY NOTHING` by the table's owner | allowed [verified, E10] |
| `pg_logical_emit_message` by `a1_mig` and by `a1_rt` | allowed [verified, E11, E12] |
| `PREPARE TRANSACTION` under the defaults, even as superuser | `55000` [verified, E13] |

On a cluster started with `wal_level=logical`, with a superuser's `test_decoding` slot [verified, `e-logical.sql`, 16/17/18]:

- the slot saw the update made with the capture triggers disabled;
- it saw nothing of an update made while the migration had set the table `UNLOGGED`;
- it saw a delete under `REPLICA IDENTITY NOTHING` as `(no-tuple-data)`, with no old values;
- it saw a forged `pg_logical_emit_message` as an ordinary message.

- **Logical decoding** closes "triggers switched off". It opens "table set unlogged" and "replica identity nothing", both the table owner's to do, and carries no identity unless it is signed. It needs IT to set `wal_level=logical` (a restart) and a role with `REPLICATION`, which `plat` cannot create. It also needs a slot per own database against a default of 10 slots for the whole instance, and a superuser for all-table publications. This breaks "`CREATEDB`, `CREATEROLE` and a Postgres IT supplies". Its new holes still need catalog checks of the B kind. **Not better.**
- **Two-phase commit** between `a1` and `platdb` would make the history row and the change atomic without an outbox. It needs `max_prepared_transactions` > 0 (default 0) and a recovery process for transactions left prepared. Turned on, it hands a migration `PREPARE TRANSACTION`, which the parser rule must refuse anyway. **Not better**: the outbox already gives at-least-once delivery with idempotence, which is what 2PC would buy here.
- **Keeping the history in `vm`, owned by the third role, without shipping**: part A shows the migration role cannot touch it either. This drops the relay and its latency. The product owner decided against a database a Contributor can migrate. Section 1 lists what a Contributor can still do, and the same holds whichever database keeps the history. The one difference is a Postgres-level compromise of the own database. Recorded here as a fact for the ADR, not a recommendation to reverse the decision.

## 7. Faults in today's code found on the way [verified, 16/17/18]

1. **A column named `t` breaks the capture.** `vm.capture()` writes `to_jsonb(t)` over `FROM vm_new t`, and a column named `t` wins over the alias. A row `(1, 'just t', 5)` of a table with a column `t` was recorded as `"just t"`, both by `crates/vm/src/own_db_schema.sql` as it is and by the design's copy (D5). `to_jsonb(vm_row.*)` over `FROM vm_new vm_row` recorded the whole row, even with columns named `t` and `vm_row`, and with a composite column named `vm_row` (D5).
2. **The capture records values in the writer's settings** (`extra_float_digits` loses digits; part D).
3. **A migration can end the platform's transaction** with `COMMIT; BEGIN` in its text (part B6).
4. **The owner role can give itself a known password** (A43 and L1 ran on the design's migration role, which is today's owner role in another configuration). [not verified] on today's set-up as such, but nothing in `own_db.rs` prevents it.

## 8. The design, amended

1. Roles: `plat` creates `<p>_app_<n>_vm` (owns the database and `vm`; the relay and set-up log in as it), `_mig` (migrations; `NOLOGIN` except while a migration runs, with a fresh password each time) and `_runtime`. `_vm` is an inheriting member of `_mig`. `plat` never connects to an own database.
2. `vm` holds the keys, `vm.outbox`, `vm.migrations` and the functions. The capture is three `AFTER ... FOR EACH STATEMENT` triggers plus `BEFORE TRUNCATE`, installed by `_vm`. `vm.capture()` uses `to_jsonb(vm_row.*)`, pins `TimeZone`, `extra_float_digits`, `IntervalStyle`, `bytea_output` and `DateStyle`, and refuses while a non-C cast to `json`/`jsonb` from a user type exists.
3. A migration: statements split by the host's parser, `TransactionStmt` refused, one per extended-protocol message, `vm.transaction_key()` unchanged after each. Before each migration, the trigger and structure digests kept in the host; after each, `vm.capture_new_tables()` and the digests compared. After the last: `SET CONSTRAINTS ALL IMMEDIATE`; `CLOSE ALL`; `capture_problems()` empty (deferrable user triggers, casts, extensions or foreign objects in `vm` refused); structural changes refused or recorded (open choice 2); `vm.migrations` written and read back; `COMMIT`; `_mig` back to `NOLOGIN`.
4. The relay as in part C, at start, before every history read, and before or after answering a write (open choice 4).
5. Reading as in part D, as the runtime role, read-only, with the key shaped in the own database under UTC.

## 9. Open choices, each with a recommendation

1. **Refuse or record `TRUNCATE` in a migration.** Recommend: record one `truncate` entry (no row values: the rows' last values are already in the history), so seed-reset migrations keep working.
2. **Data-changing DDL (rewrite with `USING`, drop or replace a table, drop a column, attach/detach, enum value renamed).** (a) refuse, which also refuses Drizzle's ordinary type changes; (b) record one structural entry per table and migration; (c) also record each row's value after the migration, read by the platform as `_vm`, which is safe only if the table has no `FORCE ROW LEVEL SECURITY` (refused by the check) [not verified]. Recommend (b) now, (c) later; the contract's "What this does not close" must then name it.
3. **Deferrable constraints.** Recommend: refuse deferrable user triggers (needed, B31), and refuse deferrable constraints too until someone needs them.
4. **Ship before answering a write, or after.** Before costs about 10 ms more per write (part C). The guarantee "a change is readable in the same request or the next" holds either way, because every history read ships first. Recommend: answer, then ship in the background under the request's owner task, and also ship before reads and at start.
5. **Trusted extensions into `vm`.** Recommend: refuse the Version (the check already names it); no harm was found, but `vm` should hold only the platform's.
6. **A quota on history per Application** in the platform database. Recommend: a stated bound, refusing a migration or write that would pass it, decided with ADR 0005's amendment.
7. **`json`/`jsonb` values in history reads**: accept `json` normalization (as today), and fix JSON `null` by reading those columns with `->` (recommend), or document both.

## 10. Still unknown

- Whether some Postgres operation not tried rewrites capture triggers' `pg_trigger` tuples as a side effect (it would refuse falsely, not let through).
- The chained deferred-trigger case of B3, and deferred unique rechecks running user code at commit, were reasoned, not run.
- Enum `RENAME VALUE`, `ATTACH`/`DETACH PARTITION`, `INHERIT` detection by the structure digest: not run.
- PG 18 virtual generated columns in transition tables and in `to_jsonb`: not looked at.
- Throughput on a dedicated machine; the numbers here are from one noisy shared VM.

## 11. F. Code a migration leaves behind (added 2026-11-13 by the driving session, in review of the first slice)

Provenance of this part: written by the implementation agent of ticket #57's second slice and rerun by it after the third round of review, on Postgres 18 (the black-box harness's, `VM_TEST_PG_VERSION=18`), on a shared 12-CPU VM with nothing else of note running. The figures below are from the rerun; its whole output is `g-cost.log` (exit 0); the first run's log is in this file's history. The script `docs/research/change-history-out-of-reach/g-cost.sh` needs only cargo, this checkout and the harness's cached Postgres binaries (and `.tools/libclang`, which it fills with `scripts/fetch-libclang.sh` where it is missing): it checks out each commit in a git worktree of its own, builds its `vm` with `cargo build --release` into `target/g-cost`, and drives each through the black-box harness (`g_cost.rs`, copied into `blackbox/tests/` for the run and removed after it, as are the worktrees). The builds are **before** (`4d6df80`, the first slice), **middle** (`bc0d37f`, the second slice before a migration's statement and the question of its transaction's key travelled together) and **after** (`f30046e`, the second slice after the third round of review). Run with `target/g-cost` absent beforehand (for `5fdf806`, one commit earlier), the script took 103 s, 66 s of it building the three binaries, other build caches of this machine such as cargo's registry being warm [verified, that run's output, not kept]; the run in `g-cost.log`, with `target/g-cost` kept from it, took 60 s, 31 s of it building [verified, `g-cost.log`]. Each figure below is from that log [verified]; what follows from them is marked.

**A write** (one `INSERT` through `execute`, from the HTTP request to its answer, 400 in a row after 30 to warm up), median:

| | round 1 | round 2 |
|---|---|---|
| before | 6.38 ms | 6.30 ms |
| middle | 6.58 ms | 6.62 ms |
| after | 6.70 ms | 6.67 ms |

The check before the commit (`SELECT vm.before_commit(); SET CONSTRAINTS ALL IMMEDIATE; CLOSE ALL; SELECT vm.before_commit(); COMMIT` in one flight) is no extra round trip. Measured alone as the superuser, a transaction with one call of `vm.before_commit()` took 107 us more than one with `SELECT 1` in its place [verified]; the commit makes two calls, so about 0.21 ms [reasoned]. After was 0.32 and 0.37 ms slower than before end to end [verified]; the rest of that difference is no larger than what one build varied between the two rounds of the first run (before: 6.36 and 6.94 ms) [verified, both logs], so it cannot be told apart from the noise of a whole request here. A write that also makes the 30-second comparison (at most one every 30 s per own database) pays for a connection as the platform's role and the comparison besides, about what a Version with nothing to apply costs below [reasoned, not measured on a write].

**One statement of a migration**, as sent: the difference between a Version whose last migration is 301 `SELECT 1` and one whose last migration is one, over 300, medians of five:

| | round 1 | round 2 |
|---|---|---|
| before | 0.324 ms | 0.324 ms |
| middle | 0.320 ms | 0.325 ms |
| after | 0.193 ms | 0.197 ms |

Before, each statement was prepared and then executed (two round trips) and `vm.transaction_key()` after it likewise (two more). After, the statement and the question each go as one unnamed extended-protocol exchange, both sent before either is answered [from code]; a statement that fails makes the question after it fail, and the statement's own error is the one answered. The question can travel with the statement, and now does. A Version with a migration of one statement became slower (21.9 to 22.2 ms before, 28.8 ms after in both rounds) [verified]. The comparison before the migrations (a connection as the platform's role and the reads below) and the pending structure kept in the platform database before the commit account for it [reasoned].

**`vm.capture_guard()`**, which every captured statement that changed rows calls: 128.2 us a call against 93.3 us for a `SELECT 1` round trip, so 35 us more per captured statement, the call itself included [verified]. It was not removed. Inlining it into `vm.capture()` would save only the call's overhead, and the check of `pg_cast` itself is needed while a cast to `json` can be made outside a migration [reasoned].

**The checks and the comparison by number of tables** (ten columns each, the capture on each), each statement run 50 times by the harness's superuser after `SET ROLE` to the migration role:

| tables | `capture_state` | `capture_problems` | `structure_state` | a Version with nothing to apply (median of 5) |
|---|---|---|---|---|
| 10 | 0.25 ms | 0.95 ms | 0.31 ms | 8.4 ms |
| 100 | 0.73 ms | 1.30 ms | 1.28 ms | 10.2 ms |
| 500 | 2.63 ms | 2.89 ms | 5.28 ms | 17.8 ms |

A single SELECT 1 round trip took 0.09 to 0.10 ms [verified]. A Version with nothing to apply includes the whole stored comparison: a connection as the platform's role, `vm.structure_state()` and the record of migrations in one snapshot, and what is kept read from the platform database [from code]. A Version with migrations pays, per migration, `capture_state` twice and `capture_problems` once, and, once per Version, `structure_state` and the pending structure written to the platform database [from code]. At 500 tables that is about 8 ms a migration and 6 ms a Version [reasoned from the table].

[not verified]: the figures on Postgres 16 and 17, and with other sessions writing at the same time; `pg_locks`, which `vm.before_commit()` reads, grows with the instance's sessions (part F).
