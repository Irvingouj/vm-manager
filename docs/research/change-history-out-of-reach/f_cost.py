import sys, time
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from pq import Conn
port = int(sys.argv[1])
mig = Conn(port, "a1_mig", "a1"); vm = Conn(port, "a1_vm", "a1"); rt = Conn(port, "a1_rt", "a1")
DIGEST = """
SELECT md5(
 coalesce((SELECT string_agg(c.oid::text || c.relname || c.relnamespace::text || c.relowner::text || c.relkind::text || c.relpersistence::text || c.relispartition::text, ',' ORDER BY c.oid)
            FROM pg_catalog.pg_class c WHERE c.relkind IN ('r','p') AND c.relnamespace NOT IN (SELECT n.oid FROM pg_catalog.pg_namespace n WHERE n.nspname IN ('vm','pg_catalog','information_schema') OR n.nspname LIKE 'pg\\_%')), '')
 || coalesce((SELECT string_agg(a.attrelid::text || ':' || a.attnum::text || ':' || a.xmin::text, ',' ORDER BY a.attrelid, a.attnum)
            FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
           WHERE a.attnum > 0 AND c.relkind IN ('r','p') AND c.relnamespace NOT IN (SELECT n.oid FROM pg_catalog.pg_namespace n WHERE n.nspname IN ('vm','pg_catalog','information_schema') OR n.nspname LIKE 'pg\\_%')), '')
 || coalesce((SELECT string_agg(t.oid::text || ':' || t.xmin::text || ':' || t.tgenabled::text, ',' ORDER BY t.oid) FROM pg_catalog.pg_trigger t), '')
 || coalesce((SELECT string_agg(i.inhrelid::text || ':' || i.inhparent::text, ',' ORDER BY i.inhrelid, i.inhparent) FROM pg_catalog.pg_inherits i), ''))"""
LOCKS = """SELECT count(*) FROM pg_catalog.pg_locks l WHERE l.pid = pg_catalog.pg_backend_pid() AND l.locktype = 'relation'
            AND l.mode IN ('ShareUpdateExclusiveLock','ShareLock','ShareRowExclusiveLock','ExclusiveLock','AccessExclusiveLock')"""
def bench(c, sql, n=1000):
    t = time.perf_counter()
    for _ in range(n): c.params(sql, [])
    return (time.perf_counter() - t) / n * 1e6
base = bench(rt, "SELECT 1")
made = 0
for target in (10, 100, 500):
    while made < target:
        made += 1
        r = mig.one(f"CREATE TABLE t{made} (id int PRIMARY KEY, a text, b text, c int, d int, e timestamptz, f jsonb, g boolean, h numeric, i text)")
        assert r.ok, r
        for op in ("insert", "update", "delete", "truncate"):
            ref = {"insert": "REFERENCING NEW TABLE AS n", "update": "REFERENCING OLD TABLE AS o NEW TABLE AS n", "delete": "REFERENCING OLD TABLE AS o", "truncate": ""}[op]
            r = vm.one(f"CREATE TRIGGER vm_capture_{op} AFTER {op.upper()} ON public.t{made} {ref} FOR EACH STATEMENT EXECUTE FUNCTION vm.capture()")
            assert r.ok, r
    vm.one("ANALYZE")
    d = bench(rt, DIGEST, 500); l = bench(rt, LOCKS, 500)
    print(f"tables={target:4}  round trip alone {base:6.0f} us   digest {d - base:7.0f} us more   own strong locks from pg_locks {l - base:6.0f} us more")
