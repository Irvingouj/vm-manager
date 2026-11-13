---
status: accepted
---

# The Change history is kept in the platform database, captured through an outbox the platform owns

ADR 0005 put the Change history "beside the data", and ADR 0013 gave an Application's own database one role that "owns every object and applies migrations". Together they meant that a Contributor's migration, which arrives with any Version, ran as the owner of the history: it could delete or rewrite history rows, read the keys identity is signed with, and remove the capture. The product owner decided on 2026-11-12 that the Change history must not live in a database a Contributor can migrate, and that "a change and its history row stand or fall together" is to be kept as well as it can be by other means. This ADR amends ADR 0005 and ADR 0013 accordingly. The evidence is `docs/research/change-history-out-of-reach.md`.

## The decision

**Three roles in an own database, not two.** The platform's own role for that database owns the database and the schema `vm` with everything in it: the keys, the outbox, the record of migrations, the capture and identity functions. The migration role owns only what the migrations make, and cannot log in except while the platform runs a migration. The runtime role is as before. The platform still needs only `CREATEDB` and `CREATEROLE`.

**Capture is a transactional outbox.** The capture triggers write each changed row to `vm.outbox` in the same transaction as the change, so a change and its captured row commit or roll back together, as before. The capture runs with the platform's role's rights, so it must never run code a migration wrote: it reads only transition tables, pins its settings, and refuses while a user-made cast to `json` exists (Postgres would call it inside `to_jsonb`).

**The history itself is in the platform database.** A relay in the host ships outbox rows there, at least once and idempotently (one row per Application and change id), then deletes what it shipped. It runs after every write (the write is answered first), before every read of the history, and at start. So every committed change is in the history exactly once, also across a crash or an unreachable platform database, and an Application reads its own changes. No role of any own database can connect to the platform database.

**A migration cannot take the capture away unseen.** A table's owner may disable or drop a trigger on its own table, and no role without superuser can take that right away, so the platform checks instead, before a migration's transaction commits:

- migration text is split by the host's Postgres parser and sent one statement at a time; a statement that controls the transaction is refused before it is sent, so the migration cannot commit behind the platform's back;
- the capture triggers' catalog rows are the very same rows as before the migration (a digest of their identity kept in the host's memory): disabling, dropping, renaming or replacing one changes it, also when undone;
- nothing of the migration's can still run at commit: deferred constraints are made immediate and cursors closed before the checks, and deferrable triggers and constraints are refused;
- every table has its capture whole; `vm` holds nothing but the platform's;
- `TRUNCATE` of a table with history is refused by a capture trigger of its own.

A Version that fails a check is refused and nothing it did is kept.

**Nor can what a migration leaves behind** (added 2026-11-13, part F of the research). The migration role cannot log in between migrations, but Postgres runs code with its rights without a login: a `SECURITY DEFINER` function; a trigger, a constraint or an index expression on a table while Postgres carries out a foreign key's action, analyzes the table or refreshes a materialized view, all of which it does as the table's owner. Such code could switch the capture off, change rows and switch it on again at run time. A table's owner cannot be stopped from doing that, and the ways code comes to run as the owner cannot all be listed, so the platform holds run time to the same discipline as a migration:

- every read-write transaction the host runs for an Application ends with deferred constraints made immediate, cursors closed, and a check that the structure of the Application's tables and the capture on them are the very same as when the last migration ended; one that changed them is rolled back, and with it everything it did. A read is in a read-only transaction, where Postgres refuses such a change itself;
- a migration is refused when it leaves a `SECURITY DEFINER` function, or an index or an extended statistic that calls code of the Application's (autovacuum would run it as the owner, in a transaction that is not the host's);
- the host sets the migration role's password to null as its last act in a migration's transaction, so that no password a migration gave the role is ever good;
- what something outside the host's transactions changed cannot be undone. The platform keeps the digest it checks against in the platform database, and when an own database no longer matches it, says so in the Operation log, puts the capture back and goes on.

