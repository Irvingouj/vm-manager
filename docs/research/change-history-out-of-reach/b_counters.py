"""Part B: are the transaction's own counters (pg_stat_xact_user_tables) fit
to prove that every row a migration changed was captured? For each case,
in a transaction on a1_mig's session with a migration identity attached:
the counters per table against the outbox rows this transaction wrote.
Usage: b_counters.py PORT
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scenario import Runner, PGV  # noqa: E402

SETUP = """
CREATE TABLE public.c (id integer PRIMARY KEY, v integer, w text);
CREATE INDEX ON public.c (v);
INSERT INTO public.c SELECT i, i, 'x' FROM generate_series(1, 10) i;
CREATE TABLE public.cp (id integer PRIMARY KEY);
CREATE TABLE public.cc (id integer PRIMARY KEY, p integer REFERENCES public.cp ON DELETE CASCADE);
INSERT INTO public.cp VALUES (1), (2);
INSERT INTO public.cc VALUES (1, 1), (2, 1), (3, 2);
CREATE TABLE public.pt (id integer, k integer) PARTITION BY RANGE (k);
CREATE TABLE public.pt1 PARTITION OF public.pt FOR VALUES FROM (0) TO (10);
CREATE TABLE public.pt2 PARTITION OF public.pt FOR VALUES FROM (10) TO (20);
INSERT INTO public.pt VALUES (1, 1), (2, 2), (3, 15);
CREATE TABLE public.ip (id integer, v integer);
INSERT INTO public.ip VALUES (1, 1);
SELECT vm.capture_new_tables();
"""

CASES = [
    ("plain insert, update 2, delete", [
        "INSERT INTO public.c VALUES (11, 11, 'x')",
        "UPDATE public.c SET v = v + 100 WHERE id IN (1, 2)",
        "DELETE FROM public.c WHERE id = 3"]),
    ("insert in a savepoint rolled back", [
        "SAVEPOINT s", "INSERT INTO public.c VALUES (11, 11, 'x')", "ROLLBACK TO SAVEPOINT s"]),
    ("insert in an EXCEPTION block that catches", [
        "DO $$ BEGIN INSERT INTO public.c VALUES (11, 11, 'x'); RAISE EXCEPTION 'x'; "
        "EXCEPTION WHEN others THEN NULL; END $$"]),
    ("ON CONFLICT DO UPDATE on an existing key", [
        "INSERT INTO public.c VALUES (1, 1, 'y') ON CONFLICT (id) DO UPDATE SET w = 'y'"]),
    ("ON CONFLICT DO NOTHING on an existing key", [
        "INSERT INTO public.c VALUES (1, 1, 'y') ON CONFLICT DO NOTHING"]),
    ("delete with a cascade", ["DELETE FROM public.cp WHERE id = 1"]),
    ("COPY of 3 rows", ["@copy COPY public.c FROM STDIN|21\t21\tx\n22\t22\tx\n23\t23\tx\n"]),
    ("HOT update of an unindexed column", ["UPDATE public.c SET w = 'hot' WHERE id = 5"]),
    ("MERGE: update 1, insert 1, delete 1", [
        "MERGE INTO public.c t USING (VALUES (1, 'm'), (2, 'del'), (30, 'new')) s(id, w) ON t.id = s.id "
        "WHEN MATCHED AND s.w = 'del' THEN DELETE WHEN MATCHED THEN UPDATE SET w = s.w "
        "WHEN NOT MATCHED THEN INSERT VALUES (s.id, 0, s.w)"]),
    ("update through the partitioned parent, within a partition", [
        "UPDATE public.pt SET id = id + 10 WHERE k = 1"]),
    ("update moving a row to another partition", ["UPDATE public.pt SET k = 16 WHERE k = 2"]),
    ("TRUNCATE (its refusing trigger dropped first)", [
        "DROP TRIGGER vm_capture_truncate ON public.c", "TRUNCATE public.c CASCADE"]),
    ("ALTER COLUMN TYPE ... USING (a rewrite)", [
        "ALTER TABLE public.c ALTER COLUMN v TYPE bigint USING 0"]),
    ("capture disabled, update 3 rows, enabled", [
        "ALTER TABLE public.c DISABLE TRIGGER vm_capture_update",
        "UPDATE public.c SET v = -1 WHERE id <= 3",
        "ALTER TABLE public.c ENABLE TRIGGER vm_capture_update"]),
    ("inheritance: a new child balances an uncaptured update of its parent", [
        "CREATE TABLE public.ic () INHERITS (public.ip)",
        "INSERT INTO public.ic VALUES (2, 2)",
        "UPDATE public.ip SET v = v + 1 WHERE id = 2",
        "ALTER TABLE public.ip DISABLE TRIGGER vm_capture_update",
        "UPDATE ONLY public.ip SET v = 999 WHERE id = 1",
        "ALTER TABLE public.ip ENABLE TRIGGER vm_capture_update"]),
    ("counters after pg_stat_clear_snapshot() and pg_stat_force_next_flush()", [
        "UPDATE public.c SET v = v + 1 WHERE id = 4",
        "SELECT pg_stat_clear_snapshot()", "SELECT pg_stat_force_next_flush()"]),
]

KIND = {"insert": 0, "update": 1, "delete": 2}


def read_counters(c):
    """The view's numbers. They are not this transaction's alone: they hold
    whatever the backend has not flushed yet (earlier transactions too), so
    a case uses the difference from a reading taken at its start."""
    return {row[0]: tuple(int(x) for x in row[1:]) for row in c.one(
        "SELECT schemaname || '.' || relname, n_tup_ins, n_tup_upd, n_tup_del, n_tup_hot_upd "
        "FROM pg_stat_xact_user_tables").rows}


def main():
    r = Runner(int(sys.argv[1]))
    r.directive(["conn", "mig", "a1_mig", "a1"], "")
    c = r.conns["mig"]
    c.run("BEGIN")
    r.attach("migration", "0001_counters_setup")
    res = c.one(SETUP)
    assert res.ok, res
    c.run("COMMIT")
    existing = ["public.c", "public.cc", "public.cp", "public.ip", "public.pt", "public.pt1", "public.pt2"]
    c.run("BEGIN")
    print(f"[pg{PGV}] counters at the start of a new transaction, before it did anything (left from earlier ones): "
          + ", ".join(f"{t} {v[0]}/{v[1]}/{v[2]}" for t, v in sorted(read_counters(c).items()) if any(v)))
    c.run("ROLLBACK")
    print(f"[pg{PGV}] counters: change of n_tup_ins/upd/del (hot) in pg_stat_xact_user_tables during the case vs outbox insert/update/delete of the case")
    for title, statements in CASES:
        c.run("BEGIN")
        r.attach("migration", "0002_case")
        base = read_counters(c)
        err = None
        for s in statements:
            if s.startswith("@copy "):
                sql, data = s[6:].split("|", 1)
                res = c.copy_in(sql, data)
            else:
                res = c.one(s)
            if not res.ok:
                err = f"{s[:40]}: {res}"
                break
        after = read_counters(c)
        counters = {}
        for t, v in after.items():
            d = tuple(a - b for a, b in zip(v, base.get(t, (0, 0, 0, 0))))
            if any(d) and t != "vm.outbox":
                counters[t] = d
        outbox = {}
        for t, k, n in c.one("SELECT * FROM vm.outbox_this_transaction()").rows:
            outbox.setdefault(t, [0, 0, 0])[KIND[k]] = int(n)
        verdicts = []
        for t in sorted(set(counters) | set(outbox)):
            cn = counters.get(t, (0, 0, 0, 0))
            ob = tuple(outbox.get(t, (0, 0, 0)))
            mark = "=" if cn[:3] == ob else "MISMATCH"
            if t not in existing:
                mark += "(new table)"
            verdicts.append(f"{t} {cn[0]}/{cn[1]}/{cn[2]}({cn[3]}) vs {ob[0]}/{ob[1]}/{ob[2]} {mark}")
        flat = "; ".join(verdicts) or "nothing counted, nothing captured"
        print(f"[pg{PGV}]   {title}: {flat}" + (f"  [error {err}]" if err else ""), flush=True)
        c.run("ROLLBACK")
    c.close()


if __name__ == "__main__":
    main()
