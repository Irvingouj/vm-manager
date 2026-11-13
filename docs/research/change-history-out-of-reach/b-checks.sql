-- Part B: what a table's owner (a1_mig) can do to the capture inside a
-- migration's transaction, and whether the platform can tell before the
-- commit. Each case runs in a transaction on a1_mig's session, as the host
-- runs migrations, and is rolled back unless it says otherwise.

-- @conn vm a1_vm a1
-- @file B00 ok hardening.sql
-- @step B00a ok a digest of every table's structure (for B5)
CREATE FUNCTION vm.structure_digest() RETURNS TABLE (relation oid, name text, filenode oid, columns text, parents text)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $f$
SELECT c.oid, c.oid::regclass::text, c.relfilenode,
       (SELECT string_agg(a.attnum || ':' || a.attname || ':' || a.atttypid || ':' || a.attisdropped, ',' ORDER BY a.attnum)
          FROM pg_attribute a WHERE a.attrelid = c.oid AND a.attnum > 0),
       (SELECT string_agg(i.inhparent::text, ',' ORDER BY i.inhparent) FROM pg_inherits i WHERE i.inhrelid = c.oid)
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE c.relkind IN ('r', 'p') AND n.nspname <> 'vm' AND n.nspname <> 'information_schema'
   AND left(n.nspname, 3) <> 'pg_'
 ORDER BY c.oid
$f$;
GRANT EXECUTE ON FUNCTION vm.structure_digest() TO a1_mig;

-- B1. The capture triggers' tuples. The catalog check (capture_problems)
-- sees the final state; the digest sees any change made in between.
-- @conn mig a1_mig a1
-- @step B10 ok begin
BEGIN;
-- @attach migration 0001
-- @step B11 ok the digest before the migration's statements
SELECT vm.capture_digest();
-- @step B12 rows:0 disable, update, enable: the final catalog looks whole
ALTER TABLE public.items DISABLE TRIGGER vm_capture_update;
UPDATE public.items SET amount = 0;
ALTER TABLE public.items ENABLE TRIGGER vm_capture_update;
SELECT * FROM vm.capture_problems();
-- @step B13 differs:B11 the digest is not the same
SELECT vm.capture_digest();
-- @step B14 ok
ROLLBACK;

-- @step B15 ok the same inside a released savepoint
BEGIN;
-- @attach migration 0001
-- @step B15a ok
SELECT vm.capture_digest();
-- @step B15b ok
SAVEPOINT s; ALTER TABLE public.items DISABLE TRIGGER USER; UPDATE public.items SET amount = 0;
ALTER TABLE public.items ENABLE TRIGGER USER; RELEASE SAVEPOINT s;
-- @step B15c differs:B15a digest differs
SELECT vm.capture_digest();
-- @step B15d ok
ROLLBACK;

-- @step B16 ok inside a savepoint rolled back: nothing changed, digest equal
BEGIN;
-- @attach migration 0001
-- @step B16a ok
SELECT vm.capture_digest();
-- @step B16b ok
SAVEPOINT s; ALTER TABLE public.items DISABLE TRIGGER USER; UPDATE public.items SET amount = 0; ROLLBACK TO SAVEPOINT s;
-- @step B16c same:B16a digest equal
SELECT vm.capture_digest();
-- @step B16d has:11 and the update did not happen
SELECT amount FROM public.items WHERE id = 1;
-- @step B16e ok
ROLLBACK;

-- @step B17 ok a trigger of the same name calling its own function
BEGIN;
CREATE FUNCTION public.fake() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$;
DROP TRIGGER vm_capture_update ON public.items;
CREATE TRIGGER vm_capture_update AFTER UPDATE ON public.items REFERENCING OLD TABLE AS vm_old NEW TABLE AS vm_new
    FOR EACH STATEMENT EXECUTE FUNCTION public.fake();
-- @step B17a has:vm_capture_update the catalog check names it
SELECT * FROM vm.capture_problems();
-- @step B17b ok
ROLLBACK;

-- @step B18 ok rename the trigger away and back
BEGIN;
-- @step B18a ok
SELECT vm.capture_digest();
-- @step B18b differs:B18a
ALTER TRIGGER vm_capture_update ON public.items RENAME TO x;
ALTER TRIGGER x ON public.items RENAME TO vm_capture_update;
SELECT vm.capture_digest();
-- @step B18c ok
ROLLBACK;

-- @step B19 ok ordinary migrations leave the digest alone
BEGIN;
-- @step B19a ok
SELECT vm.capture_digest();
-- @step B19b same:B19a add, retype, rename, constrain, index, drop a column; rename and move the table
ALTER TABLE public.items ADD COLUMN extra text;
ALTER TABLE public.items ALTER COLUMN amount TYPE numeric(12, 2);
ALTER TABLE public.items RENAME COLUMN title TO name;
ALTER TABLE public.items ALTER COLUMN name SET NOT NULL;
CREATE INDEX ON public.items (name);
ALTER TABLE public.items DROP COLUMN extra;
ALTER TABLE public.items ADD CONSTRAINT positive CHECK (amount >= 0);
CREATE TABLE public.tags (id integer PRIMARY KEY, item integer REFERENCES public.items);
ALTER TABLE public.items RENAME TO things;
CREATE SCHEMA app;
ALTER TABLE public.things SET SCHEMA app;
SELECT vm.capture_digest();
-- @step B19c ok
ROLLBACK;

