# Facts about table rewrites that ticket 63's contract states, checked on a real server.
import sys
import os; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pq import Conn
port = int(sys.argv[1])
c = Conn(port, "owner", "d", password="owner")
def q(sql):
    r = c.one(sql)
    return r
def show(label, sql):
    r = c.one(sql)
    print(f"{label}: {'ok ' + repr(r.rows) if r.ok else 'ERROR ' + str(r.sqlstate) + ' ' + str(r.message)}")
def node(t):
    return c.one(f"SELECT relfilenode FROM pg_class WHERE oid = '{t}'::regclass").rows[0][0]
def rewrites(label, t, sql):
    a = node(t); r = c.one(sql); b = node(t)
    print(f"{label}: {'ok' if r.ok else 'ERROR ' + str(r.sqlstate) + ' ' + str(r.message)}; new file node: {a != b}")
print("version", c.one("SHOW server_version").rows)
c.one("SET TimeZone = 'UTC'")
c.one("CREATE TABLE t (id int PRIMARY KEY, a int, v varchar(50), n numeric(10,2), ts timestamp)")
c.one("INSERT INTO t VALUES (1, 5, 'x', 1.50, '2024-01-01 00:00')")
rewrites("integer to bigint", "t", "ALTER TABLE t ALTER COLUMN a TYPE bigint")
rewrites("varchar(50) to text", "t", "ALTER TABLE t ALTER COLUMN v TYPE text")
rewrites("numeric(10,2) to numeric(12,4)", "t", "ALTER TABLE t ALTER COLUMN n TYPE numeric(12,4)")
show("the numeric after, as jsonb", "SELECT to_jsonb(n), '1.50'::jsonb = to_jsonb(n) FROM t")
rewrites("timestamp to timestamptz, TimeZone UTC", "t", "ALTER TABLE t ALTER COLUMN ts TYPE timestamptz")
rewrites("USING 0 to the type it had", "t", "ALTER TABLE t ALTER COLUMN a TYPE bigint USING 0")
rewrites("SET UNLOGGED", "t", "ALTER TABLE t SET UNLOGGED")
rewrites("ADD COLUMN with a volatile default", "t", "ALTER TABLE t ADD COLUMN r float8 DEFAULT random()")
rewrites("ADD COLUMN with a constant default", "t", "ALTER TABLE t ADD COLUMN k int DEFAULT 7")
rewrites("the same through a DO block", "t", "DO $$ BEGIN EXECUTE 'ALTER TABLE t ALTER COLUMN a TYPE numeric USING a + 1'; END $$")
show("jsonb 5 = \"5\"", "SELECT '5'::jsonb = '\"5\"'::jsonb, '12'::jsonb = '12.0'::jsonb")
# row security
c.one("CREATE TABLE s (id int PRIMARY KEY, a int)"); c.one("INSERT INTO s VALUES (1, 1), (2, 2)")
c.one("ALTER TABLE s ENABLE ROW LEVEL SECURITY")
show("enabled, not forced: the owner counts", "SELECT count(*), row_security_active('s') FROM s")
c.one("ALTER TABLE s FORCE ROW LEVEL SECURITY")
show("forced, no policy: the owner counts", "SELECT count(*), row_security_active('s') FROM s")
c.one("BEGIN"); c.one("SET LOCAL row_security = off")
show("forced, row_security off", "SELECT count(*) FROM s"); c.one("ROLLBACK")
# partitions
c.one("CREATE TABLE p (id int, a int, PRIMARY KEY (id)) PARTITION BY RANGE (id)")
c.one("CREATE TABLE p1 PARTITION OF p FOR VALUES FROM (0) TO (100)"); c.one("INSERT INTO p VALUES (1, 1)")
show("a partition's column retyped by itself", "ALTER TABLE p1 ALTER COLUMN a TYPE numeric USING 0")
rewrites("through the partitioned table, the partition", "p1", "ALTER TABLE p ALTER COLUMN a TYPE numeric USING 0")
# pg_input_is_valid, virtual generated columns
show("pg_input_is_valid", "SELECT pg_input_is_valid('abc', 'integer'), pg_input_is_valid('5', 'integer')")
r = c.one("CREATE TABLE g (id int PRIMARY KEY, a int, b int GENERATED ALWAYS AS (a * 2) VIRTUAL)")
print("a virtual generated column:", "made" if r.ok else f"ERROR {r.sqlstate} {r.message}")
if r.ok:
    c.one("INSERT INTO g (id, a) VALUES (1, 2)")
    show("read from the table", "SELECT to_jsonb(g) FROM g")
    c.one("CREATE FUNCTION f(int) RETURNS int LANGUAGE sql IMMUTABLE AS 'SELECT $1'")
    show("one that calls the Application's function", "ALTER TABLE g ADD COLUMN c int GENERATED ALWAYS AS (f(a)) VIRTUAL")
    rewrites("a virtual column's expression changed", "g", "ALTER TABLE g ALTER COLUMN b SET EXPRESSION AS (a * 3)")
r = c.one("CREATE TABLE h (id int PRIMARY KEY, a int, b int GENERATED ALWAYS AS (a * 2) STORED)")
rewrites("a stored generated column's expression changed", "h", "ALTER TABLE h ALTER COLUMN b SET EXPRESSION AS (a * 3)")
