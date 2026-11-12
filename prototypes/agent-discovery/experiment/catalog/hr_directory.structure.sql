create type employment_type as enum ('permanent', 'contractor', 'intern');

create table teams (
  id serial primary key,
  name text not null unique,
  department text not null,
  lead_employee_id integer
);

create table employees (
  id serial primary key,
  email text not null unique,
  full_name text not null,
  preferred_name text,
  job_title text not null,
  team_id integer references teams(id),
  manager_id integer references employees(id),
  office text,
  employment employment_type not null default 'permanent',
  start_date date not null,
  end_date date
);
