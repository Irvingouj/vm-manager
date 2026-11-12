-- Run as app1_migrator (the platform's migration role, owner of app1_db) on
-- app1_db. In the product this is what the platform applies at deploy: the
-- Application's own schema (generated from its TypeScript definition) plus the
-- platform's `vm` schema for identity and Change history.

REVOKE ALL ON DATABASE app1_db FROM PUBLIC;
GRANT CONNECT ON DATABASE app1_db TO app1_rt;      -- no TEMP: no temp tables/functions
REVOKE ALL ON SCHEMA public FROM PUBLIC;

-- ------------------------------------------------------------- platform: vm
CREATE SCHEMA vm;

-- attach_hash = sha256(secret the host holds); k1, k2 = HMAC-style keys.
-- Filled in by the harness with random values.
CREATE TABLE vm.keys (attach_hash bytea NOT NULL, k1 bytea NOT NULL, k2 bytea NOT NULL);

-- A value unique to the current transaction that Application SQL cannot
-- choose: backend pid, virtual transaction id (assigned to every transaction,
-- no xid consumed), and transaction start time.
CREATE FUNCTION vm._txkey() RETURNS text LANGUAGE sql VOLATILE
  SET search_path = pg_catalog AS $$
  SELECT pg_backend_pid()::text || '/' ||
         (SELECT virtualtransaction FROM pg_catalog.pg_locks
           WHERE pid = pg_backend_pid() AND locktype = 'virtualxid') || '/' ||
         transaction_timestamp()::text
$$;

-- Keyed tag: sha256(k2 || sha256(k1 || m)) (NMAC shape; built-in sha256 only,
-- so no pgcrypto needed).
CREATE FUNCTION vm._tag(m text) RETURNS text LANGUAGE sql STABLE SECURITY DEFINER
  SET search_path = pg_catalog AS $$
  SELECT encode(sha256(k.k2 || sha256(k.k1 || convert_to(m, 'UTF8'))), 'hex') FROM vm.keys k
$$;

-- Candidate B (recommended): host calls this first in every transaction.
CREATE FUNCTION vm.attach(p_user text, p_app text, p_secret text) RETURNS void
  LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE t text := vm._txkey();
BEGIN
  IF NOT EXISTS (SELECT 1 FROM vm.keys WHERE attach_hash = sha256(convert_to(p_secret, 'UTF8'))) THEN
    RAISE EXCEPTION 'vm.attach: refused' USING ERRCODE = '42501';
  END IF;
  PERFORM set_config('vm.ctx', jsonb_build_object(
      'u', p_user, 'a', p_app, 't', t,
      's', vm._tag(jsonb_build_array(p_user, p_app, t)::text))::text, true);
END $$;

CREATE FUNCTION vm.identity(OUT user_id text, OUT app_id text)
  LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE c jsonb;
BEGIN
  BEGIN
    c := nullif(current_setting('vm.ctx', true), '')::jsonb;
  EXCEPTION WHEN others THEN c := NULL;
  END;
  IF c IS NULL
     OR c->>'t' IS DISTINCT FROM vm._txkey()
     OR c->>'s' IS DISTINCT FROM vm._tag(jsonb_build_array(c->>'u', c->>'a', c->>'t')::text) THEN
    RAISE EXCEPTION 'vm.identity: no valid identity attached to this transaction' USING ERRCODE = '42501';
  END IF;
  user_id := c->>'u';
  app_id := c->>'a';
END $$;

-- Candidate C: platform-owned table keyed by backend + transaction.
CREATE UNLOGGED TABLE vm.attached (pid int PRIMARY KEY, txkey text NOT NULL, user_id text NOT NULL, app_id text NOT NULL);

CREATE FUNCTION vm.attach_c(p_user text, p_app text, p_secret text) RETURNS void
  LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM vm.keys WHERE attach_hash = sha256(convert_to(p_secret, 'UTF8'))) THEN
    RAISE EXCEPTION 'vm.attach_c: refused' USING ERRCODE = '42501';
  END IF;
  INSERT INTO vm.attached VALUES (pg_backend_pid(), vm._txkey(), p_user, p_app)
    ON CONFLICT (pid) DO UPDATE SET txkey = EXCLUDED.txkey, user_id = EXCLUDED.user_id, app_id = EXCLUDED.app_id;
END $$;

CREATE FUNCTION vm.identity_c(OUT user_id text, OUT app_id text)
  LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
  SELECT a.user_id, a.app_id INTO user_id, app_id FROM vm.attached a
   WHERE a.pid = pg_backend_pid() AND a.txkey = vm._txkey();
  IF NOT FOUND THEN
    RAISE EXCEPTION 'vm.identity_c: no identity attached' USING ERRCODE = '42501';
  END IF;
