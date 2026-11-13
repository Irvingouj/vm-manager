-- The platform's schema `vm` in an Application's own database
-- (blackbox/CONTRACT.md, "Identity and Change history in the own
-- database", and "The platform's part of an own database is out of a
-- migration's reach"; ADR 0017). Run by the platform's own role for the
-- database (`<p>_app_<n>_platform`), which owns the schema and everything
-- in it, in a transaction, whenever the own database is made or brought to
-- the present form (crate::own_db): every statement is idempotent, so an
-- own database made before any of this gets it whole, and one that has it
-- is left as it is. The grants to the runtime role and to the migration
-- role follow it (crate::own_db), since they name the roles.
--
-- Every function pins its search path to the catalog (then nothing: no
-- role of the Application can make a temporary object), and names the
-- platform's own objects with `vm.`, so that no name a migration or
-- Application SQL could make is found first. No function here reads a
-- table of the Application's, and none calls a function of the
-- Application's: the capture reads only its transition tables and the
-- catalog, and refuses to convert rows while a user-made cast to json
-- exists (Postgres would call it inside `to_jsonb`).
--
-- What the migration role may call is called by the host on the migration
-- role's own session, inside the migrations' transaction (only that
-- session sees its uncommitted catalog). A migration may call them as
-- well; it gains nothing by it: they report on the catalog, put the
-- platform's own capture on its tables, or read the names of migrations
-- already recorded, and recording one needs an identity only the host can
-- attach.

CREATE SCHEMA IF NOT EXISTS vm;
REVOKE ALL ON SCHEMA vm FROM PUBLIC;

-- Which migrations have been applied (ticket "An Application's own
-- database"). Written only by vm.record_migration().
CREATE TABLE IF NOT EXISTS vm.migrations (
    position integer PRIMARY KEY,
    name text NOT NULL UNIQUE,
    sha256 text NOT NULL,
    sql text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now()
);

-- The platform's secret: two keys of 32 bytes, made here from the
-- server's strong random source (four version 4 UUIDs, 488 random bits,
-- through sha256), so that neither ever travels in a statement. One row,
-- made once. Only the platform's role reads it: the host, once per own
-- database and program run (a result, which Postgres does not log), and
-- the functions below.
CREATE TABLE IF NOT EXISTS vm.keys (
    one boolean PRIMARY KEY DEFAULT true CHECK (one),
    k1 bytea NOT NULL CHECK (length(k1) = 32),
    k2 bytea NOT NULL CHECK (length(k2) = 32)
);
INSERT INTO vm.keys (k1, k2)
SELECT pg_catalog.sha256(pg_catalog.convert_to(
           pg_catalog.gen_random_uuid()::text || pg_catalog.gen_random_uuid()::text
           || pg_catalog.gen_random_uuid()::text || pg_catalog.gen_random_uuid()::text, 'UTF8')),
       pg_catalog.sha256(pg_catalog.convert_to(
           pg_catalog.gen_random_uuid()::text || pg_catalog.gen_random_uuid()::text
           || pg_catalog.gen_random_uuid()::text || pg_catalog.gen_random_uuid()::text, 'UTF8'))
ON CONFLICT (one) DO NOTHING;

-- The Change history.
CREATE TABLE IF NOT EXISTS vm.history (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at timestamptz NOT NULL,
    schema_name text NOT NULL,
    table_name text NOT NULL,
    kind text NOT NULL,
    key jsonb,
    old jsonb,
    new jsonb,
    changed jsonb,
    user_id text NOT NULL,
    application text NOT NULL,
    actor text NOT NULL,
    request text,
    migration text
);

-- A row's changes, newest first (the SDK's `history(table, key)`, ticket
-- "Read a row's Change history through the SDK"). The key is indexed by its
-- hash, not itself: a btree entry has a size limit, and a long key in it
-- would make the change itself fail. The reader compares the key too.
CREATE INDEX IF NOT EXISTS history_row ON vm.history
    (schema_name, table_name, pg_catalog.jsonb_hash_extended(key, 0), id);

-- What makes the transaction it is called in different from every other:
-- the backend, the virtual transaction id (unique in the backend's slot
-- until the server restarts) and the transaction's start, in
-- microseconds since the epoch (so not printed by the session's time
-- zone). An attached identity is signed over it; the host also asks it
-- after every statement of a migration, so that a transaction ended and
-- begun again behind its back is seen.
CREATE OR REPLACE FUNCTION vm.transaction_key() RETURNS text
LANGUAGE sql VOLATILE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
SELECT pg_catalog.pg_backend_pid()::text
    || '/' || coalesce((SELECT l.virtualtransaction FROM pg_catalog.pg_locks l
                         WHERE l.locktype = 'virtualxid'
                           AND l.pid = pg_catalog.pg_backend_pid()
                           AND l.virtualxid = l.virtualtransaction
                         LIMIT 1), '-')
    || '/' || ((EXTRACT(epoch FROM pg_catalog.transaction_timestamp()) * 1000000)::bigint)::text
$f$;

