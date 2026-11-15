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
# Added by the implementation agent of ticket 63, second round: what tells a
# table made and written anew in one statement from one only made.
def made(label, t, sql):
    r = c.one(sql)
    if not r.ok:
        print(f"{label}: ERROR {r.sqlstate} {r.message}")
        return
    same = c.one(f"SELECT relfilenode = oid FROM pg_class WHERE oid = '{t}'::regclass").rows[0][0]
    print(f"{label}: file node is the table's oid: {same}")
made("CREATE TABLE, only made", "m1", "CREATE TABLE m1 (id int PRIMARY KEY, a int)")
made("CREATE TABLE AS, only made", "m2", "CREATE TABLE m2 AS SELECT g AS id FROM generate_series(1, 10) g")
c.one("CREATE TABLE q (id int, a int, PRIMARY KEY (id)) PARTITION BY RANGE (id)")
made("a partition made, only made", "q1", "CREATE TABLE q1 PARTITION OF q FOR VALUES FROM (0) TO (100)")
made("a partition made and filled in one statement", "q2",
     "DO $$ BEGIN CREATE TABLE q2 PARTITION OF q FOR VALUES FROM (100) TO (200); INSERT INTO q VALUES (150, 1); END $$")
made("a partition made, filled and its column retyped through the partitioned table in one statement", "q3",
     "DO $$ BEGIN CREATE TABLE q3 PARTITION OF q FOR VALUES FROM (200) TO (300); INSERT INTO q VALUES (250, 1); ALTER TABLE q ALTER COLUMN a TYPE bigint USING a + 1; END $$")
made("a table made and written anew by SET UNLOGGED in one statement", "m3",
     "DO $$ BEGIN CREATE TABLE m3 (id int); ALTER TABLE m3 SET UNLOGGED; END $$")
c.one("CREATE TABLE m4 (id int NOT NULL, a bigint)")
made("a table made earlier, attached as a partition", "m4", "ALTER TABLE q ATTACH PARTITION m4 FOR VALUES FROM (300) TO (400)")
show("the partitioned table's file node, and its partitions after the partition made",
     "SELECT pg_relation_filenode('q'), (SELECT array_agg(inhrelid::regclass::text ORDER BY inhrelid) FROM pg_inherits WHERE inhparent = 'q'::regclass)")
# Added by the implementation agent of ticket 63, second round, after review:
# whether this transaction's own count of rows written in pg_inherits tells
# every change of a table's partitions, without reading them.
def count():
    return c.one("SELECT pg_stat_get_xact_tuples_inserted('pg_inherits'::regclass) + pg_stat_get_xact_tuples_deleted('pg_inherits'::regclass) + pg_stat_get_xact_tuples_updated('pg_inherits'::regclass)").rows[0][0]
def counted(label, sql):
    a = count(); r = c.one(sql); b = count()
    print(f"{label}: {'ok' if r.ok else 'ERROR ' + str(r.sqlstate) + ' ' + str(r.message)}; this transaction's count of pg_inherits rows written, before and after: {a} {b}")
show("track_counts, and whether the role is a superuser", "SELECT current_setting('track_counts'), rolsuper FROM pg_roles WHERE rolname = current_user")
c.one("CREATE TABLE w (id int, a int, PRIMARY KEY (id)) PARTITION BY RANGE (id)")
c.one("CREATE TABLE w9 (id int NOT NULL, a int)")
c.one("BEGIN")
counted("in a transaction, a SELECT 1", "SELECT 1")
counted("a partition made", "CREATE TABLE w1 PARTITION OF w FOR VALUES FROM (0) TO (10)")
counted("a partition made in a DO block", "DO $$ BEGIN CREATE TABLE w2 PARTITION OF w FOR VALUES FROM (10) TO (20); END $$")
counted("a table attached", "ALTER TABLE w ATTACH PARTITION w9 FOR VALUES FROM (90) TO (100)")
counted("a partition detached", "ALTER TABLE w DETACH PARTITION w9")
counted("a partition dropped", "DROP TABLE w2")
counted("a partition made in a block that fails and is caught", "DO $$ BEGIN BEGIN CREATE TABLE w3 PARTITION OF w FOR VALUES FROM (20) TO (30); RAISE EXCEPTION 'x'; EXCEPTION WHEN OTHERS THEN NULL; END; END $$")
c.one("SAVEPOINT s")
counted("a partition made after a savepoint", "CREATE TABLE w4 PARTITION OF w FOR VALUES FROM (40) TO (50)")
counted("rolled back to the savepoint", "ROLLBACK TO SAVEPOINT s")
counted("a detach and an attach in one DO block", "DO $$ BEGIN ALTER TABLE w DETACH PARTITION w1; ALTER TABLE w ATTACH PARTITION w9 FOR VALUES FROM (0) TO (10); END $$")
counted("a table made, not a partition", "CREATE TABLE w5 (id int)")
show("the partitions at the end", "SELECT array_agg(inhrelid::regclass::text ORDER BY inhrelid::regclass::text) FROM pg_inherits WHERE inhparent = 'w'::regclass")
c.one("ROLLBACK")
