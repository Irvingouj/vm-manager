create type service_tier as enum ('critical', 'standard', 'internal');
create type incident_severity as enum ('sev1', 'sev2', 'sev3', 'sev4');
create type incident_status as enum ('open', 'acknowledged', 'mitigated', 'resolved');

create table teams (
  id serial primary key,
  name text not null unique,
  chat_channel text
);

create table services (
  id serial primary key,
  name text not null unique,
  team_id integer not null references teams(id),
  tier service_tier not null default 'standard',
  runbook_url text
);

create table incidents (
  id serial primary key,
  service_id integer not null references services(id),
  title text not null,
  severity incident_severity not null,
  status incident_status not null default 'open',
  opened_at timestamptz not null default now(),
  acknowledged_by text,
  acknowledged_at timestamptz,
  resolved_at timestamptz,
  summary text
);
create index incidents_status_idx on incidents (status);

create table incident_updates (
  id serial primary key,
  incident_id integer not null references incidents(id) on delete cascade,
  author text not null,
  body text not null,
  created_at timestamptz not null default now()
);