-- B2. The platform's check runs on a1_mig's own session, which a1_mig has
-- prepared: shadows in public, first on the path. The check pins its path.
-- @step B20 ok
BEGIN;
CREATE FUNCTION public.yes(oid, oid) RETURNS boolean LANGUAGE sql AS 'SELECT true';
CREATE OPERATOR public.= (LEFTARG = oid, RIGHTARG = oid, FUNCTION = public.yes);
CREATE FUNCTION public.yes(name, text) RETURNS boolean LANGUAGE sql AS 'SELECT true';
CREATE OPERATOR public.= (LEFTARG = name, RIGHTARG = text, FUNCTION = public.yes);
SET LOCAL search_path = public, pg_catalog;
DROP TRIGGER vm_capture_delete ON public.items;
-- @step B20a has:vm_capture_delete the check still sees it
SELECT * FROM vm.capture_problems();
-- @step B20b ok
ROLLBACK;

-- B3. Code of a1_mig that runs at COMMIT, after any check the platform
-- ran before it: a deferred constraint trigger, a cursor WITH HOLD.
-- @step B30 ok a deferred constraint trigger that switches the capture off at commit
BEGIN;
-- @attach migration 0002_deferred
-- @step B30a ok
CREATE FUNCTION public.sneak() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    ALTER TABLE public.notes DISABLE TRIGGER vm_capture_update;
    UPDATE public.notes SET body = 'changed at commit';
    ALTER TABLE public.notes ENABLE TRIGGER vm_capture_update;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER sneak AFTER UPDATE ON public.items DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION public.sneak();
UPDATE public.items SET amount = amount WHERE id = 1;
-- @step B30b has:deferrable the catalog check names the deferrable trigger
SELECT * FROM vm.capture_problems();
-- @step B30c ok the digest before COMMIT
SELECT vm.capture_digest();
-- @step B30d ok drop the trigger, its event still queued
DROP TRIGGER sneak ON public.items;
-- @step B30e rows:0 now the catalog check sees nothing wrong
SELECT * FROM vm.capture_problems();
-- @step B30f same:B30c and the digest is unchanged
SELECT vm.capture_digest();
-- @step B30g any COMMIT: does the dropped trigger's event still run?
COMMIT;
-- @conn vm a1_vm a1
-- @step B30h any the notes after that commit, and their outbox rows
SELECT (SELECT string_agg(body, ',') FROM public.notes),
       (SELECT count(*) FROM vm.outbox WHERE table_name = 'notes' AND new::text LIKE '%changed at commit%');

-- @conn mig a1_mig a1
-- @step B31 ok the same, the trigger kept: it runs at COMMIT, after the check
BEGIN;
-- @attach migration 0002_deferred
-- @step B31a ok
CREATE CONSTRAINT TRIGGER sneak AFTER UPDATE ON public.items DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION public.sneak();
UPDATE public.items SET amount = amount WHERE id = 1;
-- @step B31b any COMMIT
COMMIT;
-- @conn vm a1_vm a1
-- @step B31c any the notes after that commit, and their outbox rows
SELECT (SELECT string_agg(body, ',') FROM public.notes),
       (SELECT count(*) FROM vm.outbox WHERE table_name = 'notes' AND new::text LIKE '%changed at commit%');
-- @conn mig a1_mig a1
-- @step B31d ok put the note back, recorded
BEGIN;
-- @attach migration 0002_cleanup
-- @step B31e ok
DROP TRIGGER sneak ON public.items;
UPDATE public.notes SET body = 'note';
COMMIT;

-- @step B32 ok with SET CONSTRAINTS ALL IMMEDIATE before the check, it runs before the check
BEGIN;
-- @attach migration 0002_deferred
-- @step B32a ok
SELECT vm.capture_digest();
-- @step B32b ok
CREATE CONSTRAINT TRIGGER sneak AFTER UPDATE ON public.items DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION public.sneak();
UPDATE public.items SET amount = amount WHERE id = 1;
SET CONSTRAINTS ALL IMMEDIATE;
-- @step B32c differs:B32a the digest shows it
SELECT vm.capture_digest();
-- @step B32d ok
ROLLBACK;