-- The identity attached to this transaction, as the host wrote it: the
-- setting `vm.ctx` is 64 hex digits of a tag, then the identity as JSON;
-- the tag is sha256(k2 || sha256(k1 || 'vm.ctx v1' \n transaction key \n
-- identity)). Raises VM001 unless the tag is the one the keys give for
-- this transaction: nothing set, anything made up, anything set in
-- another transaction.
CREATE OR REPLACE FUNCTION vm.attached() RETURNS jsonb
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
DECLARE
    v_ctx text := pg_catalog.current_setting('vm.ctx', true);
    v_k1 bytea;
    v_k2 bytea;
    v_identity text;
BEGIN
    IF v_ctx IS NULL OR pg_catalog.length(v_ctx) <= 64 THEN
        RAISE EXCEPTION USING ERRCODE = 'VM001',
            MESSAGE = 'no verifiable identity is attached to this transaction';
    END IF;
    SELECT k.k1, k.k2 INTO v_k1, v_k2 FROM vm.keys k;
    IF v_k1 IS NULL OR v_k2 IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'VM001',
            MESSAGE = 'no verifiable identity is attached to this transaction';
    END IF;
    v_identity := pg_catalog.substr(v_ctx, 65);
    IF pg_catalog.encode(pg_catalog.sha256(v_k2 || pg_catalog.sha256(v_k1 || pg_catalog.convert_to(
           'vm.ctx v1' || pg_catalog.chr(10) || vm.transaction_key() || pg_catalog.chr(10) || v_identity,
           'UTF8'))), 'hex')
       IS DISTINCT FROM pg_catalog.substr(v_ctx, 1, 64) THEN
        RAISE EXCEPTION USING ERRCODE = 'VM001',
            MESSAGE = 'no verifiable identity is attached to this transaction';
    END IF;
    RETURN v_identity::jsonb;
END
$f$;

-- Who the transaction is for (blackbox/CONTRACT.md, "Identity").
CREATE OR REPLACE FUNCTION vm.identity()
RETURNS TABLE (user_id text, application text, actor text, request text)
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
DECLARE
    v_who jsonb := vm.attached();
BEGIN
    user_id := v_who ->> 'user_id';
    application := v_who ->> 'application';
    actor := v_who ->> 'actor';
    request := v_who ->> 'request';
    RETURN NEXT;
END
$f$;