END $$;

-- Change history
CREATE TABLE vm.history (
  id       bigserial PRIMARY KEY,
  at       timestamptz NOT NULL DEFAULT clock_timestamp(),
  txid     xid8 NOT NULL DEFAULT pg_current_xact_id(),
  tbl      text NOT NULL,
  op       text NOT NULL,
  old_row  jsonb,
  new_row  jsonb,
  user_id  text NOT NULL,
  app_id   text NOT NULL
);

CREATE FUNCTION vm.capture() RETURNS trigger
  LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE i record;
BEGIN
  SELECT * INTO i FROM vm.identity();
  INSERT INTO vm.history (tbl, op, old_row, new_row, user_id, app_id)
  VALUES (TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME, TG_OP,
          CASE WHEN TG_OP <> 'INSERT' THEN to_jsonb(OLD) END,
          CASE WHEN TG_OP <> 'DELETE' THEN to_jsonb(NEW) END,
          i.user_id, i.app_id);
  RETURN NULL;
END $$;

-- -------------------------------------------------------- the Application's
CREATE SCHEMA app;
CREATE TABLE app.orders (id serial PRIMARY KEY, item text NOT NULL, qty int NOT NULL, approved_by text);
CREATE TRIGGER orders_history AFTER INSERT OR UPDATE OR DELETE ON app.orders
  FOR EACH ROW EXECUTE FUNCTION vm.capture();
CREATE TABLE app.bench (id int PRIMARY KEY, v int NOT NULL);
INSERT INTO app.bench SELECT g, 0 FROM generate_series(1, 1000) g;
CREATE TRIGGER bench_history AFTER INSERT OR UPDATE OR DELETE ON app.bench
  FOR EACH ROW EXECUTE FUNCTION vm.capture();
-- Cost comparison only: same trigger body but identity read unverified.
CREATE FUNCTION vm.capture_unverified() RETURNS trigger
  LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
  INSERT INTO vm.history (tbl, op, old_row, new_row, user_id, app_id)
  VALUES (TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME, TG_OP,
          CASE WHEN TG_OP <> 'INSERT' THEN to_jsonb(OLD) END,
          CASE WHEN TG_OP <> 'DELETE' THEN to_jsonb(NEW) END,
          coalesce(current_setting('vm.ctx', true), '?'), '?');
  RETURN NULL;
END $$;
CREATE TABLE app.bench_unv (id int PRIMARY KEY, v int NOT NULL);
INSERT INTO app.bench_unv SELECT g, 0 FROM generate_series(1, 1000) g;
CREATE TRIGGER bench_unv_history AFTER INSERT OR UPDATE OR DELETE ON app.bench_unv
  FOR EACH ROW EXECUTE FUNCTION vm.capture_unverified();
-- Same shape, no trigger: baseline for the cost measurement.
CREATE TABLE app.plain (id int PRIMARY KEY, v int NOT NULL);
INSERT INTO app.plain SELECT g, 0 FROM generate_series(1, 1000) g;

-- A row-level-security policy reading the same identity (what a Schema owner
-- of a shared database could write if it installed vm.identity()).
CREATE TABLE app.notes (id serial PRIMARY KEY, owner text NOT NULL, body text NOT NULL);
INSERT INTO app.notes (owner, body) VALUES ('alice@corp', 'alice-note'), ('bob@corp', 'bob-note');
ALTER TABLE app.notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY own_rows ON app.notes USING (owner = (SELECT user_id FROM vm.identity()));

-- --------------------------------------------------------------- privileges
GRANT USAGE ON SCHEMA app, vm TO app1_rt;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA app TO app1_rt;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA app TO app1_rt;
GRANT SELECT ON vm.history TO app1_rt;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA vm FROM PUBLIC;
GRANT EXECUTE ON FUNCTION vm.attach(text, text, text), vm.identity(),
                          vm.attach_c(text, text, text), vm.identity_c() TO app1_rt;
-- _txkey is harmless; _tag is the signing oracle and must stay private.
GRANT EXECUTE ON FUNCTION vm._txkey() TO app1_rt;

-- For the rejected "one host login + SET ROLE" design only: the host's own
-- login may read the keys (it stands for any role more powerful than app1_rt).
GRANT CONNECT ON DATABASE app1_db TO host_login;
GRANT SELECT ON vm.keys TO host_login;
