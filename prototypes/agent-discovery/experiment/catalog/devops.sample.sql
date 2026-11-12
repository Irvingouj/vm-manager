insert into teams (name, chat_channel) values
  ('Platform', '#team-platform'),
  ('Payments', '#team-payments'),
  ('Workplace IT', '#workplace-it');

insert into services (name, team_id, tier, runbook_url) values
  ('checkout-api', 2, 'critical', 'https://wiki.example.com/runbooks/checkout-api'),
  ('identity', 1, 'critical', 'https://wiki.example.com/runbooks/identity'),
  ('office-wifi', 3, 'internal', 'https://wiki.example.com/runbooks/office-wifi'),
  ('badge-readers', 3, 'internal', null);

insert into incidents (service_id, title, severity, status, opened_at, acknowledged_by, acknowledged_at) values
  (1, 'Checkout latency above 2s', 'sev2', 'open', now() - interval '40 minutes', null, null),
  (3, 'Wi-Fi down on London floor 2', 'sev3', 'acknowledged', now() - interval '3 hours', 'marco@example.com', now() - interval '2 hours'),
  (4, 'Badge readers rejecting valid badges at AMS1', 'sev2', 'open', now() - interval '15 minutes', null, null),
  (2, 'Login errors for some users', 'sev1', 'resolved', now() - interval '3 days', 'li@example.com', now() - interval '3 days');

insert into incident_updates (incident_id, author, body) values
  (2, 'marco@example.com', 'Access point 2.3 replaced, monitoring.'),
  (4, 'li@example.com', 'Rolled back identity release 4.12.');