-- Refuses (VM003) while a cast from a type of the Application's to json
-- or jsonb exists whose function is not written in C or internal (only a
-- superuser, or a trusted extension's script, makes those): `to_jsonb`
-- would call it inside the capture, with the platform's rights.
CREATE OR REPLACE FUNCTION vm.capture_guard() RETURNS void
LANGUAGE plpgsql STABLE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
BEGIN
    IF EXISTS (SELECT FROM pg_catalog.pg_cast c
                 JOIN pg_catalog.pg_proc p ON p.oid = c.castfunc
                 JOIN pg_catalog.pg_language l ON l.oid = p.prolang
                WHERE c.casttarget IN ('pg_catalog.json'::pg_catalog.regtype,
                                       'pg_catalog.jsonb'::pg_catalog.regtype)
                  AND c.castsource >= 16384
                  AND l.lanname NOT IN ('c', 'internal')) THEN
        RAISE EXCEPTION USING ERRCODE = 'VM003',
            MESSAGE = 'a cast to json or jsonb from a type of the Application''s exists: the Change history capture refuses to run it, so no captured table can be changed until the cast is dropped';
    END IF;
END
$f$;

-- The capture: an AFTER ... FOR EACH STATEMENT trigger for each of
-- insert, update and delete, with the statement's rows as transition
-- tables (`vm_new`, `vm_old`). The identity is verified once per
-- statement that changed rows; then one history row per changed row is
-- written, in the order the rows were changed. A statement that changed
-- no row records nothing and needs no identity. An update's old and new
-- rows are paired in the order Postgres put them into its two transition
-- tables, which is the order it updated them in, one old and one new each.
--
-- A row is turned into JSON whole, as `to_jsonb(vm_row.*)`: a column
-- named like the alias (`t`, `vm_row`, `vm_new`, `vm_old`) cannot stand
-- for it. The settings that change how values print are pinned, so what
-- is recorded does not depend on the writer's session.
CREATE OR REPLACE FUNCTION vm.capture() RETURNS trigger
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
SET TimeZone = 'UTC'
SET extra_float_digits = 1
SET IntervalStyle = 'postgres'
SET DateStyle = 'ISO, YMD'
SET bytea_output = 'hex'
AS $f$
DECLARE
    v_who jsonb;
    v_at timestamptz;
    v_key text[];
    v_columns text[];
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NOT EXISTS (SELECT FROM vm_new) THEN
            RETURN NULL;
        END IF;
    ELSIF NOT EXISTS (SELECT FROM vm_old) THEN
        RETURN NULL;
    END IF;
    v_who := vm.attached();
    PERFORM vm.capture_guard();
    v_at := pg_catalog.clock_timestamp();
    SELECT pg_catalog.array_agg(a.attname::text ORDER BY pg_catalog.array_position(i.indkey::int2[], a.attnum))
      INTO v_key
      FROM pg_catalog.pg_index i
      JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey)
     WHERE i.indrelid = TG_RELID AND i.indisprimary;
    IF TG_OP = 'INSERT' THEN
        INSERT INTO vm.history (at, schema_name, table_name, kind, key, old, new, changed,
                                user_id, application, actor, request, migration)
        SELECT v_at, TG_TABLE_SCHEMA::text, TG_TABLE_NAME::text, 'insert',
               CASE WHEN v_key IS NULL THEN NULL
                    ELSE (SELECT pg_catalog.jsonb_object_agg(c, r.j -> c) FROM pg_catalog.unnest(v_key) c) END,
               NULL, r.j, NULL,
               v_who ->> 'user_id', v_who ->> 'application', v_who ->> 'actor',
               v_who ->> 'request', v_who ->> 'migration'
          FROM (SELECT pg_catalog.to_jsonb(vm_row.*) AS j, pg_catalog.row_number() OVER () AS i
                  FROM vm_new vm_row) r
         ORDER BY r.i;
    ELSIF TG_OP = 'DELETE' THEN
        INSERT INTO vm.history (at, schema_name, table_name, kind, key, old, new, changed,
                                user_id, application, actor, request, migration)
        SELECT v_at, TG_TABLE_SCHEMA::text, TG_TABLE_NAME::text, 'delete',
               CASE WHEN v_key IS NULL THEN NULL
                    ELSE (SELECT pg_catalog.jsonb_object_agg(c, r.j -> c) FROM pg_catalog.unnest(v_key) c) END,
               r.j, NULL, NULL,
               v_who ->> 'user_id', v_who ->> 'application', v_who ->> 'actor',
               v_who ->> 'request', v_who ->> 'migration'
          FROM (SELECT pg_catalog.to_jsonb(vm_row.*) AS j, pg_catalog.row_number() OVER () AS i
                  FROM vm_old vm_row) r
         ORDER BY r.i;
    ELSE
        SELECT pg_catalog.array_agg(a.attname::text ORDER BY a.attnum)
          INTO v_columns
          FROM pg_catalog.pg_attribute a
         WHERE a.attrelid = TG_RELID AND a.attnum > 0 AND NOT a.attisdropped;
        INSERT INTO vm.history (at, schema_name, table_name, kind, key, old, new, changed,
                                user_id, application, actor, request, migration)
        SELECT v_at, TG_TABLE_SCHEMA::text, TG_TABLE_NAME::text, 'update',
               CASE WHEN v_key IS NULL THEN NULL
                    ELSE (SELECT pg_catalog.jsonb_object_agg(c, o.j -> c) FROM pg_catalog.unnest(v_key) c) END,
               o.j, n.j,
               (SELECT coalesce(pg_catalog.jsonb_agg(u.c ORDER BY u.p), '[]'::jsonb)
                  FROM pg_catalog.unnest(v_columns) WITH ORDINALITY u(c, p)
                 WHERE (o.j -> u.c) IS DISTINCT FROM (n.j -> u.c)),
               v_who ->> 'user_id', v_who ->> 'application', v_who ->> 'actor',
               v_who ->> 'request', v_who ->> 'migration'
          FROM (SELECT pg_catalog.to_jsonb(vm_row.*) AS j, pg_catalog.row_number() OVER () AS i
                  FROM vm_old vm_row) o
          JOIN (SELECT pg_catalog.to_jsonb(vm_row.*) AS j, pg_catalog.row_number() OVER () AS i
                  FROM vm_new vm_row) n
            ON n.i = o.i
         ORDER BY o.i;
    END IF;
    RETURN NULL;
END
$f$;

-- The fourth capture trigger's function: TRUNCATE of a table with history
-- is refused, whoever runs it, also through CASCADE and through a
-- partition (each truncated table fires its own).
CREATE OR REPLACE FUNCTION vm.capture_truncate() RETURNS trigger
LANGUAGE plpgsql VOLATILE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
BEGIN
    RAISE EXCEPTION USING ERRCODE = 'VM002',
        MESSAGE = pg_catalog.format(
            'TRUNCATE of %I.%I is refused: the rows it removes would leave no Change history. Use DELETE FROM %I.%I instead, which is recorded',
            TG_TABLE_SCHEMA, TG_TABLE_NAME, TG_TABLE_SCHEMA, TG_TABLE_NAME);
END
$f$;

-- Puts the four capture triggers on every table, in any schema but the
-- platform's and the catalog's, that does not have them yet: partitioned
-- tables and partitions both, since a statement fires the statement
-- triggers of the table it names only. Runs with the platform's role's
-- rights, which may make triggers on the migration role's tables through
-- its inheriting membership; a table of another role's (a superuser made
-- it there) is left as it is, and `vm.capture_problems()` names it.
-- Called by the host after each migration, inside the migrations'
-- transaction, and when the capture is put back; answers how many
-- triggers it made.
CREATE OR REPLACE FUNCTION vm.capture_new_tables() RETURNS integer
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
DECLARE
    v_table record;
    v_made integer := 0;
