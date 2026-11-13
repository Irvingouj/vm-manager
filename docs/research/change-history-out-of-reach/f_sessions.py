"""Part F, `sessions`: what the look at the session's own strong locks (as
vm.before_commit() makes it before every commit of a write) costs as the
instance gets busy. pg_locks copies the lock manager's whole table, so the look
grows with the locks all sessions hold and with max_connections, and it takes
the lock manager's partitions while it copies.

    f_sessions.py PORT scaling      one session's look while 0 to 400 others hold 20 locks each (needs max_connections >= 500)
    f_sessions.py PORT throughput   small write transactions from 1 to 64 client processes, with and without the look
"""
import subprocess
import sys
import time
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from pq import Conn
LOCKS = ("SELECT count(*) FROM pg_catalog.pg_locks l WHERE l.pid = pg_catalog.pg_backend_pid() AND l.locktype = 'relation' "
         "AND l.mode IN ('ShareUpdateExclusiveLock','ShareLock','ShareRowExclusiveLock','ExclusiveLock','AccessExclusiveLock')")
SECONDS = 4

def bench(c, sql, n=2000):
    for _ in range(100):
        c.params(sql, [])
    t = time.perf_counter()
    for _ in range(n):
        c.params(sql, [])
    return (time.perf_counter() - t) / n * 1e6

def scaling(port):
    mig = Conn(port, "a1_mig", "a1")
    for i in range(20):
        assert mig.one(f"CREATE TABLE s{i} (id int)").ok
    held = []
    for target in (0, 50, 200, 400):
        while len(held) < target:
            c = Conn(port, "a1_mig", "a1")
            c.run("BEGIN; " + "; ".join(f"SELECT count(*) FROM s{i}" for i in range(20)))  # 20 ACCESS SHARE locks, held
            held.append(c)
        rows = mig.one("SELECT count(*) FROM pg_locks").rows[0][0]
        base = bench(mig, "SELECT 1")
        look = bench(mig, LOCKS)
        print(f"other sessions in a transaction: {target:4}  rows in pg_locks: {rows:>6}  round trip alone {base:5.0f} us  own strong locks {look - base:6.0f} us more")

def throughput(port):
    mig = Conn(port, "a1_mig", "a1")
    for i in range(16):
        assert mig.one(f"CREATE UNLOGGED TABLE w{i} (id int)").ok
    worker = __import__("os").path.join(__import__("os").path.dirname(__import__("os").path.abspath(__file__)), "f_sessions_worker.py")
    def run(clients, checked):
        ps = [subprocess.Popen([sys.executable, "-I", "-B", worker, str(port), str(i), "1" if checked else "0", str(SECONDS)],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) for i in range(clients)]
        time.sleep(1.5)
        for p in ps:
            p.stdin.write("go\n")
            p.stdin.flush()
        return sum(int(p.communicate()[0]) for p in ps) / SECONDS
    for clients in (1, 8, 32, 64):
        a = run(clients, False); b = run(clients, True); a2 = run(clients, False); b2 = run(clients, True)
        print(f"client processes {clients:3}: without the look {a:8.0f} and {a2:8.0f} tx/s; with it {b:8.0f} and {b2:8.0f} tx/s; ratio {(b + b2) / (a + a2):.2f}")

if __name__ == "__main__":
    {"scaling": scaling, "throughput": throughput}[sys.argv[2]](int(sys.argv[1]))
