insert into teams (name, department) values
  ('Facilities', 'Operations'),
  ('Site Reliability', 'Engineering'),
  ('People Operations', 'People');

insert into employees (email, full_name, preferred_name, job_title, team_id, manager_id, office, employment, start_date) values
  ('ines@example.com', 'Ines Moreau', null, 'Head of Facilities', 1, null, 'LDN1', 'permanent', '2019-04-01'),
  ('dana@example.com', 'Dana Whitfield', null, 'Facilities Coordinator', 1, 1, 'LDN1', 'permanent', '2022-09-12'),
  ('marco@example.com', 'Marco Bianchi', null, 'Site Reliability Engineer', 2, null, 'AMS1', 'permanent', '2021-02-01'),
  ('noor@example.com', 'Noor Haddad', null, 'People Partner', 3, null, 'LDN1', 'permanent', '2020-07-15'),
  ('sam@example.com', 'Samuel Okoro', 'Sam', 'Facilities Assistant', 1, 1, 'AMS1', 'contractor', '2026-11-02');

update teams set lead_employee_id = 1 where id = 1;
update teams set lead_employee_id = 3 where id = 2;
update teams set lead_employee_id = 4 where id = 3;
