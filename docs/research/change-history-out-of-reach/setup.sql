-- Set-up of the design under test, done as the platform's role `plat`,
-- which has only CREATEDB and CREATEROLE. The superuser is used once: to
-- create `plat`.
--
--   a1      the own database, owned by a1_vm
--   a1_vm   the third role: owns the database and schema vm; an inheriting
--           member of a1_mig, so it may put triggers on a1_mig's tables
--   a1_mig  applies migrations: CREATE on the database and on public
--   a1_rt   the runtime role
--   platdb  the platform database, owned by plat, with the Change history

-- @conn su postgres postgres
-- @step S01 ok the platform's role (the only act of the superuser)
CREATE ROLE plat LOGIN CREATEDB CREATEROLE PASSWORD 'plat';

-- @conn plat plat postgres
-- @step S02 ok the platform database
CREATE DATABASE platdb;
-- @step S03 ok three roles for the own database
CREATE ROLE a1_vm LOGIN PASSWORD 'a1_vm';
CREATE ROLE a1_mig LOGIN PASSWORD 'a1_mig';
CREATE ROLE a1_rt LOGIN PASSWORD 'a1_rt';
GRANT a1_vm TO plat WITH INHERIT FALSE, SET TRUE;
GRANT a1_mig TO a1_vm WITH INHERIT TRUE, SET FALSE;
-- @step S04 ok the own database, owned by the third role
CREATE DATABASE a1 OWNER a1_vm TEMPLATE template0;

-- @conn vm a1_vm a1
-- @step S05 ok rights in the database
REVOKE ALL ON DATABASE a1 FROM PUBLIC;
GRANT CONNECT ON DATABASE a1 TO a1_mig, a1_rt;
GRANT CREATE ON DATABASE a1 TO a1_mig;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE, CREATE ON SCHEMA public TO a1_mig;
GRANT USAGE ON SCHEMA public TO a1_rt;
-- @file S06 ok design.sql
-- @step S07 ok default privileges for what a1_mig makes, set by a1_vm
ALTER DEFAULT PRIVILEGES FOR ROLE a1_mig GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO a1_rt;
ALTER DEFAULT PRIVILEGES FOR ROLE a1_mig GRANT USAGE, SELECT ON SEQUENCES TO a1_rt;
ALTER DEFAULT PRIVILEGES FOR ROLE a1_mig GRANT USAGE ON SCHEMAS TO a1_rt;

-- @conn plat_p plat platdb
-- @step S08 ok the Change history in the platform database
CREATE TABLE history (
    application text NOT NULL,
    change_id bigint NOT NULL,
    at timestamptz NOT NULL,
    schema_name text NOT NULL,
    table_name text NOT NULL,
    kind text NOT NULL,
    key jsonb, old jsonb, new jsonb, changed jsonb,
    user_id text NOT NULL, app text NOT NULL, actor text NOT NULL,
    request text, migration text,
    PRIMARY KEY (application, change_id)
);
CREATE INDEX history_row ON history (application, schema_name, table_name, jsonb_hash_extended(key, 0), change_id);
REVOKE ALL ON DATABASE platdb FROM PUBLIC;

-- A first migration, as the host would run it: a transaction on a1_mig's
-- own session, identity attached, the capture put on what it made.
-- @conn mig a1_mig a1
-- @step S10 ok begin the first migration
BEGIN;
-- @attach migration 0000_first
-- @step S11 ok the first migration's tables
CREATE TABLE public.items (id integer PRIMARY KEY, title text, amount numeric);
CREATE TABLE public.notes (id integer PRIMARY KEY, item integer REFERENCES public.items ON DELETE CASCADE, body text);
SELECT vm.capture_new_tables();
-- @step S12 rows:0 the capture is whole
SELECT * FROM vm.capture_problems();
-- @step S13 ok commit
COMMIT;

-- @conn rt a1_rt a1
-- @step S20 ok a write as the runtime role
BEGIN;
-- @attach user
-- @step S21 ok rows
INSERT INTO public.items VALUES (1, 'one', 10), (2, 'two', 20);
INSERT INTO public.notes VALUES (1, 1, 'note');
UPDATE public.items SET amount = 11 WHERE id = 1;
COMMIT;
-- @conn vm a1_vm a1
-- @step S22 has:4 four changes in the outbox
SELECT count(*) FROM vm.outbox;
