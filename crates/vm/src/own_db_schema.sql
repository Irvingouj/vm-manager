-- The platform's schema `vm` in an Application's own database
-- (blackbox/CONTRACT.md, "Identity and Change history in the own
-- database"). Run by the owner role, in a transaction, whenever the own
-- database is made and whenever a Version is sent: every statement is
-- idempotent, so an own database made before any of this gets it whole,
-- and one that has it is left as it is. The runtime role's grants follow
-- it (crate::own_db), since they name the role.
--
-- Every function that runs with its owner's rights pins its search path to
-- the catalog (then nothing: the runtime role can make no temporary
-- object), and names the platform's own objects with `vm.`, so that no
-- name Application SQL could make is found first.

CREATE SCHEMA IF NOT EXISTS vm;
REVOKE ALL ON SCHEMA vm FROM PUBLIC;

-- Which migrations have been applied (ticket "An Application's own
-- database").
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
-- made once. Only the owner role reads it: the host, once per own database
-- (a result, which Postgres does not log), and the functions below.
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
-- zone). An attached identity is signed over it.
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

-- The capture: an AFTER ... FOR EACH STATEMENT trigger for each of
-- insert, update and delete, with the statement's rows as transition
-- tables (`vm_new`, `vm_old`). The identity is verified once per
-- statement that changed rows; then one history row per changed row is
-- written, in the order the rows were changed. A statement that changed
-- no row records nothing and needs no identity. An update's old and new
-- rows are paired in the order Postgres put them into its two transition
-- tables, which is the order it updated them in, one old and one new each.
CREATE OR REPLACE FUNCTION vm.capture() RETURNS trigger
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
        IF NOT EXISTS (SELECT FROM vm_new) THEN
            RETURN NULL;
        END IF;
    ELSIF NOT EXISTS (SELECT FROM vm_old) THEN
        RETURN NULL;
    END IF;
    v_who := vm.attached();
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
          FROM (SELECT pg_catalog.to_jsonb(t) AS j, pg_catalog.row_number() OVER () AS i FROM vm_new t) r
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
          FROM (SELECT pg_catalog.to_jsonb(t) AS j, pg_catalog.row_number() OVER () AS i FROM vm_old t) r
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
          FROM (SELECT pg_catalog.to_jsonb(t) AS j, pg_catalog.row_number() OVER () AS i FROM vm_old t) o
          JOIN (SELECT pg_catalog.to_jsonb(t) AS j, pg_catalog.row_number() OVER () AS i FROM vm_new t) n
            ON n.i = o.i
         ORDER BY o.i;
    END IF;
    RETURN NULL;
END
$f$;

-- Puts the three capture triggers on every table, in any schema but the
-- platform's and the catalog's, that does not have them yet: partitioned
-- tables and partitions both, since a statement fires the statement
-- triggers of the table it names only. Run by the host after each
-- migration, inside the migrations' transaction; answers how many
-- triggers it made.
CREATE OR REPLACE FUNCTION vm.capture_new_tables() RETURNS integer
LANGUAGE plpgsql VOLATILE PARALLEL UNSAFE
SET search_path = pg_catalog, pg_temp
AS $f$
DECLARE
    v_table record;
    v_made integer := 0;
BEGIN
    FOR v_table IN
        SELECT c.oid::regclass AS relation, e.event, e.referencing
          FROM pg_catalog.pg_class c
          JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         CROSS JOIN (VALUES ('insert', 'NEW TABLE AS vm_new'),
                            ('update', 'OLD TABLE AS vm_old NEW TABLE AS vm_new'),
                            ('delete', 'OLD TABLE AS vm_old')) AS e(event, referencing)
         WHERE c.relkind IN ('r', 'p')
           AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
           AND pg_catalog.left(n.nspname, 3) <> 'pg_'
           AND NOT EXISTS (SELECT FROM pg_catalog.pg_trigger t
                            WHERE t.tgrelid = c.oid AND t.tgname = 'vm_capture_' || e.event)
         ORDER BY c.oid, e.event
    LOOP
        EXECUTE pg_catalog.format(
            'CREATE TRIGGER %I AFTER %s ON %s REFERENCING %s FOR EACH STATEMENT EXECUTE FUNCTION vm.capture()',
            'vm_capture_' || v_table.event, pg_catalog.upper(v_table.event),
            v_table.relation, v_table.referencing);
        v_made := v_made + 1;
    END LOOP;
    RETURN v_made;
END
$f$;

SELECT vm.capture_new_tables();

-- New functions are executable by PUBLIC: only these two are by anyone but
-- the owner, and only for the runtime role (granted with its name).
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA vm FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA vm FROM PUBLIC;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA vm FROM PUBLIC;
