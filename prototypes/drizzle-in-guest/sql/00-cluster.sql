-- Run as the superuser on database postgres. Throwaway local passwords.
DROP DATABASE IF EXISTS app;
DROP DATABASE IF EXISTS app_kitcheck;
DROP DATABASE IF EXISTS devops;
DROP ROLE IF EXISTS app_owner;
DROP ROLE IF EXISTS app_rt;
DROP ROLE IF EXISTS devops_owner;
DROP ROLE IF EXISTS devops_reader;
DROP ROLE IF EXISTS devops_structure;
DROP ROLE IF EXISTS devops_nothing;

-- The Application's own database: app_owner is the privileged role the
-- platform migrates with; app_rt is what the running Application gets.
CREATE ROLE app_owner LOGIN PASSWORD 'spike';
CREATE ROLE app_rt LOGIN PASSWORD 'spike';
CREATE DATABASE app OWNER app_owner;
CREATE DATABASE app_kitcheck OWNER app_owner;

-- A shared database owned outside the Application.
CREATE ROLE devops_owner LOGIN PASSWORD 'spike';
CREATE ROLE devops_reader LOGIN PASSWORD 'spike';
CREATE ROLE devops_structure LOGIN PASSWORD 'spike';
CREATE ROLE devops_nothing LOGIN PASSWORD 'spike';
CREATE DATABASE devops OWNER devops_owner;