BEGIN
    FOR v_table IN
        SELECT c.oid::pg_catalog.regclass AS relation, e.event, e.timing, e.referencing, e.fn
          FROM pg_catalog.pg_class c
          JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         CROSS JOIN (VALUES ('insert', 'AFTER', 'REFERENCING NEW TABLE AS vm_new', 'vm.capture()'),
                            ('update', 'AFTER', 'REFERENCING OLD TABLE AS vm_old NEW TABLE AS vm_new', 'vm.capture()'),
                            ('delete', 'AFTER', 'REFERENCING OLD TABLE AS vm_old', 'vm.capture()'),
                            ('truncate', 'BEFORE', '', 'vm.capture_truncate()'))
               AS e(event, timing, referencing, fn)
         WHERE c.relkind IN ('r', 'p')
           AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
           AND pg_catalog.left(n.nspname, 3) <> 'pg_'
           AND pg_catalog.pg_has_role(c.relowner, 'USAGE')
           AND NOT EXISTS (SELECT FROM pg_catalog.pg_trigger t
                            WHERE t.tgrelid = c.oid AND t.tgname = 'vm_capture_' || e.event)
         ORDER BY c.oid, e.event
    LOOP
        EXECUTE pg_catalog.format(
            'CREATE TRIGGER %I %s %s ON %s %s FOR EACH STATEMENT EXECUTE FUNCTION %s',
            'vm_capture_' || v_table.event, v_table.timing, pg_catalog.upper(v_table.event),
            v_table.relation, v_table.referencing, v_table.fn);
        v_made := v_made + 1;
    END LOOP;
    RETURN v_made;
END
$f$;

-- Whether the table `p_table` has its whole capture: the four triggers as
-- `vm.capture_new_tables()` makes them, enabled, and no other trigger
-- calling a function of `vm`.
CREATE OR REPLACE FUNCTION vm.capture_whole(p_table oid) RETURNS boolean
LANGUAGE sql STABLE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
SELECT NOT EXISTS (
    SELECT FROM (VALUES
        ('vm_capture_insert', 4, 'vm.capture()'::pg_catalog.regprocedure, NULL, 'vm_new'),
        ('vm_capture_update', 16, 'vm.capture()'::pg_catalog.regprocedure, 'vm_old', 'vm_new'),
        ('vm_capture_delete', 8, 'vm.capture()'::pg_catalog.regprocedure, 'vm_old', NULL),
        ('vm_capture_truncate', 34, 'vm.capture_truncate()'::pg_catalog.regprocedure, NULL, NULL))
        AS w(name, tgtype, fn, oldt, newt)
     WHERE NOT EXISTS (
        SELECT FROM pg_catalog.pg_trigger g
         WHERE g.tgrelid = p_table AND g.tgname = w.name
           AND g.tgtype = w.tgtype AND g.tgfoid = w.fn
           AND g.tgenabled IN ('O', 'A') AND g.tgqual IS NULL
           AND g.tgnargs = 0 AND coalesce(pg_catalog.array_length(g.tgattr::int2[], 1), 0) = 0
           AND NOT g.tgdeferrable AND g.tgconstraint = 0
           AND g.tgoldtable IS NOT DISTINCT FROM w.oldt
           AND g.tgnewtable IS NOT DISTINCT FROM w.newt))
   AND NOT EXISTS (
    SELECT FROM pg_catalog.pg_trigger g
      JOIN pg_catalog.pg_proc p ON p.oid = g.tgfoid
     WHERE g.tgrelid = p_table AND p.pronamespace = 'vm'::pg_catalog.regnamespace
       AND g.tgname NOT LIKE 'vm\_capture\_%')
$f$;

