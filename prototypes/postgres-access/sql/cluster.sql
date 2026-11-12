-- Run as the bootstrap superuser on database "postgres".
-- Throwaway local passwords only.

-- Shared database, owned by IT (the platform does not own it).
CREATE ROLE it_owner LOGIN PASSWORD 'it';            -- IT's own schema owner
CREATE ROLE vm_read LOGIN PASSWORD 'read';           -- handed over by IT: read level
CREATE ROLE vm_write LOGIN PASSWORD 'write';         -- handed over by IT: write level
CREATE ROLE vm_struct LOGIN PASSWORD 'struct';       -- optional: structure level, no SELECT
CREATE DATABASE shared_db OWNER it_owner;

-- Application-owned database (the platform owns it).
CREATE ROLE app1_migrator LOGIN PASSWORD 'migrator'; -- platform: applies migrations, owns everything
CREATE ROLE app1_rt LOGIN PASSWORD 'rt';             -- the Application's runtime role
CREATE DATABASE app1_db OWNER app1_migrator;

-- Rejected design kept for the attack: one host login that SET ROLEs down.
CREATE ROLE host_login LOGIN PASSWORD 'host';
GRANT app1_rt TO host_login;
