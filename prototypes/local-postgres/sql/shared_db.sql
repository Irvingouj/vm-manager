-- Run as it_owner on shared_db. This is IT's existing database; nothing here is
-- installed by the platform. The only platform-specific lines are the GRANTs
-- to the logins IT hands over.

CREATE SCHEMA hr;
CREATE TABLE hr.emp (
  id      int PRIMARY KEY,
  name    text NOT NULL,
  salary  int  NOT NULL CHECK (salary < 1000000),
  status  text NOT NULL DEFAULT 'active'
);
INSERT INTO hr.emp VALUES (1, 'Ada', 100), (2, 'Grace', 200), (3, 'Ms CEO', 900);
COMMENT ON COLUMN hr.emp.salary IS 'yearly, kEUR';
CREATE TABLE hr.dept (id int PRIMARY KEY, head int REFERENCES hr.emp(id));
-- A view whose definition embeds a data literal (a structure-level leak path).
CREATE VIEW hr.ceo AS SELECT id, salary FROM hr.emp WHERE name = 'Ms CEO';

-- A side-effecting SECURITY DEFINER function IT wrote for its own tools.
-- EXECUTE is granted to PUBLIC by default, as on any Postgres function.
CREATE FUNCTION hr.give_raise() RETURNS void LANGUAGE sql SECURITY DEFINER
  SET search_path = pg_catalog AS $$ UPDATE hr.emp SET salary = salary + 1 $$;

-- A schema the platform was not given anything on.
CREATE SCHEMA it_private;
CREATE TABLE it_private.secret (v text);
INSERT INTO it_private.secret VALUES ('top secret');
REVOKE ALL ON SCHEMA it_private FROM PUBLIC;

-- What IT hands over: three logins, privileges only.
GRANT USAGE ON SCHEMA hr TO vm_read, vm_write, vm_struct;
GRANT SELECT ON ALL TABLES IN SCHEMA hr TO vm_read;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA hr TO vm_write;
-- vm_struct: USAGE only, no SELECT on anything.

-- Note: TEMP on the database is still granted to PUBLIC (Postgres default),
-- and EXECUTE on pg_catalog functions such as pg_terminate_backend, lo_create,
-- pg_notify and set_config is PUBLIC. The checks show what that allows.
