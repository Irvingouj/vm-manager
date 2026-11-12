create type room_kind as enum ('meeting', 'office', 'kitchen', 'storage', 'lab', 'other');
create type request_status as enum ('open', 'assigned', 'waiting_parts', 'done', 'cancelled');
create type request_priority as enum ('low', 'normal', 'high', 'urgent');

create table buildings (
  id serial primary key,
  code text not null unique,
  name text not null,
  city text not null,
  address text not null
);

create table rooms (
  id serial primary key,
  building_id integer not null references buildings(id),
  floor integer not null,
  code text not null,
  name text,
  kind room_kind not null default 'meeting',
  capacity integer,
  bookable boolean not null default false,
  unique (building_id, code)
);

create table assets (
  id serial primary key,
  tag text not null unique,
  description text not null,
  category text not null,
  room_id integer references rooms(id),
  purchased_on date,
  retired boolean not null default false
);

create table maintenance_requests (
  id serial primary key,
  room_id integer references rooms(id),
  asset_id integer references assets(id),
  title text not null,
  details text,
  priority request_priority not null default 'normal',
  status request_status not null default 'open',
  reported_by text not null,
  assigned_to text,
  created_at timestamptz not null default now(),
  closed_at timestamptz
);
create index maintenance_requests_status_idx on maintenance_requests (status);