**Changes of structure are entries of the history too.** DDL that rewrites or removes data fires no row trigger: a column's type changed with `USING`, a column or a table dropped, a table replaced under its name. The platform sees each from the catalogs before commit and records it as an entry of the table's history, with the Contributor and the migration's name. A migration that rewrites values this way additionally has each changed row recorded (its own ticket).

**Reading** is a host operation of its own, for the Application in flight only: it ships first, reads the platform database, and shapes the rows into the table's present row type in the own database, as the runtime role in a read-only transaction. The history is given to nobody the Application's own code does not give it to: it has no place in any API for Platform administrators, so that, as ADR 0005 requires, reading the Operation log gives no view of the data.

## Considered Options

- **Keep the history in `vm`, owned by the third role, without shipping.** The research found no way for the migration role to reach it. Rejected by the product owner's decision: the history's integrity would rest for ever on no privilege mistake being made in a database where a Contributor runs arbitrary DDL; outside it, a breach of an own database cannot rewrite the past.
- **Logical decoding of the write-ahead log.** It would see changes made with the triggers off, but not those to a table its owner set `UNLOGGED`, nor old values under `REPLICA IDENTITY NOTHING`, both the table owner's to set. It needs `wal_level = logical`, a role with `REPLICATION` (which a `CREATEROLE` role cannot make) and a slot per own database against a default of ten for the instance. Rejected: it breaks "a Postgres IT supplies, `CREATEDB` and `CREATEROLE`" and still needs the same catalog checks.
- **Two-phase commit across the two databases.** Needs `max_prepared_transactions` above its default of 0 and a recovery process, and buys what the outbox already gives. Rejected.
- **Refusing, when a migration is applied, every way its code could later run as the owner.** Besides `SECURITY DEFINER` these are ordinary things: a `BEFORE` trigger or a `CHECK` on a table that a cascading foreign key points from. Rejected as the only defence: it would refuse common, honest schemas, and the list cannot be shown complete. Kept for the two ways the check before commit does not make harmless or does not see.
- **A role per table, or holding locks on every table from a session of the platform's**, so that code running as one table's owner could not alter another. Rejected: a role per table for every Application in one cluster, ownership handed over inside every migration; a lock lasts only as long as a session.
- **Comparing the transaction's row counters with the captured rows**, to catch a capture switched off and on again. Rejected: the counters include earlier transactions, miscount savepoints and partitions, see neither `TRUNCATE` nor rewrites, and were defeated through an inheritance child.

## Consequences

- The platform database now holds Application data (old and new rows). It grows with every change in every own database; its capacity is planned with the instance's, like the own databases beside it.
- A write is followed by a shipment (about 10 ms in the research's set-up), after its answer. Between the commit and the shipment the captured rows are in the own database's outbox, which only the platform's role can touch.
- Application SQL no longer reads the history: `vm.history` is gone from the own database, and the SDK's `history(...)` is the way.
- Every write on an own database pays for the check before its commit, and an Application cannot change its tables' structure at run time, by any means: structure changes in migrations only.
- `SECURITY DEFINER` functions, and indexes or extended statistics that call the Application's own functions, are refused in migrations.
- After the Postgres under the platform is replaced (a restore, `pg_upgrade`), every own database is found changed once, and the Operation log says so.
- Deferrable constraints and triggers, a user-made cast to `json` or `jsonb`, an extension placed in `vm`, and `TRUNCATE` of a table with history are refused in migrations. Each refusal says what to do instead.
- Migrations are parsed by the host: text the host's parser cannot split is refused.
- What a Contributor can still do, and the history shows: change structure, and with it data, under their own name; make the Application's own triggers change rows, which are recorded under the User in flight like any change the Application makes.
- Row-level rules that trust the platform's identity (Milestone 3) keep working: `vm.identity()` and the keys stay in the own database, out of the migration role's reach.
