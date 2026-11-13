"""Facts the fix would rest on. fix.py PORT (a cluster prepared by spike-setup)."""
import sys, time, threading
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from pq import Conn, ConnectError
port = int(sys.argv[1])
def show(label, r): print(f"{label}: {r}")
vm = Conn(port, "a1_vm", "a1")
DIGEST = "SELECT md5(string_agg(t.oid::text || ':' || t.xmin::text || ':' || t.tgenabled::text, ',' ORDER BY t.oid)) FROM pg_catalog.pg_trigger t WHERE t.tgname LIKE 'vm\\_capture\\_%'"
base = vm.val(DIGEST)
print("baseline", base)

print("== F1: read-only transaction and the security definer function")
rt = Conn(port, "a1_rt", "a1")
show("begin ro", rt.one("BEGIN READ ONLY"))
show("v1 in read-only", rt.one("SELECT v1b()"))
show("rollback", rt.one("ROLLBACK"))
print("witness", vm.run("SELECT via, ran_as, note FROM public.witness ORDER BY at")[0].rows)

print("== F2: in a read-write transaction the digest differs before commit; rollback undoes all")
show("begin", rt.one("BEGIN"))
show("v1", rt.one("SELECT v1b()"))
show("digest inside equals baseline", rt.val(DIGEST) == base)
show("victim inside", rt.val("SELECT v FROM victim WHERE id = 1"))
show("rollback", rt.one("ROLLBACK"))
show("digest after rollback equals baseline", vm.val(DIGEST) == base)
show("victim after rollback", vm.val("SELECT v FROM public.victim WHERE id = 1"))

print("== F3: a WITH HOLD cursor runs its query at COMMIT, after any check")
show("begin", rt.one("BEGIN"))
show("declare", rt.one("DECLARE c CURSOR WITH HOLD FOR SELECT cascade_now()"))
show("digest before commit equals baseline", rt.val(DIGEST) == base)
show("commit", rt.one("COMMIT"))
show("digest after commit equals baseline", vm.val(DIGEST) == base)
show("victim", vm.val("SELECT v FROM public.victim WHERE id = 1"))
rt.one("CLOSE ALL")
base = vm.val(DIGEST)
print("  the same, with CLOSE ALL before the check")
show("begin", rt.one("BEGIN"))
show("declare", rt.one("DECLARE c CURSOR WITH HOLD FOR SELECT cascade_now()"))
show("close all", rt.one("CLOSE ALL"))
show("digest before commit equals baseline", rt.val(DIGEST) == base)
show("commit", rt.one("COMMIT"))
show("digest after commit equals baseline", vm.val(DIGEST) == base)

print("== F4: the migration role's own password, set to NULL by the host as the last act")
plat = Conn(port, "plat", "postgres")
mig = Conn(port, "a1_mig", "a1")
show("begin", mig.one("BEGIN"))
show("migration sets a password", mig.one("ALTER ROLE CURRENT_USER PASSWORD 'known'"))
show("migration tries more", mig.one("SAVEPOINT s; ALTER ROLE CURRENT_USER VALID UNTIL 'infinity'"))
mig.one("ROLLBACK TO s")
show("migration tries connection limit", mig.one("ALTER ROLE CURRENT_USER CONNECTION LIMIT 5"))
mig.one("ROLLBACK TO s")
show("host: password null", mig.one("ALTER ROLE CURRENT_USER PASSWORD NULL"))
show("commit", mig.one("COMMIT"))
for pw in ("known", "a1_mig"):
    try:
        Conn(port, "a1_mig", "a1", password=pw); print(f"login with {pw!r}: SUCCEEDED")
    except ConnectError as e:
        print(f"login with {pw!r}: refused: {e}")
show("session still alive", mig.one("SELECT current_user"))

print("== F5: stretching the window between commit and NOLOGIN (the test's technique)")
show("platform restores a password", plat.one("ALTER ROLE a1_mig LOGIN PASSWORD 'fresh'"))
mig = Conn(port, "a1_mig", "a1", password="fresh")
su = Conn(port, "postgres", "postgres", password="postgres")
show("mig begin", mig.one("BEGIN"))
show("mig sets a password", mig.one("ALTER ROLE CURRENT_USER PASSWORD 'known2'"))
got = {}
def locker():
    su.one("BEGIN")
    got["lock"] = su.one("SELECT 1 FROM pg_catalog.pg_authid WHERE rolname = 'a1_mig' FOR UPDATE")
t = threading.Thread(target=locker); t.start(); time.sleep(0.5)
show("mig commits", mig.one("COMMIT")); t.join()
show("superuser holds the row", got["lock"])
def closer():
    got["close"] = plat.one("SET statement_timeout = '3s'; ALTER ROLE a1_mig NOLOGIN PASSWORD NULL")
t = threading.Thread(target=closer); t.start(); time.sleep(0.5)
try:
    Conn(port, "a1_mig", "a1", password="known2"); print("login with the migration's password while the close waits: SUCCEEDED")
except ConnectError as e:
    print(f"login with the migration's password while the close waits: refused: {e}")
t.join(); show("the close", got["close"]); su.one("ROLLBACK")
plat.one("RESET statement_timeout; ALTER ROLE a1_mig NOLOGIN PASSWORD NULL")

print("== F6: what an index, a statistic depends on, from the catalogs")
for r in vm.run("""
SELECT c.relname, d.refclassid::regclass, CASE WHEN d.refclassid = 'pg_proc'::regclass THEN d.refobjid::regprocedure::text ELSE d.refobjid::text END,
       (SELECT l.lanname FROM pg_proc p JOIN pg_language l ON l.oid = p.prolang WHERE d.refclassid = 'pg_proc'::regclass AND p.oid = d.refobjid)
  FROM pg_depend d JOIN pg_class c ON c.oid = d.objid AND d.classid = 'pg_class'::regclass
 WHERE c.relkind = 'i' AND c.relname IN ('idx_e', 'idx_p', 'idx_op', 'idx_dom') AND d.refclassid <> 'pg_class'::regclass ORDER BY 1, 2, 3""")[0].rows: print("  ", r)
for r in vm.run("""
SELECT s.stxname, d.refclassid::regclass, d.refobjid::regprocedure::text FROM pg_depend d JOIN pg_statistic_ext s ON s.oid = d.objid AND d.classid = 'pg_statistic_ext'::regclass
 WHERE d.refclassid = 'pg_proc'::regclass""")[0].rows: print("  ", r)
