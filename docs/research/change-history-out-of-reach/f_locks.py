"""Part F: does every change of a table's structure or capture leave, until the
transaction ends, a lock on a relation that reading and writing rows never takes?
f_locks.py PORT (a cluster prepared by f-left-behind.sh)."""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pq import Conn
port = int(sys.argv[1])
STRONG = """SELECT count(*), coalesce(string_agg(DISTINCT l.mode, ','), '') FROM pg_catalog.pg_locks l
             WHERE l.pid = pg_catalog.pg_backend_pid() AND l.locktype = 'relation'
               AND l.mode IN ('ShareUpdateExclusiveLock','ShareLock','ShareRowExclusiveLock','ExclusiveLock','AccessExclusiveLock')"""
mig = Conn(port, "a1_mig", "a1"); vm = Conn(port, "a1_vm", "a1"); rt = Conn(port, "a1_rt", "a1")
r = mig.one("""
CREATE TABLE part (id int, v text) PARTITION BY RANGE (id);
CREATE TABLE part1 (id int, v text);
CREATE TABLE part2 PARTITION OF part FOR VALUES FROM (100) TO (200);
CREATE TABLE plain (id int PRIMARY KEY, v text, n int);
CREATE SEQUENCE sq; GRANT USAGE ON SEQUENCE sq TO a1_rt;
CREATE TABLE honest_parent (id int PRIMARY KEY);
CREATE TABLE honest_child (id int PRIMARY KEY, p int REFERENCES honest_parent ON DELETE CASCADE ON UPDATE CASCADE CHECK (p > 0), touched timestamptz);
CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN NEW.touched := now(); RETURN NEW; END $f$;
CREATE TRIGGER t BEFORE UPDATE ON honest_child FOR EACH ROW EXECUTE FUNCTION touch();
INSERT INTO honest_parent SELECT g FROM generate_series(1, 50) g; INSERT INTO honest_child SELECT g, g FROM generate_series(1, 50) g;
INSERT INTO plain SELECT g, 'x', g FROM generate_series(1, 50) g;
CREATE MATERIALIZED VIEW mv2 AS SELECT count(*) FROM plain;
CREATE VIEW vw AS SELECT * FROM plain;
CREATE TABLE loose (id int, v text, n int); INSERT INTO loose VALUES (1, 'x', 1);
CREATE TABLE inh_parent (id int); CREATE TABLE inh_child (id int);
""")
assert r.ok, r
def case(conn, label, sql, expect_strong):
    conn.one("BEGIN")
    r = conn.one(sql)
    n, modes = conn.run(STRONG)[0].rows[0] if r.ok else ("-", "")
    conn.one("ROLLBACK")
    verdict = "as-expected" if r.ok and ((int(n) > 0) == expect_strong) else ("FAILED-TO-RUN " + str(r) if not r.ok else "UNEXPECTED")
    print(f"{verdict:12} strong={n:>2} {modes:45} {label}")
