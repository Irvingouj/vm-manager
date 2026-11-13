import sys
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from pq import Conn
port = int(sys.argv[1])
su = Conn(port, "postgres", "a1", password="postgres")
Q = "SELECT oid, xmin, ctid, tgenabled FROM pg_trigger WHERE tgname = 'vm_capture_update'"
A = "SELECT attnum, xmin, ctid FROM pg_attribute WHERE attrelid = 'public.victim'::regclass AND attnum > 0 ORDER BY 1"
C = "SELECT xmin, ctid, relfilenode FROM pg_class WHERE oid = 'public.victim'::regclass"
def state(label):
    print(label, su.run(Q)[0].rows, su.run(A)[0].rows, su.run(C)[0].rows)
state("before            ")
for sql in ["VACUUM (FREEZE, ANALYZE)", "VACUUM FULL public.victim", "REINDEX TABLE public.victim", "CLUSTER public.victim USING victim_pkey",
            "VACUUM FULL pg_catalog.pg_trigger", "VACUUM FULL pg_catalog.pg_attribute", "VACUUM (FREEZE) pg_catalog.pg_trigger", "ANALYZE"]:
    r = su.one(sql); state(f"{sql[:34]:34} {'ok' if r.ok else r}")
