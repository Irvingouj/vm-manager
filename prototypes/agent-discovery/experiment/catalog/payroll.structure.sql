create table pay_runs (
  id serial primary key,
  period_start date not null,
  period_end date not null,
  paid_on date,
  status text not null default 'draft'
);

create table payslips (
  id serial primary key,
  pay_run_id integer not null references pay_runs(id),
  employee_email text not null,
  gross_pence bigint not null,
  net_pence bigint not null,
  cost_centre text not null
);
