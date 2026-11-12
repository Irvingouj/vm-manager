insert into buildings (code, name, city, address) values
  ('LDN1', 'London Bankside', 'London', '12 Sumner Street'),
  ('AMS1', 'Amsterdam Docks', 'Amsterdam', 'Piet Heinkade 55');

insert into rooms (building_id, floor, code, name, kind, capacity, bookable) values
  (1, 2, '2.01', 'Thames', 'meeting', 8, true),
  (1, 2, '2.02', 'Fleet', 'meeting', 4, true),
  (1, 3, '3.10', null, 'kitchen', null, false),
  (2, 1, '1.04', 'IJ', 'meeting', 12, true),
  (2, 0, '0.02', 'Server room', 'storage', null, false);

insert into assets (tag, description, category, room_id, purchased_on) values
  ('AV-0042', 'Ceiling projector', 'av', 1, '2023-03-14'),
  ('AV-0043', 'Video conference bar', 'av', 2, '2024-01-09'),
  ('KT-0007', 'Coffee machine', 'kitchen', 3, '2022-06-01'),
  ('HV-0101', 'Air conditioning unit', 'hvac', 5, '2021-11-20');

insert into maintenance_requests (room_id, asset_id, title, details, priority, status, reported_by, assigned_to, created_at) values
  (1, 1, 'Projector flickers', 'Image flickers after 10 minutes', 'normal', 'open', 'priya@example.com', null, now() - interval '2 days'),
  (3, 3, 'Coffee machine leaking', null, 'high', 'assigned', 'tom@example.com', 'dana@example.com', now() - interval '1 day'),
  (5, 4, 'Server room too warm', 'Thermometer shows 29C', 'urgent', 'waiting_parts', 'oncall@example.com', 'marco@example.com', now() - interval '5 hours');
