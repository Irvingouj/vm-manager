-- Part A: the migration role (a1_mig) attacks what the third role (a1_vm)
-- owns. Each step is one attempt; an attempt that succeeds and could harm
-- is inside a transaction that is rolled back, unless the step says not.
-- F0 and F9 fingerprint, as a1_vm, everything of vm (the keys' digest, the
-- outbox, every function's text, owner, SECURITY DEFINER and settings,
-- every relation's owner and rights, the schema's owner and rights, the
-- capture triggers' tuples); F9 must equal F0.

-- @conn vm a1_vm a1
-- @step F0 ok fingerprint of vm before
SELECT md5(k.k1 || k.k2) AS keys,
       (SELECT count(*) || ':' || md5(string_agg(o.id || o.kind || coalesce(o.new::text, ''), ',' ORDER BY o.id)) FROM vm.outbox o) AS outbox,
       (SELECT md5(string_agg(p.proname || p.prosrc || p.proowner::regrole || p.prosecdef || coalesce(array_to_string(p.proconfig, ','), '') || coalesce(p.proacl::text, ''), ';' ORDER BY p.proname))
          FROM pg_proc p WHERE p.pronamespace = 'vm'::regnamespace) AS functions,
       (SELECT string_agg(c.relname || ':' || c.relowner::regrole || ':' || coalesce(c.relacl::text, ''), ',' ORDER BY c.relname)
          FROM pg_class c WHERE c.relnamespace = 'vm'::regnamespace) AS relations,
       (SELECT n.nspowner::regrole || ':' || n.nspacl::text FROM pg_namespace n WHERE n.nspname = 'vm') AS schema,
       vm.capture_digest() AS triggers
  FROM vm.keys k;

-- @conn mig a1_mig a1
-- @step A01 err:42501 read the keys
SELECT * FROM vm.keys;
-- @step A02 err:42501 read the outbox
SELECT * FROM vm.outbox;
-- @step A03 err:42501 write the outbox
INSERT INTO vm.outbox (at, schema_name, table_name, kind, user_id, application, actor)
VALUES (now(), 'public', 'items', 'insert', 'someone', 'a1', 'user');
-- @step A04 err:42501 update the outbox
UPDATE vm.outbox SET user_id = 'someone else';
-- @step A05 err:42501 delete from the outbox
DELETE FROM vm.outbox;
-- @step A06 err:42501 truncate the outbox
TRUNCATE vm.outbox;
-- @step A07 err:42501 the outbox's sequence
SELECT setval(pg_get_serial_sequence('vm.outbox', 'id'), 1);
-- @step A08 err:42501 lock the outbox
BEGIN; LOCK TABLE vm.outbox IN ACCESS EXCLUSIVE MODE; ROLLBACK;
-- @step A09 err:42501 read the record of migrations
SELECT * FROM vm.migrations;
-- @step A10 err:42501 replace a vm function
CREATE OR REPLACE FUNCTION vm.capture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$;
-- @step A11 err:42501 drop a vm function
DROP FUNCTION vm.capture() CASCADE;
-- @step A12 err:42501 take a vm function over
ALTER FUNCTION vm.capture() OWNER TO a1_mig;
-- @step A13 err:42501 change a vm function's settings
ALTER FUNCTION vm.capture() RESET search_path;
-- @step A14 err:42501 call the identity check directly
SELECT vm.attached();
-- @step A15 err:42501 create a table in vm
CREATE TABLE vm.mine (i integer);
-- @step A16 err:42501 create a function in vm
CREATE FUNCTION vm.mine() RETURNS integer LANGUAGE sql AS 'SELECT 1';
-- @step A17 err:42501 move an own table into vm
CREATE TABLE public.mover (i integer); ALTER TABLE public.mover SET SCHEMA vm;
-- @step A18 err:42501 a view or rule on the outbox
CREATE RULE r AS ON INSERT TO vm.outbox DO INSTEAD NOTHING;
-- @step A19 err:42501 a trigger on the outbox
CREATE FUNCTION public.nop() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$;
CREATE TRIGGER t BEFORE INSERT ON vm.outbox FOR EACH ROW EXECUTE FUNCTION public.nop();
-- @step A20 err:42501 a child table of the outbox
CREATE TABLE public.kid () INHERITS (vm.outbox);
-- @step A21 err:42501 rename schema vm
ALTER SCHEMA vm RENAME TO vm_old;
-- @step A22 err:42501 take schema vm over
ALTER SCHEMA vm OWNER TO a1_mig;
-- @step A23 err:42501 drop schema vm
DROP SCHEMA vm CASCADE;
-- @step A24 err:42501 comment on schema vm
COMMENT ON SCHEMA vm IS 'mine';
-- @step A25 err:42501 reassign what a1_vm owns
REASSIGN OWNED BY a1_vm TO a1_mig;
-- @step A26 err:42501 drop what a1_vm owns
DROP OWNED BY a1_vm;
-- @step A27 err:42501 become a1_vm
SET ROLE a1_vm;
-- @step A28 err:42501 become a1_vm by session
SET SESSION AUTHORIZATION a1_vm;
-- @step A29 err:42501 become a1_vm by setting
SELECT set_config('role', 'a1_vm', false);
-- @step A30 err:42501 default privileges of a1_vm
ALTER DEFAULT PRIVILEGES FOR ROLE a1_vm GRANT SELECT ON TABLES TO a1_mig;
-- @step A31 err:42501 grant itself the keys
GRANT SELECT ON vm.keys TO a1_mig;
-- @step A32 err:42501 grant itself a1_vm
GRANT a1_vm TO a1_mig;
-- @step A33 err:42501 grant itself to the runtime role
GRANT a1_mig TO a1_rt;
-- @step A34 err:42501 take a1_vm's membership in a1_mig away
REVOKE a1_mig FROM a1_vm;
-- @step A35 err:42501 settings of the database
ALTER DATABASE a1 SET search_path = public, pg_catalog;
-- @step A36 err:42501 take the database over
ALTER DATABASE a1 OWNER TO a1_mig;
-- @step A37 err:42501 rename the database
ALTER DATABASE a1 RENAME TO a2;
-- @step A38 err:42501 another role's attributes
ALTER ROLE a1_vm PASSWORD 'x';
-- @step A39 err:42501 its own attributes: CREATEROLE
ALTER ROLE a1_mig CREATEROLE;
-- @step A40 err:42501 its own attributes: VALID UNTIL
ALTER ROLE a1_mig VALID UNTIL 'infinity';
-- @step A41 err:42501 create a role
CREATE ROLE intruder LOGIN;
-- @step A42 ok its own settings (session defaults of its own logins)
BEGIN; ALTER ROLE a1_mig SET search_path = public; ROLLBACK;
-- @step A43 ok its own password (committed; see L1)
ALTER ROLE a1_mig PASSWORD 'pwned';
-- @login L1 ok a1_mig a1 pwned
-- @conn plat plat postgres
-- @step A44 ok the platform's role sets it back
ALTER ROLE a1_mig PASSWORD 'a1_mig';
-- @step A44a ok the platform's role can take the login away between migrations
ALTER ROLE a1_mig NOLOGIN;
-- @conn mig a1_mig a1
-- @step A44b ok a1_mig's open session may still change its password
ALTER ROLE a1_mig PASSWORD 'pwned2';
-- @login L2 err a1_mig a1 pwned2
-- @step A44c err:42501 nor give itself the login back
ALTER ROLE a1_mig LOGIN;
-- @conn rt a1_rt a1
-- @step A44d ok the runtime role can change its own password too
BEGIN; ALTER ROLE a1_rt PASSWORD 'x'; ROLLBACK;
-- @conn plat plat postgres
-- @step A44e ok back to the scenario's login
ALTER ROLE a1_mig LOGIN PASSWORD 'a1_mig';
-- @conn mig a1_mig a1
-- @step A45 err:42501 session_replication_role
SET session_replication_role = replica;
-- @step A46 err:42501 temporary objects
CREATE TEMP TABLE t (i integer);
-- @step A47 err:42501 extension not trusted
CREATE EXTENSION dblink;
-- @step A48 ok a trusted extension into vm: allowed (its script runs as the bootstrap superuser)
BEGIN; CREATE EXTENSION hstore SCHEMA vm;
-- @step A48a has:postgres what it put there is the superuser's, the extension a1_mig's
SELECT e.extowner::regrole, (SELECT count(*) FROM pg_proc WHERE pronamespace = 'vm'::regnamespace AND proowner <> 'a1_vm'::regrole),
       (SELECT string_agg(DISTINCT proowner::regrole::text, ',') FROM pg_proc WHERE pronamespace = 'vm'::regnamespace AND proowner <> 'a1_vm'::regrole)
  FROM pg_extension e WHERE e.extname = 'hstore';
-- @step A48b err:42501 add vm.capture() to its extension (to drop it with the extension)
ALTER EXTENSION hstore ADD FUNCTION vm.capture();
-- @step A48c err:42501 replace a function the extension put in vm
BEGIN; CREATE EXTENSION hstore SCHEMA vm;
CREATE OR REPLACE FUNCTION vm.akeys(vm.hstore) RETURNS text[] LANGUAGE plpgsql AS $$ BEGIN RETURN '{}'; END $$;
-- @step A48d has:extension the check names it
BEGIN; CREATE EXTENSION hstore SCHEMA vm; SELECT vm.capture_problems();
-- @step A48e ok
ROLLBACK;
-- @step A49 ok a trusted extension into public
BEGIN; CREATE EXTENSION hstore; ROLLBACK;
-- @step A50 err:42501 an untrusted language
CREATE LANGUAGE plpython3u;
-- @step A51 err:42501 an event trigger
CREATE FUNCTION public.et() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN END $$;
CREATE EVENT TRIGGER et ON ddl_command_start EXECUTE FUNCTION public.et();
-- @step A52 err:42501 a publication of every table
CREATE PUBLICATION p FOR ALL TABLES;
-- @step A53 err:42501 end a1_vm's sessions
SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = 'a1_vm';
-- @step A54 err:42501 reset the statistics counters
SELECT pg_stat_reset();
-- @step A55 err:42501 reset one table's counters
SELECT pg_stat_reset_single_table_counters('public.items'::regclass);
-- @step A56 err:42501 server files
SELECT pg_read_file('postgresql.conf');
-- @step A57 err:42501 give its table to a1_vm
CREATE TABLE public.gift (i integer); ALTER TABLE public.gift OWNER TO a1_vm;
-- @step A58 err:42501 a capture trigger of its own (no EXECUTE on vm.capture)
CREATE TRIGGER mine AFTER INSERT ON public.items REFERENCING NEW TABLE AS vm_new
    FOR EACH STATEMENT EXECUTE FUNCTION vm.capture();
-- @step A59 err:42501 replace the capture trigger with old and new swapped
CREATE OR REPLACE TRIGGER vm_capture_update AFTER UPDATE ON public.items
    REFERENCING OLD TABLE AS vm_new NEW TABLE AS vm_old FOR EACH STATEMENT EXECUTE FUNCTION vm.capture();
-- @step A60 ok disable a capture trigger on its own table (owner's right)
BEGIN; ALTER TABLE public.items DISABLE TRIGGER vm_capture_update; ROLLBACK;
-- @step A61 ok drop a capture trigger on its own table (owner's right)
BEGIN; DROP TRIGGER vm_capture_insert ON public.items; ROLLBACK;
-- @step A62 ok make a capture trigger fire only on replicas (owner's right)
BEGIN; ALTER TABLE public.items ENABLE REPLICA TRIGGER vm_capture_update; ROLLBACK;
-- @step A63 ok rename a capture trigger (owner's right)
BEGIN; ALTER TRIGGER vm_capture_update ON public.items RENAME TO x; ROLLBACK;
-- @step A64 err:VM002 truncate a captured table
TRUNCATE public.notes;
-- @step A65 ok its table unlogged (owner's right; the capture still fires)
BEGIN; ALTER TABLE public.notes SET UNLOGGED; ROLLBACK;
-- @step A66 ok drop all it owns (owner's right; drops every table, unrecorded)
BEGIN; DROP OWNED BY a1_mig; SELECT count(*) FROM pg_class WHERE relnamespace = 'public'::regnamespace AND relkind = 'r'; ROLLBACK;
-- @step A67 err:42P06 a schema named like vm
CREATE SCHEMA vm;
-- @step A68 err:42939 a schema in the reserved pg_ names
CREATE SCHEMA pg_vm;

-- Names the capture or the platform's functions might resolve: shadows in
-- public, put first on the search path. The capture pins its own path.
-- @step A70 ok shadows of what the capture calls, then a captured write
BEGIN;
CREATE FUNCTION public.to_jsonb(anyelement) RETURNS jsonb LANGUAGE sql AS $$ SELECT '"forged"'::jsonb $$;
CREATE FUNCTION public.clock_timestamp() RETURNS timestamptz LANGUAGE sql AS $$ SELECT 'epoch'::timestamptz $$;
CREATE FUNCTION public.jsonb_object_agg(text, jsonb) RETURNS jsonb LANGUAGE sql AS $$ SELECT '{}'::jsonb $$;
SET LOCAL search_path = public, pg_catalog;
-- @attach migration 0001_shadows
-- @step A71 ok
UPDATE public.items SET title = 'shadowed' WHERE id = 2;
SELECT vm.capture_problems();
-- @conn vm a1_vm a1
-- @step A72 lacks:forged the outbox is unaffected by the shadows (row not committed: none seen; see A73)
SELECT new FROM vm.outbox WHERE new::text LIKE '%forged%';
-- @conn mig a1_mig a1
-- @step A73 ok commit the shadow test
COMMIT;
-- @conn vm a1_vm a1
-- @step A74 has:shadowed the update was captured with the real functions
SELECT kind, new, at > now() - interval '1 hour' AS real_time FROM vm.outbox ORDER BY id DESC LIMIT 1;
-- @conn mig a1_mig a1
-- @step A75 ok drop the shadows again
DROP FUNCTION public.to_jsonb(anyelement), public.clock_timestamp(), public.jsonb_object_agg(text, jsonb);
-- @conn vm a1_vm a1
-- @step F1 ok fingerprint after the one committed write of A71
SELECT md5(k.k1 || k.k2) AS keys,
       (SELECT count(*) || ':' || md5(string_agg(o.id || o.kind || coalesce(o.new::text, ''), ',' ORDER BY o.id)) FROM vm.outbox o) AS outbox,
       (SELECT md5(string_agg(p.proname || p.prosrc || p.proowner::regrole || p.prosecdef || coalesce(array_to_string(p.proconfig, ','), '') || coalesce(p.proacl::text, ''), ';' ORDER BY p.proname))
          FROM pg_proc p WHERE p.pronamespace = 'vm'::regnamespace) AS functions,
       (SELECT string_agg(c.relname || ':' || c.relowner::regrole || ':' || coalesce(c.relacl::text, ''), ',' ORDER BY c.relname)
          FROM pg_class c WHERE c.relnamespace = 'vm'::regnamespace) AS relations,
       (SELECT n.nspowner::regrole || ':' || n.nspacl::text FROM pg_namespace n WHERE n.nspname = 'vm') AS schema,
       vm.capture_digest() AS triggers
  FROM vm.keys k;

-- @conn mig a1_mig a1

-- The capture runs as a1_vm (SECURITY DEFINER) and calls to_jsonb on the
-- changed rows; to_jsonb uses a cast to json of a user-defined type if
-- there is one. A cast's function belongs to a1_mig and is not SECURITY
-- DEFINER: it runs as whoever calls it.
-- @step A80 ok a cast to json for an enum of its own
BEGIN;
CREATE TYPE public.mood AS ENUM ('ok', 'sad');
CREATE TABLE public.loot (who text, what text);
CREATE FUNCTION public.mood_json(public.mood) RETURNS json LANGUAGE plpgsql AS $$
DECLARE n bigint;
BEGIN
    INSERT INTO public.loot SELECT current_user, 'keys ' || length(encode(k1 || k2, 'hex')) || ' hex digits' FROM vm.keys;
    DELETE FROM vm.outbox;
    GET DIAGNOSTICS n = ROW_COUNT;
    INSERT INTO public.loot VALUES (current_user, 'deleted ' || n || ' outbox rows');
    RETURN to_json($1::text);
END $$;
CREATE CAST (public.mood AS json) WITH FUNCTION public.mood_json(public.mood);
CREATE TABLE public.moods (id integer PRIMARY KEY, m public.mood);
SELECT vm.capture_new_tables();
-- @attach migration 0002_trojan
-- @step A81 has:a1_vm a captured write runs a1_mig's cast function as a1_vm
INSERT INTO public.moods VALUES (1, 'sad');
SELECT * FROM public.loot;
-- @step A82 has:cast the check names the cast
SELECT vm.capture_problems();
-- @step A83 ok roll it back
ROLLBACK;
-- @step A84 ok the same with a cast to jsonb
BEGIN;
CREATE TYPE public.mood AS ENUM ('ok', 'sad');
CREATE TABLE public.loot (who text, what text);
CREATE FUNCTION public.mood_jsonb(public.mood) RETURNS jsonb LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO public.loot SELECT current_user, 'keys ' || length(encode(k1 || k2, 'hex')) FROM vm.keys;
    RETURN to_jsonb($1::text);
END $$;
CREATE CAST (public.mood AS jsonb) WITH FUNCTION public.mood_jsonb(public.mood);
CREATE TABLE public.moods (id integer PRIMARY KEY, m public.mood);
SELECT vm.capture_new_tables();
-- @attach migration 0002_trojan
-- @step A85 any a captured write with a cast to jsonb only
INSERT INTO public.moods VALUES (1, 'sad');
SELECT * FROM public.loot;
-- @step A86 ok roll it back
ROLLBACK;
-- The same trojan once hardening.sql is applied.
-- @conn vm a1_vm a1
-- @file A86h ok hardening.sql
-- @conn mig a1_mig a1
-- @step A86i ok the trojan again
BEGIN;
CREATE TYPE public.mood AS ENUM ('ok', 'sad');
CREATE TABLE public.loot (who text, what text);
CREATE FUNCTION public.mood_json(public.mood) RETURNS json LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO public.loot SELECT current_user, 'keys ' || length(encode(k1 || k2, 'hex')) FROM vm.keys;
    RETURN to_json($1::text);
END $$;
CREATE CAST (public.mood AS json) WITH FUNCTION public.mood_json(public.mood);
CREATE TABLE public.moods (id integer PRIMARY KEY, m public.mood);
SELECT vm.capture_new_tables();
-- @attach migration 0002_trojan
-- @step A86j err:VM003 hardened: the capture refuses before converting
INSERT INTO public.moods VALUES (1, 'sad');
-- @step A86k ok a write to a table without the enum: refused too (the guard is global)
BEGIN;
CREATE TYPE public.mood AS ENUM ('ok', 'sad');
CREATE FUNCTION public.mood_json(public.mood) RETURNS json LANGUAGE sql AS $$ SELECT to_json($1::text) $$;
CREATE CAST (public.mood AS json) WITH FUNCTION public.mood_json(public.mood);
-- @attach migration 0002_trojan
-- @step A86l err:VM003
UPDATE public.items SET title = title;
-- @step A86m ok an extension's C cast (hstore in public) does not trip the guard
BEGIN; CREATE EXTENSION hstore;
-- @attach migration 0002_hstore
-- @step A86n ok
UPDATE public.items SET title = title;
ROLLBACK;
-- @conn vm a1_vm a1
-- @step F2 ok fingerprint after hardening
SELECT md5(k.k1 || k.k2) AS keys,
       (SELECT count(*) || ':' || md5(string_agg(o.id || o.kind || coalesce(o.new::text, ''), ',' ORDER BY o.id)) FROM vm.outbox o) AS outbox,
       (SELECT md5(string_agg(p.proname || p.prosrc || p.proowner::regrole || p.prosecdef || coalesce(array_to_string(p.proconfig, ','), '') || coalesce(p.proacl::text, ''), ';' ORDER BY p.proname))
          FROM pg_proc p WHERE p.pronamespace = 'vm'::regnamespace) AS functions,
       (SELECT string_agg(c.relname || ':' || c.relowner::regrole || ':' || coalesce(c.relacl::text, ''), ',' ORDER BY c.relname)
          FROM pg_class c WHERE c.relnamespace = 'vm'::regnamespace) AS relations,
       (SELECT n.nspowner::regrole || ':' || n.nspacl::text FROM pg_namespace n WHERE n.nspname = 'vm') AS schema,
       vm.capture_digest() AS triggers
  FROM vm.keys k;

-- @conn mig a1_mig a1
-- @step A87 ok a sequence keeps what a rolled-back transaction wrote into it
CREATE SEQUENCE public.stash;
-- @step A87a ok
BEGIN; SELECT setval('public.stash', 424242); ROLLBACK;
-- @step A88 has:424242 the value outlived the rollback
SELECT last_value FROM public.stash;

-- Code of a1_mig that might run as a1_vm when the capture touches a
-- table: row-level security, defaults, checks, generated columns, index
-- expressions, a trigger of its own. Each reports current_user in a notice.
-- @step A90 ok a table full of a1_mig's code
BEGIN;
CREATE FUNCTION public.spy(i integer) RETURNS integer LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN RAISE NOTICE 'spy ran as %', current_user; RETURN i; END $$;
CREATE FUNCTION public.spy_policy() RETURNS boolean LANGUAGE plpgsql AS $$
BEGIN RAISE NOTICE 'policy ran as %', current_user; RETURN true; END $$;
CREATE FUNCTION public.spy_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE NOTICE 'trigger ran as %', current_user; RETURN NULL; END $$;
CREATE TABLE public.watched (id integer PRIMARY KEY CHECK (public.spy(id) > 0),
                             d integer DEFAULT public.spy(1),
                             g integer GENERATED ALWAYS AS (public.spy(id)) STORED);
CREATE INDEX ON public.watched (public.spy(id));
ALTER TABLE public.watched ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.watched FORCE ROW LEVEL SECURITY;
CREATE POLICY p ON public.watched USING (public.spy_policy()) WITH CHECK (public.spy_policy());
CREATE TRIGGER zz AFTER INSERT ON public.watched FOR EACH STATEMENT EXECUTE FUNCTION public.spy_trigger();
SELECT vm.capture_new_tables();
-- @attach migration 0003_spy
-- @step A91 nonotice:a1_vm a write: none of it runs as a1_vm
INSERT INTO public.watched (id) VALUES (5);
UPDATE public.watched SET id = 6;
DELETE FROM public.watched;
-- @step A92 ok roll it back
ROLLBACK;

-- @conn vm a1_vm a1
-- @step F9 same:F2 fingerprint of vm after: unchanged since F2 (F1 differs from F0 in the outbox only, F2 from F1 in the functions only)
SELECT md5(k.k1 || k.k2) AS keys,
       (SELECT count(*) || ':' || md5(string_agg(o.id || o.kind || coalesce(o.new::text, ''), ',' ORDER BY o.id)) FROM vm.outbox o) AS outbox,
       (SELECT md5(string_agg(p.proname || p.prosrc || p.proowner::regrole || p.prosecdef || coalesce(array_to_string(p.proconfig, ','), '') || coalesce(p.proacl::text, ''), ';' ORDER BY p.proname))
          FROM pg_proc p WHERE p.pronamespace = 'vm'::regnamespace) AS functions,
       (SELECT string_agg(c.relname || ':' || c.relowner::regrole || ':' || coalesce(c.relacl::text, ''), ',' ORDER BY c.relname)
          FROM pg_class c WHERE c.relnamespace = 'vm'::regnamespace) AS relations,
       (SELECT n.nspowner::regrole || ':' || n.nspacl::text FROM pg_namespace n WHERE n.nspname = 'vm') AS schema,
       vm.capture_digest() AS triggers
  FROM vm.keys k;
