-- The design under test: schema `vm` of an own database, owned by the
-- third role (here a1_vm), with a transactional outbox instead of
-- vm.history. Adapted from crates/vm/src/own_db_schema.sql: identity
-- functions unchanged; the capture writes vm.outbox; a BEFORE TRUNCATE
-- trigger; the checks the platform would run before a migration commits.
-- Run as a1_vm in database a1, in one transaction.

CREATE SCHEMA vm;
REVOKE ALL ON SCHEMA vm FROM PUBLIC;

CREATE TABLE vm.migrations (
    position integer PRIMARY KEY,
    name text NOT NULL UNIQUE,
    sha256 text NOT NULL,
    sql text NOT NULL,
    applied_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE vm.keys (
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
           || pg_catalog.gen_random_uuid()::text || pg_catalog.gen_random_uuid()::text, 'UTF8'));

-- The outbox: what the relay ships to the platform database and deletes.
CREATE TABLE vm.outbox (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    xid xid8 NOT NULL DEFAULT pg_catalog.pg_current_xact_id(),
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

CREATE FUNCTION vm.transaction_key() RETURNS text
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

CREATE FUNCTION vm.attached() RETURNS jsonb
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

CREATE FUNCTION vm.identity()
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

-- Called by the capture before it turns rows into jsonb. Does nothing
-- here; hardening.sql makes it refuse what would run the Application's
-- code inside the capture.
CREATE FUNCTION vm.capture_guard() RETURNS void
LANGUAGE sql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$ SELECT $f$;

-- As today's vm.capture(), writing vm.outbox instead of vm.history.
CREATE FUNCTION vm.capture() RETURNS trigger
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
DECLARE
    v_who jsonb;
    v_at timestamptz;
    v_key text[];
    v_columns text[];
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NOT EXISTS (SELECT FROM vm_new) THEN RETURN NULL; END IF;
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
        INSERT INTO vm.outbox (at, schema_name, table_name, kind, key, old, new, changed,
                               user_id, application, actor, request, migration)
        SELECT v_at, TG_TABLE_SCHEMA::text, TG_TABLE_NAME::text, 'insert',
               CASE WHEN v_key IS NULL THEN NULL
                    ELSE (SELECT pg_catalog.jsonb_object_agg(c, r.j -> c) FROM pg_catalog.unnest(v_key) c) END,
               NULL, r.j, NULL,
               v_who ->> 'user_id', v_who ->> 'application', v_who ->> 'actor',
               v_who ->> 'request', v_who ->> 'migration'
          FROM (SELECT pg_catalog.to_jsonb(t) AS j, pg_catalog.row_number() OVER () AS i FROM vm_new t) r
         ORDER BY r.i;
    ELSIF TG_OP = 'DELETE' THEN
        INSERT INTO vm.outbox (at, schema_name, table_name, kind, key, old, new, changed,
                               user_id, application, actor, request, migration)
        SELECT v_at, TG_TABLE_SCHEMA::text, TG_TABLE_NAME::text, 'delete',
               CASE WHEN v_key IS NULL THEN NULL
                    ELSE (SELECT pg_catalog.jsonb_object_agg(c, r.j -> c) FROM pg_catalog.unnest(v_key) c) END,
               r.j, NULL, NULL,
               v_who ->> 'user_id', v_who ->> 'application', v_who ->> 'actor',
               v_who ->> 'request', v_who ->> 'migration'
          FROM (SELECT pg_catalog.to_jsonb(t) AS j, pg_catalog.row_number() OVER () AS i FROM vm_old t) r
         ORDER BY r.i;
    ELSE
        SELECT pg_catalog.array_agg(a.attname::text ORDER BY a.attnum)
          INTO v_columns
          FROM pg_catalog.pg_attribute a
         WHERE a.attrelid = TG_RELID AND a.attnum > 0 AND NOT a.attisdropped;
        INSERT INTO vm.outbox (at, schema_name, table_name, kind, key, old, new, changed,
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
          FROM (SELECT pg_catalog.to_jsonb(t) AS j, pg_catalog.row_number() OVER () AS i FROM vm_old t) o
          JOIN (SELECT pg_catalog.to_jsonb(t) AS j, pg_catalog.row_number() OVER () AS i FROM vm_new t) n
            ON n.i = o.i
         ORDER BY o.i;
    END IF;
    RETURN NULL;
END
$f$;

-- TRUNCATE of a captured table is refused, whoever runs it.
CREATE FUNCTION vm.capture_truncate() RETURNS trigger
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
BEGIN
    RAISE EXCEPTION USING ERRCODE = 'VM002',
        MESSAGE = pg_catalog.format('TRUNCATE of %I.%I is refused: its Change history would lose rows',
                                    TG_TABLE_SCHEMA, TG_TABLE_NAME);
END
$f$;

-- The four capture triggers on every table outside vm that lacks one,
-- created by a1_vm (an inheriting member of a1_mig, so it passes the
-- ownership check on a1_mig's tables).
CREATE FUNCTION vm.capture_new_tables() RETURNS integer
LANGUAGE plpgsql VOLATILE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
DECLARE
    v_table record;
    v_made integer := 0;
BEGIN
    FOR v_table IN
        SELECT c.oid::regclass AS relation, e.event, e.timing, e.referencing, e.fn
          FROM pg_catalog.pg_class c
          JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         CROSS JOIN (VALUES ('insert', 'AFTER', 'REFERENCING NEW TABLE AS vm_new', 'vm.capture()'),
                            ('update', 'AFTER', 'REFERENCING OLD TABLE AS vm_old NEW TABLE AS vm_new', 'vm.capture()'),
                            ('delete', 'AFTER', 'REFERENCING OLD TABLE AS vm_old', 'vm.capture()'),
                            ('truncate', 'BEFORE', '', 'vm.capture_truncate()')) AS e(event, timing, referencing, fn)
         WHERE c.relkind IN ('r', 'p')
           AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
           AND pg_catalog.left(n.nspname, 3) <> 'pg_'
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

-- Everything wrong with the capture, one line each; no line means whole.
CREATE FUNCTION vm.capture_problems() RETURNS SETOF text
LANGUAGE sql STABLE SECURITY DEFINER PARALLEL UNSAFE
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
    ('vm_capture_truncate', 34, 'vm.capture_truncate()'::pg_catalog.regprocedure, NULL, NULL))
SELECT pg_catalog.format('%s: %s missing, disabled or altered', t.oid::regclass, w.name)
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
SELECT pg_catalog.format('%s: trigger %s calls %s and is not a capture trigger',
                         g.tgrelid::regclass, g.tgname, g.tgfoid::regprocedure)
  FROM pg_catalog.pg_trigger g
  JOIN pg_catalog.pg_proc p ON p.oid = g.tgfoid
  JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
 WHERE n.nspname = 'vm'
   AND (g.tgname NOT LIKE 'vm\_capture\_%' OR g.tgrelid NOT IN (SELECT oid FROM tabs))
UNION ALL
SELECT pg_catalog.format('%s: deferrable trigger %s', g.tgrelid::regclass, g.tgname)
  FROM pg_catalog.pg_trigger g
 WHERE g.tgdeferrable AND NOT g.tgisinternal
UNION ALL
SELECT pg_catalog.format('cast %s -> %s runs user code where to_jsonb looks',
                         c.castsource::regtype, c.casttarget::regtype)
  FROM pg_catalog.pg_cast c
  JOIN pg_catalog.pg_proc p ON p.oid = c.castfunc
  JOIN pg_catalog.pg_language l ON l.oid = p.prolang
 WHERE c.casttarget IN ('json'::pg_catalog.regtype, 'jsonb'::pg_catalog.regtype)
   AND c.castsource >= 16384 AND l.lanname NOT IN ('c', 'internal')
UNION ALL
SELECT pg_catalog.format('extension %s is in schema vm', e.extname)
  FROM pg_catalog.pg_extension e WHERE e.extnamespace = 'vm'::pg_catalog.regnamespace
UNION ALL
SELECT pg_catalog.format('%s %s in schema vm is not the platform''s', o.kind, o.name)
  FROM (SELECT 'function' AS kind, p.oid::regprocedure::text AS name, p.proowner AS owner
          FROM pg_catalog.pg_proc p WHERE p.pronamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'relation', c.oid::regclass::text, c.relowner
          FROM pg_catalog.pg_class c WHERE c.relnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'type', t.oid::regtype::text, t.typowner
          FROM pg_catalog.pg_type t WHERE t.typnamespace = 'vm'::pg_catalog.regnamespace
        UNION ALL
        SELECT 'operator', o.oid::regoperator::text, o.oprowner
          FROM pg_catalog.pg_operator o WHERE o.oprnamespace = 'vm'::pg_catalog.regnamespace) o
 WHERE o.owner <> (SELECT n.nspowner FROM pg_catalog.pg_namespace n WHERE n.nspname = 'vm')
$f$;

-- Which tuple of pg_trigger each capture trigger is: any ALTER/DROP/CREATE
-- of one, even undone again, gives a new tuple (another ctid or xmin).
CREATE FUNCTION vm.capture_digest() RETURNS text
LANGUAGE sql STABLE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
SELECT pg_catalog.md5(coalesce(pg_catalog.string_agg(
           pg_catalog.format('%s|%s|%s|%s|%s', g.tgrelid, g.tgname, g.oid, g.xmin, g.ctid),
           ';' ORDER BY g.tgrelid, g.tgname), ''))
  FROM pg_catalog.pg_trigger g
 WHERE g.tgname LIKE 'vm\_capture\_%'
$f$;

-- The outbox rows this transaction wrote, per table and kind.
CREATE FUNCTION vm.outbox_this_transaction()
RETURNS TABLE (table_name text, kind text, n bigint)
LANGUAGE sql STABLE SECURITY DEFINER PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
SELECT o.schema_name || '.' || o.table_name, o.kind, pg_catalog.count(*)
  FROM vm.outbox o WHERE o.xid = pg_catalog.pg_current_xact_id()
 GROUP BY 1, 2 ORDER BY 1, 2
$f$;

REVOKE ALL ON ALL FUNCTIONS IN SCHEMA vm FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA vm FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA vm FROM PUBLIC;
GRANT USAGE ON SCHEMA vm TO a1_rt, a1_mig;
GRANT EXECUTE ON FUNCTION vm.identity(), vm.transaction_key() TO a1_rt;
-- What the platform calls on the migration role's own session, inside the
-- migration's transaction (so the migration role can call them too).
GRANT EXECUTE ON FUNCTION vm.transaction_key(), vm.capture_new_tables(), vm.capture_problems(),
                          vm.capture_digest(), vm.outbox_this_transaction() TO a1_mig;