-- Puts the capture back whole (blackbox/CONTRACT.md, "What something else
-- changed"): on every table of the migration role's without its whole
-- capture, every trigger calling a function of `vm` is dropped and the
-- four are made again (so one found disabled, missing, renamed or
-- replaced is as the platform makes it). Answers the tables the platform
-- may not put the capture on (another role's) that are without it.
-- Called by the host as the platform's role, outside any migration.
CREATE OR REPLACE FUNCTION vm.restore_capture() RETURNS TABLE (uncaptured text)
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
DECLARE
    v_trigger record;
BEGIN
    FOR v_trigger IN
        SELECT g.tgname, g.tgrelid::pg_catalog.regclass AS relation
          FROM pg_catalog.pg_trigger g
          JOIN pg_catalog.pg_proc p ON p.oid = g.tgfoid
          JOIN pg_catalog.pg_class c ON c.oid = g.tgrelid
          JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         WHERE p.pronamespace = 'vm'::pg_catalog.regnamespace AND NOT g.tgisinternal
           AND c.relkind IN ('r', 'p')
           AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
           AND pg_catalog.left(n.nspname, 3) <> 'pg_'
           AND pg_catalog.pg_has_role(c.relowner, 'USAGE')
           AND NOT vm.capture_whole(c.oid)
         ORDER BY g.tgrelid, g.tgname
    LOOP
        EXECUTE pg_catalog.format('DROP TRIGGER %I ON %s', v_trigger.tgname, v_trigger.relation);
    END LOOP;
    PERFORM vm.capture_new_tables();
    RETURN QUERY
        SELECT n.nspname::text || '.' || c.relname::text
          FROM pg_catalog.pg_class c
          JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relkind IN ('r', 'p')
           AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
           AND pg_catalog.left(n.nspname, 3) <> 'pg_'
           AND NOT vm.capture_whole(c.oid)
         ORDER BY 1;
END
$f$;

-- What the structure and the capture of every table outside `vm` are, as
-- a digest per table (blackbox/CONTRACT.md, "What something else
-- changed"; research part F): the values of its `pg_class` row that make
-- it what it is (object id, name, schema, owner, kind, persistence,
-- partition), the object id and `xmin` of each of its `pg_attribute` and
-- `pg_trigger` rows, and its `pg_inherits` rows. Ordinary maintenance
-- (`VACUUM`, `VACUUM FULL`, `CLUSTER`, `REINDEX`, `ANALYZE`) changes none of
-- these; disabling, enabling, dropping, renaming or making a trigger, and
-- adding, dropping or retyping a column, change one. Neither `ctid` nor
-- `pg_class`'s `xmin` is used: maintenance moves them. The host keeps the
-- answer in the platform database and compares.
CREATE OR REPLACE FUNCTION vm.structure_state() RETURNS TABLE (table_name text, digest text)
LANGUAGE sql STABLE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
SELECT n.nspname::text || '.' || c.relname::text,
       pg_catalog.encode(pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.concat_ws('|',
           c.oid::text, c.relname::text, n.nspname::text, c.relowner::text, c.relkind::text,
           c.relpersistence::text, c.relispartition::text,
           (SELECT pg_catalog.string_agg(a.attnum::text || ':' || a.xmin::text, ',' ORDER BY a.attnum)
              FROM pg_catalog.pg_attribute a WHERE a.attrelid = c.oid),
           (SELECT pg_catalog.string_agg(g.oid::text || ':' || g.xmin::text, ',' ORDER BY g.oid)
              FROM pg_catalog.pg_trigger g WHERE g.tgrelid = c.oid),
           (SELECT pg_catalog.string_agg(i.inhparent::text || ':' || i.inhseqno::text, ',' ORDER BY i.inhseqno)
              FROM pg_catalog.pg_inherits i WHERE i.inhrelid = c.oid)), 'UTF8')), 'hex')
  FROM pg_catalog.pg_class c
  JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
 WHERE c.relkind IN ('r', 'p')
   AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
   AND pg_catalog.left(n.nspname, 3) <> 'pg_'
 ORDER BY 1
$f$;

-- The check before the commit of every read-write transaction the host
-- runs on the own database (blackbox/CONTRACT.md, "Checked before every
-- commit that could carry a change"; ADR 0017; research part F): raises
-- VM004 while this session holds a lock on a relation of a mode that
-- reading and writing rows never takes (`SHARE UPDATE EXCLUSIVE` or
-- stronger). Every change of a table's structure or of the capture takes
-- one and keeps it until the transaction ends; one made in a
-- subtransaction that was rolled back was let go with it, and so was what
-- it changed. The host sends it in the same simple query as the `COMMIT`,
-- after `SET CONSTRAINTS ALL IMMEDIATE` and `CLOSE ALL`, so a raised error
-- turns the commit into a rollback and nothing runs in between. The
-- runtime role may call it itself, to no end: it reads the lock manager's
-- own view of this session, and it cannot be replaced (the platform's
-- role owns it and `vm`).
CREATE OR REPLACE FUNCTION vm.before_commit() RETURNS void
LANGUAGE plpgsql VOLATILE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
BEGIN
    IF EXISTS (SELECT FROM pg_catalog.pg_locks l
                WHERE l.locktype = 'relation'
                  AND l.pid = pg_catalog.pg_backend_pid()
                  AND l.mode IN ('ShareUpdateExclusiveLock', 'ShareLock', 'ShareRowExclusiveLock',
                                 'ExclusiveLock', 'AccessExclusiveLock')) THEN
        RAISE EXCEPTION USING ERRCODE = 'VM004',
            MESSAGE = 'the transaction changed the structure of a table or the Change history capture, or locked, analyzed or refreshed a table, which only a migration may: an Application''s structure changes in migrations only. Everything the transaction did was rolled back';
    END IF;
END
$f$;

-- Which catalog rows the capture triggers of every table are: one row per
-- table outside `vm`, with its capture triggers (those named like the
-- platform's; another trigger calling a function of `vm` is
-- `capture-missing`, below) as their names and the
-- identity of their `pg_trigger` tuples (object id, `xmin`, `ctid`).
-- Disabling, enabling, renaming, replacing or dropping one gives another
-- tuple, also when it is put back, so the host compares what this answers
-- before and after each migration (kept in the host's memory, not here).
CREATE OR REPLACE FUNCTION vm.capture_state()
RETURNS TABLE (table_oid oid, table_name text, triggers text)
LANGUAGE sql STABLE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
SELECT c.oid, c.oid::pg_catalog.regclass::text,
       coalesce((SELECT pg_catalog.string_agg(
                            pg_catalog.format('%s|%s|%s|%s|%s', g.tgname, g.oid, g.xmin, g.ctid, g.tgenabled),
                            ';' ORDER BY g.oid)
                   FROM pg_catalog.pg_trigger g
                  WHERE g.tgrelid = c.oid
                    AND g.tgname LIKE 'vm\_capture\_%'), '')
  FROM pg_catalog.pg_class c
  JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
 WHERE c.relkind IN ('r', 'p')
   AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
   AND pg_catalog.left(n.nspname, 3) <> 'pg_'
$f$;

-- Whether the object `p_object` of the catalog `p_catalog` is code of the
-- Application's for `index-code`: neither Postgres's own (made by initdb,
-- so with an object id below 16384, FirstNormalObjectId) nor a member of
-- an extension (a `pg_depend` row of kind 'e' to it; a trusted extension's
-- members belong to the bootstrap superuser, an untrusted one's needed a
-- superuser to make). Whoever owns it: the migration role, or the runtime
-- role where a migration let it create.
CREATE OR REPLACE FUNCTION vm.application_code(p_catalog oid, p_object oid) RETURNS boolean
LANGUAGE sql STABLE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
SELECT p_object >= 16384::pg_catalog.oid
   AND NOT EXISTS (SELECT FROM pg_catalog.pg_depend e
                    WHERE e.classid = p_catalog AND e.objid = p_object
                      AND e.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
                      AND e.deptype = 'e')
$f$;

-- What the platform refuses at the end of a migration, one row per
-- finding: the rule (as the answer names it) and what was found, where,
-- and what to do instead. No row: nothing to refuse. Called on the
-- migration role's own session: "the migrations wrote" is "the session's
-- user owns" (`SESSION_USER`, which no statement of a non-superuser
-- changes), so an extension's functions (owned by the bootstrap
-- superuser) are not counted.
CREATE OR REPLACE FUNCTION vm.capture_problems()
RETURNS TABLE (rule text, message text)
LANGUAGE sql STABLE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
WITH tabs AS (
    SELECT c.oid FROM pg_catalog.pg_class c
      JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
     WHERE c.relkind IN ('r', 'p')
       AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
       AND pg_catalog.left(n.nspname, 3) <> 'pg_'),
want(name, tgtype, fn, oldt, newt) AS (VALUES
    ('vm_capture_insert', 4, 'vm.capture()'::pg_catalog.regprocedure, NULL, 'vm_new'),
    ('vm_capture_update', 16, 'vm.capture()'::pg_catalog.regprocedure, 'vm_old', 'vm_new'),
    ('vm_capture_delete', 8, 'vm.capture()'::pg_catalog.regprocedure, 'vm_old', NULL),
    ('vm_capture_truncate', 34, 'vm.capture_truncate()'::pg_catalog.regprocedure, NULL, NULL)),
vm_functions AS (
    SELECT p.oid FROM pg_catalog.pg_proc p WHERE p.pronamespace = 'vm'::pg_catalog.regnamespace)
SELECT 'capture-missing',
       pg_catalog.format('the table %s is without its whole Change history capture: its trigger %s is missing, disabled or altered. The platform puts its capture triggers (named vm_capture_...) on every table; leave them as they are: do not drop, disable, rename or replace them', t.oid::pg_catalog.regclass, w.name)
  FROM tabs t CROSS JOIN want w
 WHERE NOT EXISTS (
    SELECT FROM pg_catalog.pg_trigger g
     WHERE g.tgrelid = t.oid AND g.tgname = w.name
       AND g.tgtype = w.tgtype AND g.tgfoid = w.fn
       AND g.tgenabled IN ('O', 'A') AND g.tgqual IS NULL
       AND g.tgnargs = 0 AND coalesce(pg_catalog.array_length(g.tgattr::int2[], 1), 0) = 0
       AND NOT g.tgdeferrable AND g.tgconstraint = 0
       AND g.tgoldtable IS NOT DISTINCT FROM w.oldt
       AND g.tgnewtable IS NOT DISTINCT FROM w.newt)
UNION ALL
SELECT 'capture-missing',
       pg_catalog.format('the trigger %s on %s calls the platform''s function %s and is not one of its capture triggers: drop it; a trigger of the Application''s calls a function of the Application''s',
                         g.tgname, g.tgrelid::pg_catalog.regclass, g.tgfoid::pg_catalog.regprocedure)
  FROM pg_catalog.pg_trigger g
 WHERE g.tgfoid IN (SELECT oid FROM vm_functions)
   AND (g.tgname NOT LIKE 'vm\_capture\_%' OR g.tgrelid NOT IN (SELECT oid FROM tabs))
UNION ALL
SELECT 'deferrable',
       pg_catalog.format('the constraint %s on %s is DEFERRABLE: its work would run at the commit, after the platform''s checks. Declare it NOT DEFERRABLE (the default)',
                         co.conname, co.conrelid::pg_catalog.regclass)
  FROM pg_catalog.pg_constraint co
  JOIN pg_catalog.pg_namespace n ON n.oid = co.connamespace
 WHERE co.condeferrable AND co.conrelid <> 0
   AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
   AND pg_catalog.left(n.nspname, 3) <> 'pg_'
UNION ALL
SELECT 'deferrable',
       pg_catalog.format('the trigger %s on %s is DEFERRABLE: its work would run at the commit, after the platform''s checks. Make it an ordinary trigger, or a constraint trigger NOT DEFERRABLE',
                         g.tgname, g.tgrelid::pg_catalog.regclass)
  FROM pg_catalog.pg_trigger g
 WHERE g.tgdeferrable AND NOT g.tgisinternal AND g.tgconstraint = 0
UNION ALL
SELECT 'json-cast',
       pg_catalog.format('the cast from %s to %s runs the function %s, which Postgres would call inside the Change history capture with the platform''s rights: drop the cast (DROP CAST (%s AS %s)) and call the function where the conversion is wanted',
                         c.castsource::pg_catalog.regtype, c.casttarget::pg_catalog.regtype,
                         c.castfunc::pg_catalog.regprocedure,
                         c.castsource::pg_catalog.regtype, c.casttarget::pg_catalog.regtype)
  FROM pg_catalog.pg_cast c
  JOIN pg_catalog.pg_proc p ON p.oid = c.castfunc
  JOIN pg_catalog.pg_language l ON l.oid = p.prolang
 WHERE c.casttarget IN ('pg_catalog.json'::pg_catalog.regtype, 'pg_catalog.jsonb'::pg_catalog.regtype)
   AND c.castsource >= 16384 AND l.lanname NOT IN ('c', 'internal')
UNION ALL
SELECT 'in-vm',
       pg_catalog.format('the extension %s is in the platform''s schema vm, which holds only the platform''s: create it in a schema of the Application''s (CREATE EXTENSION %s SCHEMA public)',
                         e.extname, e.extname)
  FROM pg_catalog.pg_extension e
 WHERE e.extnamespace = 'vm'::pg_catalog.regnamespace
UNION ALL
SELECT 'in-vm',
       pg_catalog.format('the %s %s in the platform''s schema vm is not the platform''s: the schema vm holds only the platform''s; make it in a schema of the Application''s', o.kind, o.name)
  FROM (SELECT 'function' AS kind, p.oid::pg_catalog.regprocedure::text AS name, p.proowner AS owner
          FROM pg_catalog.pg_proc p WHERE p.pronamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'relation', c.oid::pg_catalog.regclass::text, c.relowner
          FROM pg_catalog.pg_class c WHERE c.relnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'type', t.oid::pg_catalog.regtype::text, t.typowner
          FROM pg_catalog.pg_type t WHERE t.typnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'operator', o.oid::pg_catalog.regoperator::text, o.oprowner
          FROM pg_catalog.pg_operator o WHERE o.oprnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'operator class', oc.opcname::text, oc.opcowner
          FROM pg_catalog.pg_opclass oc WHERE oc.opcnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'operator family', of.opfname::text, of.opfowner
          FROM pg_catalog.pg_opfamily of WHERE of.opfnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'collation', co.collname::text, co.collowner
          FROM pg_catalog.pg_collation co WHERE co.collnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'conversion', cv.conname::text, cv.conowner
          FROM pg_catalog.pg_conversion cv WHERE cv.connamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'text search configuration', ts.cfgname::text, ts.cfgowner
          FROM pg_catalog.pg_ts_config ts WHERE ts.cfgnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'text search dictionary', td.dictname::text, td.dictowner
          FROM pg_catalog.pg_ts_dict td WHERE td.dictnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'statistics object', se.stxname::text, se.stxowner
          FROM pg_catalog.pg_statistic_ext se WHERE se.stxnamespace = 'vm'::pg_catalog.regnamespace) o
 WHERE o.owner <> (SELECT n.nspowner FROM pg_catalog.pg_namespace n WHERE n.nspname = 'vm')
UNION ALL
SELECT 'security-definer',
       pg_catalog.format('the %s %s is SECURITY DEFINER: Postgres would run it with the rights of the role that owns the Application''s tables, which can switch the Change history capture off, whoever calls it. The runtime role may already read and write every table the migrations make: declare it SECURITY INVOKER (the default)',
                         CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END,
                         p.oid::pg_catalog.regprocedure)
  FROM pg_catalog.pg_proc p
 WHERE p.prosecdef AND pg_catalog.pg_get_userbyid(p.proowner) = SESSION_USER
UNION ALL
SELECT 'index-code',
       pg_catalog.format('the %s %s calls %s, which is neither Postgres''s own nor an extension''s: Postgres''s autovacuum would run it with the rights of the role that owns the Application''s tables, outside any transaction of the platform''s. Index or analyze Postgres''s own functions and operators only (or an extension''s), or keep the computed value in a column of its own',
                         x.what, x.name, x.called)
  FROM (SELECT DISTINCT
               CASE d.classid WHEN 'pg_catalog.pg_class'::pg_catalog.regclass THEN 'index'
                              WHEN 'pg_catalog.pg_constraint'::pg_catalog.regclass THEN 'exclusion constraint'
                              ELSE 'extended statistic' END AS what,
               CASE d.classid WHEN 'pg_catalog.pg_class'::pg_catalog.regclass
                                   THEN d.objid::pg_catalog.regclass::text
                              WHEN 'pg_catalog.pg_constraint'::pg_catalog.regclass
                                   THEN (SELECT co.conname::text FROM pg_catalog.pg_constraint co WHERE co.oid = d.objid)
                              ELSE (SELECT se.stxname::text FROM pg_catalog.pg_statistic_ext se WHERE se.oid = d.objid) END AS name,
               CASE d.refclassid WHEN 'pg_catalog.pg_proc'::pg_catalog.regclass
                                      THEN 'the function ' || d.refobjid::pg_catalog.regprocedure::text
                                 WHEN 'pg_catalog.pg_operator'::pg_catalog.regclass
                                      THEN (SELECT 'the operator ' || o.oid::pg_catalog.regoperator::text
                                                   || ' (its function ' || o.oprcode::pg_catalog.regprocedure::text || ')'
                                              FROM pg_catalog.pg_operator o WHERE o.oid = d.refobjid)
                                 WHEN 'pg_catalog.pg_cast'::pg_catalog.regclass
                                      THEN 'the cast ' || (SELECT ca.castsource::pg_catalog.regtype::text || ' AS ' || ca.casttarget::pg_catalog.regtype::text
                                                                  || ' (its function ' || ca.castfunc::pg_catalog.regprocedure::text || ')'
                                                             FROM pg_catalog.pg_cast ca WHERE ca.oid = d.refobjid)
                                 ELSE 'the domain ' || d.refobjid::pg_catalog.regtype::text END AS called
          FROM pg_catalog.pg_depend d
         WHERE (d.classid = 'pg_catalog.pg_statistic_ext'::pg_catalog.regclass
                OR (d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
                    AND EXISTS (SELECT FROM pg_catalog.pg_class i
                                 WHERE i.oid = d.objid AND i.relkind IN ('i', 'I')))
                OR (d.classid = 'pg_catalog.pg_constraint'::pg_catalog.regclass
                    AND EXISTS (SELECT FROM pg_catalog.pg_constraint co
                                 WHERE co.oid = d.objid AND co.contype = 'x')))
           AND ((d.refclassid = 'pg_catalog.pg_proc'::pg_catalog.regclass
                 AND vm.application_code('pg_catalog.pg_proc'::pg_catalog.regclass, d.refobjid))
             OR (d.refclassid = 'pg_catalog.pg_operator'::pg_catalog.regclass
                 AND EXISTS (SELECT FROM pg_catalog.pg_operator o
                              WHERE o.oid = d.refobjid
                                AND vm.application_code('pg_catalog.pg_proc'::pg_catalog.regclass, o.oprcode)))
             OR (d.refclassid = 'pg_catalog.pg_cast'::pg_catalog.regclass
                 AND EXISTS (SELECT FROM pg_catalog.pg_cast ca
                              WHERE ca.oid = d.refobjid AND ca.castfunc <> 0
                                AND vm.application_code('pg_catalog.pg_proc'::pg_catalog.regclass, ca.castfunc)))
             OR (d.refclassid = 'pg_catalog.pg_type'::pg_catalog.regclass
                 AND EXISTS (SELECT FROM pg_catalog.pg_type t WHERE t.oid = d.refobjid AND t.typtype = 'd')
                 AND vm.application_code('pg_catalog.pg_type'::pg_catalog.regclass, d.refobjid)))) x
$f$;

-- The migrations recorded, in order: what the host compares a Version's
-- list with, and reads back after recording. `sql_sha256` is computed
-- from the text kept, not taken from the record.
CREATE OR REPLACE FUNCTION vm.applied_migrations()
RETURNS TABLE (place integer, name text, sha256 text, sql_sha256 text)
LANGUAGE sql STABLE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
SELECT m.position, m.name, m.sha256,
       pg_catalog.encode(pg_catalog.sha256(pg_catalog.convert_to(m.sql, 'UTF8')), 'hex')
  FROM vm.migrations m
 ORDER BY m.position
$f$;

-- Records one migration as applied. Only with the identity the host
-- attaches for this, after the Version's last migration and its checks
-- (`actor` `platform`), which a migration cannot make: it does not have
-- the keys.
CREATE OR REPLACE FUNCTION vm.record_migration(p_position integer, p_name text, p_sql text)
RETURNS void
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
BEGIN
    IF (vm.attached() ->> 'actor') IS DISTINCT FROM 'platform' THEN
        RAISE EXCEPTION USING ERRCODE = '42501',
            MESSAGE = 'only the platform records a migration';
    END IF;
    INSERT INTO vm.migrations (position, name, sha256, sql)
    VALUES (p_position, p_name,
            pg_catalog.encode(pg_catalog.sha256(pg_catalog.convert_to(p_sql, 'UTF8')), 'hex'),
            p_sql);
END
$f$;

SELECT vm.capture_new_tables();

-- New functions are executable by PUBLIC: none is by anyone but the
-- platform's role, except as granted by name (crate::own_db).
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA vm FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA vm FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA vm FROM PUBLIC;