-- @step B33 ok a cursor WITH HOLD whose query runs at COMMIT
BEGIN;
-- @attach migration 0003_cursor
-- @step B33a ok
CREATE FUNCTION public.sneak2() RETURNS integer LANGUAGE plpgsql AS $$
BEGIN
    ALTER TABLE public.notes DISABLE TRIGGER vm_capture_update;
    UPDATE public.notes SET body = 'changed by a cursor at commit';
    ALTER TABLE public.notes ENABLE TRIGGER vm_capture_update;
    RETURN 1;
END $$;
DECLARE c CURSOR WITH HOLD FOR SELECT public.sneak2();
-- @step B33b ok the digest before COMMIT
SELECT vm.capture_digest();
-- @step B33c rows:0 the check sees nothing
SELECT * FROM vm.capture_problems();
-- @step B33d any COMMIT
COMMIT;
-- @conn vm a1_vm a1
-- @step B33e any the notes after that commit, and their outbox rows
SELECT (SELECT string_agg(body, ',') FROM public.notes),
       (SELECT count(*) FROM vm.outbox WHERE table_name = 'notes' AND new::text LIKE '%cursor%');
-- @conn mig a1_mig a1
-- @step B33f ok
CLOSE ALL;
BEGIN;
-- @attach migration 0003_cleanup
-- @step B33g ok
UPDATE public.notes SET body = 'note';
COMMIT;
-- @step B34 ok with CLOSE ALL before the check, the cursor's query never runs
BEGIN;
-- @attach migration 0003_cursor
-- @step B34a ok
DECLARE c CURSOR WITH HOLD FOR SELECT public.sneak2();
CLOSE ALL;
COMMIT;
-- @conn vm a1_vm a1
-- @step B34b has:note the note is unchanged
SELECT string_agg(body, ',') FROM public.notes;

-- B4. TRUNCATE.
-- @conn mig a1_mig a1
-- @step B40 err:VM002 TRUNCATE ... CASCADE through a captured table
TRUNCATE public.items CASCADE;
-- @step B41 ok partitions: captured parent and partition
BEGIN;
-- @attach migration 0004_parts
-- @step B41a ok
CREATE TABLE public.p (id integer, k integer) PARTITION BY RANGE (k);
CREATE TABLE public.p1 PARTITION OF public.p FOR VALUES FROM (0) TO (10);
CREATE TABLE public.p2 PARTITION OF public.p FOR VALUES FROM (10) TO (20);
SELECT vm.capture_new_tables();
INSERT INTO public.p VALUES (1, 1), (2, 15);
-- @step B41b err:VM002 TRUNCATE of the parent
TRUNCATE public.p;
-- @step B41c ok
ROLLBACK;
BEGIN;
-- @attach migration 0004_parts
-- @step B41d ok
CREATE TABLE public.p (id integer, k integer) PARTITION BY RANGE (k);
CREATE TABLE public.p1 PARTITION OF public.p FOR VALUES FROM (0) TO (10);
SELECT vm.capture_new_tables();
INSERT INTO public.p VALUES (1, 1);
DROP TRIGGER vm_capture_truncate ON public.p;
-- @step B41e err:VM002 TRUNCATE of the parent without its own trigger: the partition's fires
TRUNCATE public.p;
-- @step B41f ok
ROLLBACK;

-- B5. Changes of data that fire no row trigger: a table rewrite with
-- USING, a table replaced under its name. The capture checks pass; the
-- structure digest shows them.
-- @step B50 ok
BEGIN;
-- @attach migration 0005_rewrite
-- @step B50a ok
SELECT vm.capture_digest();
-- @step B50b ok
SELECT * FROM vm.structure_digest();
-- @step B50c ok every amount set to 0 by a rewrite
ALTER TABLE public.items ALTER COLUMN amount TYPE numeric USING 0;
-- @step B50d has:0 the values changed
SELECT string_agg(amount::text, ',') FROM public.items;
-- @step B50e same:B50a the capture digest is unchanged
SELECT vm.capture_digest();
-- @step B50f rows:0 the catalog check sees nothing
SELECT * FROM vm.capture_problems();
-- @step B50g rows:0 the outbox has nothing of it
SELECT * FROM vm.outbox_this_transaction();
-- @step B50h differs:B50b the structure digest shows the rewrite (a new filenode)
SELECT * FROM vm.structure_digest();
-- @step B50i ok
ROLLBACK;
-- @step B51 ok a table replaced under its own name
BEGIN;
-- @attach migration 0005_replace
-- @step B51a ok
SELECT * FROM vm.structure_digest();
-- @step B51b ok
ALTER TABLE public.notes RENAME TO notes_old;
CREATE TABLE public.notes (id integer PRIMARY KEY, item integer, body text);
INSERT INTO public.notes SELECT id, item, 'rewritten' FROM public.notes_old;
DROP TABLE public.notes_old;
SELECT vm.capture_new_tables();
-- @step B51c rows:0 the catalog check sees nothing
SELECT * FROM vm.capture_problems();
-- @step B51d differs:B51a the structure digest shows a new table under the old name
SELECT * FROM vm.structure_digest();
-- @step B51e ok
ROLLBACK;
