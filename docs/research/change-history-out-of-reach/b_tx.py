"""Part B: which migration texts end the platform's transaction. The host
today sends each breakpoint-separated part of a migration with
batch_execute, i.e. the simple-query protocol, inside a transaction it
began (crates/vm/src/own_db.rs). Here: on a1_mig's session, BEGIN, a
captured INSERT the platform considers part of the migration, then the
text; then the session's transaction status and whether that INSERT is
already visible to another session (committed). Usage: b_tx.py PORT
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scenario import Runner, PGV  # noqa: E402
from pq import Conn  # noqa: E402

CASES = [
    ("simple", "COMMIT; BEGIN"),
    ("simple", "COMMIT AND CHAIN"),
    ("simple", "END; START TRANSACTION"),
    ("simple", "ROLLBACK AND CHAIN"),
    ("simple", "ABORT; BEGIN"),
    ("simple", "PREPARE TRANSACTION 'x'; BEGIN"),
    ("simple", "SAVEPOINT a; RELEASE SAVEPOINT a"),
    ("simple", "DO $$ BEGIN COMMIT; END $$"),
    ("simple", "CREATE PROCEDURE public.p() LANGUAGE plpgsql AS $$ BEGIN COMMIT; END $$; CALL public.p()"),
    ("simple", "SELECT query_to_xml('ALTER TABLE public.items DISABLE TRIGGER USER', true, true, '')"),
    ("simple", "SELECT query_to_xml('COMMIT', true, true, '')"),
    ("extended", "COMMIT"),
    ("extended", "COMMIT AND CHAIN"),
    ("extended", "SELECT 1; COMMIT"),
]


def main():
    port = int(sys.argv[1])
    r = Runner(port)
    r.directive(["conn", "mig", "a1_mig", "a1"], "")
    mig = r.conns["mig"]
    other = Conn(port, "a1_vm", "a1")
    print(f"[pg{PGV}] protocol | text | answer | status after | the earlier INSERT committed?")
    for n, (proto, text) in enumerate(CASES):
        key = 1000 + n
        mig.run("BEGIN")
        r.attach("migration", "0001_tx")
        assert mig.one(f"INSERT INTO public.items VALUES ({key}, 'tx test', 0)").ok
        res = mig.one(text) if proto == "simple" else mig.params(text, [])
        status = mig.status()
        seen = other.val(f"SELECT count(*) FROM public.items WHERE id = {key}")
        ans = "ok" if res.ok else f"{res.sqlstate} {res.message}"
        print(f"[pg{PGV}]   {proto} | {text} | {ans} | {status} | {'YES' if seen == '1' else 'no'}", flush=True)
        if mig.status() != "idle":
            mig.run("ROLLBACK")
        mig.run("ROLLBACK PREPARED 'x'")
    other.close()
    mig.close()


if __name__ == "__main__":
    main()
