-- Part E, on a cluster started with wal_level=logical and
-- max_prepared_transactions=2 (settings only the instance's owner can
-- make): what logical decoding sees of what a table's owner does, read
-- with the superuser's slot (test_decoding).

-- @conn su postgres a1
-- @step L01 ok a logical slot in the own database (superuser)
SELECT pg_create_logical_replication_slot('s', 'test_decoding') IS NOT NULL;
-- @conn mig a1_mig a1
-- @step L02 ok a migration with the capture switched off, a table unlogged, a replica identity of nothing
BEGIN;
-- @attach migration 0009_quiet
-- @step L03 ok
ALTER TABLE public.items DISABLE TRIGGER vm_capture_update;
UPDATE public.items SET title = 'triggers off' WHERE id = 2;
ALTER TABLE public.items ENABLE TRIGGER vm_capture_update;
ALTER TABLE public.notes SET UNLOGGED;
UPDATE public.notes SET body = 'while unlogged';
ALTER TABLE public.notes SET LOGGED;
ALTER TABLE public.items REPLICA IDENTITY NOTHING;
DELETE FROM public.items WHERE id = 2;
SELECT pg_logical_emit_message(true, 'vm', '{"user_id": "forged"}') IS NOT NULL;
COMMIT;
-- @conn su postgres a1
-- @step L04 any what the slot saw (the vm.outbox rows of the capture left out)
SELECT string_agg(data, ' || ') FROM pg_logical_slot_get_changes('s', NULL, NULL)
 WHERE data NOT LIKE 'table vm.%';
-- @step L05 ok
SELECT pg_drop_replication_slot('s') IS NULL;
-- @conn mig a1_mig a1
-- @step L06 ok with max_prepared_transactions set, a migration can detach its transaction
BEGIN;
-- @attach migration 0010_prepared
-- @step L06a ok
INSERT INTO public.notes VALUES (9, NULL, 'prepared'); PREPARE TRANSACTION 'mine';
-- @tx L06b idle
-- @step L07 ok and commit it later itself
COMMIT PREPARED 'mine';
