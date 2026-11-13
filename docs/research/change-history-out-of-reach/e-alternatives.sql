-- Part E, on a cluster with Postgres's default settings: what logical
-- decoding and two-phase commit would need, from a platform role with only
-- CREATEDB and CREATEROLE.

-- @conn plat plat postgres
-- @step E01 has:replica defaults that matter
SELECT current_setting('wal_level') AS wal_level, current_setting('max_replication_slots') AS slots,
       current_setting('max_wal_senders') AS senders, current_setting('max_prepared_transactions') AS prepared;
-- @step E02 err:42501 a CREATEROLE role creates a role with REPLICATION
CREATE ROLE r_repl LOGIN REPLICATION;
-- @step E03 err:42501 or gives it to a role it made
ALTER ROLE a1_vm REPLICATION;
-- @step E04 err:42501 BYPASSRLS likewise
CREATE ROLE r_bypass BYPASSRLS;
-- @step E05 err:42501 nor can it change wal_level
ALTER SYSTEM SET wal_level = logical;
-- @step E06 err:42501 a logical slot without REPLICATION
SELECT pg_create_logical_replication_slot('s', 'test_decoding');
-- @conn vm a1_vm a1
-- @step E07 err:42501 the database's owner: a publication of all tables
CREATE PUBLICATION p_all FOR ALL TABLES;
-- @step E08 err:42501 or of every table in a schema
CREATE PUBLICATION p_schema FOR TABLES IN SCHEMA public;
-- @conn mig a1_mig a1
-- @step E09 ok a table's owner: a publication of its table (with a warning about wal_level)
BEGIN; CREATE PUBLICATION p_items FOR TABLE public.items; ROLLBACK;
-- @step E10 ok a table's owner sets its replica identity
BEGIN; ALTER TABLE public.items REPLICA IDENTITY NOTHING; ROLLBACK;
-- @step E11 ok anyone may write a message into the WAL
SELECT pg_logical_emit_message(true, 'vm', '{"user_id": "forged"}') IS NOT NULL;
-- @conn rt a1_rt a1
-- @step E12 ok the runtime role too
SELECT pg_logical_emit_message(true, 'vm', '{"user_id": "forged"}') IS NOT NULL;
-- @conn su postgres a1
-- @step E13 err:55000 two-phase commit, even for the superuser, under the defaults
BEGIN; PREPARE TRANSACTION 'x';