print("== changes of structure or capture (as the migration role: the rights such code runs with)")
for label, sql in [
    ("disable a capture trigger", "ALTER TABLE victim DISABLE TRIGGER vm_capture_update"),
    ("disable and enable", "ALTER TABLE victim DISABLE TRIGGER vm_capture_update; ALTER TABLE victim ENABLE TRIGGER vm_capture_update"),
    ("disable all triggers", "ALTER TABLE victim DISABLE TRIGGER USER"),
    ("drop a capture trigger", "DROP TRIGGER vm_capture_update ON victim"),
    ("rename a capture trigger", "ALTER TRIGGER vm_capture_update ON victim RENAME TO x"),
    ("a deferrable constraint trigger", "CREATE CONSTRAINT TRIGGER late AFTER INSERT ON plain DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION touch()"),
    ("a foreign key made deferrable", "ALTER TABLE honest_child ALTER CONSTRAINT honest_child_p_fkey DEFERRABLE"),
    ("a column's type", "ALTER TABLE loose ALTER COLUMN n TYPE bigint USING 0"),
    ("add a column", "ALTER TABLE plain ADD COLUMN z int DEFAULT 7"),
    ("drop a column", "ALTER TABLE loose DROP COLUMN n"),
    ("rename a column", "ALTER TABLE plain RENAME COLUMN n TO m"),
    ("create a table", "CREATE TABLE fresh (id int)"),
    ("create a table as", "CREATE TABLE fresh2 AS SELECT * FROM plain"),
    ("create and drop a table", "CREATE TABLE fresh3 (id int); DROP TABLE fresh3"),
    ("drop a table", "DROP TABLE plain CASCADE"),
    ("rename a table", "ALTER TABLE plain RENAME TO plain2"),
    ("move a table", "CREATE SCHEMA other; ALTER TABLE plain SET SCHEMA other"),
    ("attach a partition", "ALTER TABLE part ATTACH PARTITION part1 FOR VALUES FROM (0) TO (100)"),
    ("detach a partition", "ALTER TABLE part DETACH PARTITION part2"),
    ("inherit", "ALTER TABLE inh_child INHERIT inh_parent"),
    ("unlogged", "ALTER TABLE plain SET UNLOGGED"),
    ("row security", "ALTER TABLE plain ENABLE ROW LEVEL SECURITY"),
    ("a rule", "CREATE RULE r AS ON UPDATE TO plain DO INSTEAD NOTHING"),
    ("an index", "CREATE INDEX ON plain (v)"),
    ("replace a view", "CREATE OR REPLACE VIEW vw AS SELECT *, 1 AS one FROM plain"),
    ("truncate", "TRUNCATE plain"),
    ("lock table share", "LOCK TABLE plain IN SHARE MODE"),
    ("analyze", "ANALYZE plain"),
    ("refresh a materialized view", "REFRESH MATERIALIZED VIEW mv2"),
    ("alter a sequence", "ALTER SEQUENCE sq RESTART 5"),
    ("in a block that fails and is caught, then nothing", "DO $d$ BEGIN BEGIN ALTER TABLE victim DISABLE TRIGGER vm_capture_update; RAISE EXCEPTION 'x'; EXCEPTION WHEN OTHERS THEN NULL; END; END $d$"),
]:
    case(mig, label, sql, label != "in a block that fails and is caught, then nothing")
print("== reading and writing rows (as the runtime role)")
for label, sql in [
    ("insert, update, delete", "INSERT INTO plain VALUES (1000, 'a', 1); UPDATE plain SET v = 'b' WHERE id < 10; DELETE FROM plain WHERE id = 1000"),
    ("update of a captured table (the capture fires)", "UPDATE victim SET v = 'seen' WHERE id = 1"),
    ("cascading delete, update with trigger and check", "DELETE FROM honest_parent WHERE id < 5; UPDATE honest_parent SET id = id + 1000 WHERE id > 40; UPDATE honest_child SET p = p WHERE id = 20"),
    ("select for update, for share", "SELECT * FROM plain WHERE id = 1 FOR UPDATE; SELECT * FROM plain WHERE id = 2 FOR KEY SHARE"),
    ("on conflict", "INSERT INTO plain VALUES (1, 'c', 1) ON CONFLICT (id) DO UPDATE SET v = excluded.v"),
    ("nextval", "SELECT nextval('sq')"),
    ("a view, a materialized view read", "SELECT * FROM vw LIMIT 1; SELECT * FROM mv2"),
    ("serializable", "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; SELECT count(*) FROM plain; UPDATE plain SET n = n + 1 WHERE id = 3"),
    ("lock table row exclusive", "LOCK TABLE plain IN ROW EXCLUSIVE MODE"),
    ("advisory lock", "SELECT pg_advisory_xact_lock(1)"),
    ("partitioned insert", "INSERT INTO part VALUES (150, 'p')"),
]:
    case(rt, label, sql, False)
