-- Run as devops_owner on database devops: a shared database someone else
-- manages, with the things real ones have (an enum, a view, a function, a
-- check, an index, a policy, a comment, a second schema).
CREATE TYPE service_tier AS ENUM ('gold', 'silver', 'bronze');

CREATE TABLE teams (
  id serial PRIMARY KEY,
  name text NOT NULL UNIQUE
);

CREATE TABLE services (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  name text NOT NULL UNIQUE,
  team_id integer NOT NULL REFERENCES teams(id),
  tier service_tier NOT NULL DEFAULT 'bronze',
  config jsonb NOT NULL DEFAULT '{}',
  tags text[] NOT NULL DEFAULT '{}',
  monthly_cost numeric(10, 2) CHECK (monthly_cost >= 0),
  created_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE services IS 'Every running service';
CREATE INDEX services_team_idx ON services(team_id);

CREATE TABLE deployments (
  id bigserial PRIMARY KEY,
  service_id uuid NOT NULL REFERENCES services(id) ON DELETE CASCADE,
  version text NOT NULL,
  deployed_at timestamptz NOT NULL DEFAULT now(),
  ok boolean NOT NULL
);
ALTER TABLE deployments ENABLE ROW LEVEL SECURITY;
CREATE POLICY deployments_read ON deployments FOR SELECT USING (true);

CREATE VIEW service_summary AS
  SELECT s.name, t.name AS team, count(d.id) AS deployments
  FROM services s JOIN teams t ON t.id = s.team_id
  LEFT JOIN deployments d ON d.service_id = s.id
  GROUP BY s.name, t.name;

CREATE FUNCTION bump() RETURNS integer LANGUAGE sql AS 'select 1';

CREATE SCHEMA billing;
CREATE TABLE billing.invoices (id serial PRIMARY KEY, amount numeric(12, 2) NOT NULL);

INSERT INTO teams (name) VALUES ('platform'), ('payments');
INSERT INTO services (id, name, team_id, tier, config, tags, monthly_cost) VALUES
  ('11111111-1111-1111-1111-111111111111', 'gateway', 1, 'gold', '{"replicas": 3}', '{edge,public}', 1234.50),
  ('22222222-2222-2222-2222-222222222222', 'ledger', 2, 'silver', '{"replicas": 2}', '{core}', 99.99);
INSERT INTO deployments (service_id, version, ok) VALUES
  ('11111111-1111-1111-1111-111111111111', 'v1', true),
  ('11111111-1111-1111-1111-111111111111', 'v2', false),
  ('22222222-2222-2222-2222-222222222222', 'v7', true);

-- Access levels as roles: read = SELECT; structure = can connect and see the
-- schema, no rows; nothing = can connect only.
GRANT USAGE ON SCHEMA public, billing TO devops_reader, devops_structure;
GRANT SELECT ON ALL TABLES IN SCHEMA public, billing TO devops_reader;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION bump() FROM PUBLIC;
