"""Part C: the outbox and the relay. Two databases (a1, the own database;
platdb, the platform database), the relay as the host would run it. Usage:
c_relay.py PORT  (the scenario); c_relay.py PORT --ship-and-hang (used by
the crash case: ships one batch, prints SHIPPED, then waits to be killed).
"""
import hashlib
import json
import os
import statistics
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pq import Conn, ConnectError  # noqa: E402

PGV = os.environ.get("PGV", "?")
HERE = os.path.dirname(os.path.abspath(__file__))
APP = "a1"
BATCH_ROWS = 1000
BATCH_BYTES = 8 << 20

FIELDS = ("id bigint, at timestamptz, schema_name text, table_name text, kind text, key jsonb, "
          "old jsonb, new jsonb, changed jsonb, user_id text, application text, actor text, "
          "request text, migration text")

# One batch, oldest first: at most `rows` rows and, past the first row, at
# most `bytes` bytes of jsonb text. Only committed rows are visible.
SELECT_BATCH = """
SELECT coalesce(jsonb_agg(to_jsonb(s) - 'running' ORDER BY s.id), '[]')::text, count(*)
  FROM (SELECT o.id, o.at, o.schema_name, o.table_name, o.kind, o.key, o.old, o.new, o.changed,
               o.user_id, o.application, o.actor, o.request, o.migration,
               sum(octet_length(coalesce(o.old::text, '')) + octet_length(coalesce(o.new::text, '')))
                   OVER (ORDER BY o.id) AS running,
               row_number() OVER (ORDER BY o.id) AS n
          FROM (SELECT * FROM vm.outbox ORDER BY id LIMIT $1) o) s
 WHERE s.n = 1 OR s.running <= $2
"""
INSERT_HISTORY = f"""
INSERT INTO history (application, change_id, at, schema_name, table_name, kind, key, old, new,
                     changed, user_id, app, actor, request, migration)
SELECT $1, r.id, r.at, r.schema_name, r.table_name, r.kind, r.key, r.old, r.new, r.changed,
       r.user_id, r.application, r.actor, r.request, r.migration
  FROM jsonb_to_recordset($2::jsonb) AS r({FIELDS})
    ON CONFLICT (application, change_id) DO NOTHING
"""
DELETE_SHIPPED = "DELETE FROM vm.outbox WHERE id = ANY (SELECT (e ->> 'id')::bigint FROM jsonb_array_elements($1::jsonb) e)"


PHASES = {"read": 0.0, "insert": 0.0, "delete": 0.0, "bytes": 0}


def ship_once(own, plat, rows=BATCH_ROWS, nbytes=BATCH_BYTES, hang_after_insert=False):
    """One batch: read, insert into the platform database and commit there,
    then delete from the outbox. Returns the number of rows shipped."""
    t0 = time.perf_counter()
    r = own.params(SELECT_BATCH, [rows, nbytes])
    assert r.ok, r
    payload, n = r.rows[0][0], int(r.rows[0][1])
    t1 = time.perf_counter()
    PHASES["read"] += t1 - t0
    if n == 0:
        return 0
    PHASES["bytes"] += len(payload)
    r = plat.params(INSERT_HISTORY, [APP, payload])
    assert r.ok, r
    t2 = time.perf_counter()
    PHASES["insert"] += t2 - t1
    if hang_after_insert:
        print("SHIPPED", flush=True)
        sys.stdin.read()
    r = own.params(DELETE_SHIPPED, [payload])
    assert r.ok, r
    PHASES["delete"] += time.perf_counter() - t2
    return n


def relay(own, plat, **kw):
    total = 0
    while True:
        n = ship_once(own, plat, **kw)
        if n == 0:
            return total
        total += n


class Identity:
    def __init__(self, port, db, role):
        k = Conn(port, role, db)
        row = k.one("SELECT encode(k1,'hex'), encode(k2,'hex') FROM vm.keys").rows[0]
        self.k1, self.k2 = bytes.fromhex(row[0]), bytes.fromhex(row[1])
        k.close()

    def attach(self, c, actor="user"):
        txkey = c.val("SELECT vm.transaction_key()")
        who = json.dumps({"user_id": "u@example", "application": APP, "actor": actor,
                          "request": "r1", "migration": None}, separators=(",", ":"))
        inner = hashlib.sha256(self.k1 + f"vm.ctx v1\n{txkey}\n{who}".encode()).digest()
        tag = hashlib.sha256(self.k2 + inner).hexdigest()
        r = c.params("SELECT pg_catalog.set_config('vm.ctx', $1, true)", [tag + who])
        assert r.ok, r


def say(text):
    print(f"[pg{PGV}] {text}", flush=True)


def ms(t):
    return f"{t * 1000:.2f} ms"


def timed(f):
    t = time.perf_counter()
    f()
    return time.perf_counter() - t


def main_hang(port):
    own, plat = Conn(port, "a1_vm", "a1"), Conn(port, "plat", "platdb")
    ship_once(own, plat, hang_after_insert=True)


def main(port):
    own = Conn(port, "a1_vm", "a1")
    with open(os.path.join(HERE, "hardening.sql")) as f:
        assert own.one(f.read()).ok
    plat = Conn(port, "plat", "platdb")
    ident = Identity(port, "a1", "a1_vm")
    rt = Conn(port, "a1_rt", "a1")
    rt2 = Conn(port, "a1_rt", "a1")
    count_out = lambda: int(own.val("SELECT count(*) FROM vm.outbox"))  # noqa: E731
    count_hist = lambda: int(plat.val("SELECT count(*) FROM history"))  # noqa: E731
    say(f"start: outbox {count_out()}, history {count_hist()}; relay: {relay(own, plat)} shipped; "
        f"outbox {count_out()}, history {count_hist()}")

    # C1: the change and its outbox row commit or roll back together.
    before = count_out()
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("INSERT INTO public.items VALUES (500, 'rolled back', 1)")
    seen_open = count_out()
    rt.run("ROLLBACK")
    after_rb = count_out()
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("INSERT INTO public.items VALUES (500, 'committed', 1)")
    rt.run("COMMIT")
    after_c = count_out()
    say(f"C1 outbox rows: before {before}; seen by another session while the write is open {seen_open}; "
        f"after ROLLBACK {after_rb}; after COMMIT {after_c}")
    relay(own, plat)

    # C2: a transaction still open when a later one commits.
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("INSERT INTO public.items VALUES (501, 'A, open', 1)")
    rt2.run("BEGIN"); ident.attach(rt2)
    rt2.one("INSERT INTO public.items VALUES (502, 'B, committed first', 1)")
    rt2.run("COMMIT")
    n1 = relay(own, plat)
    ids1 = plat.one("SELECT string_agg(change_id::text, ',' ORDER BY change_id) FROM history WHERE new ->> 'id' IN ('501','502')").rows[0][0]
    rt.run("COMMIT")
    n2 = relay(own, plat)
    ids2 = plat.one("SELECT string_agg(change_id::text || ':' || (new ->> 'id'), ',' ORDER BY change_id) FROM history WHERE new ->> 'id' IN ('501','502')").rows[0][0]
    say(f"C2 relay while A is open shipped {n1} "
        f"(history now has change ids {ids1}); after A commits shipped {n2}; history change_id:row = {ids2}")

    # C3: the same batch inserted twice.
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("UPDATE public.items SET title = 'twice' WHERE id = 500")
    rt.run("COMMIT")
    payload = own.params(SELECT_BATCH, [BATCH_ROWS, BATCH_BYTES]).rows[0][0]
    a = plat.params(INSERT_HISTORY, [APP, payload]).cmd
    b = plat.params(INSERT_HISTORY, [APP, payload]).cmd
    own.params(DELETE_SHIPPED, [payload])
    rows = plat.val("SELECT count(*) FROM history WHERE new ->> 'title' = 'twice'")
    say(f"C3 the same batch inserted twice: {a!r} then {b!r}; rows in history: {rows}")

    # C4: the relay killed between the insert into platdb and the delete.
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("UPDATE public.items SET title = 'crash' WHERE id IN (500, 501, 502)")
    rt.run("COMMIT")
    p = subprocess.Popen([sys.executable, "-I", "-B", os.path.abspath(__file__), str(port), "--ship-and-hang"],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    line = p.stdout.readline().strip()
    p.kill()
    p.wait()
    in_hist = plat.val("SELECT count(*) FROM history WHERE new ->> 'title' = 'crash'")
    in_out = own.val("SELECT count(*) FROM vm.outbox WHERE new ->> 'title' = 'crash'")
    again = relay(own, plat)
    in_hist2 = plat.val("SELECT count(*) FROM history WHERE new ->> 'title' = 'crash'")
    in_out2 = own.val("SELECT count(*) FROM vm.outbox WHERE new ->> 'title' = 'crash'")
    say(f"C4 relay said {line!r} and was killed (SIGKILL): history {in_hist}, outbox {in_out}; "
        f"relay again shipped {again}: history {in_hist2}, outbox {in_out2}")

    # C5: the platform database cannot be reached for a while.
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("UPDATE public.items SET title = 'while down' WHERE id = 500")
    rt.run("COMMIT")
    try:
        Conn(1, "plat", "platdb")
        down = "connected?!"
    except ConnectError as e:
        down = "connection refused" if "refused" in str(e) else str(e)[:60]
    kept = own.val("SELECT count(*) FROM vm.outbox")
    shipped = relay(own, plat)
    has = plat.params("SELECT count(*) FROM history WHERE new ->> 'title' = $1", ["while down"]).rows[0][0]
    say(f"C5 platform database unreachable: {down}; outbox keeps {kept}; reachable again: shipped {shipped}, "
        f"rows of it in history {has}")

    # C7: two relays at once (two requests of the host), interleaved.
    own2, plat2 = Conn(port, "a1_vm", "a1"), Conn(port, "plat", "platdb")
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("UPDATE public.items SET title = 'two relays' WHERE id IN (500, 501)")
    rt.run("COMMIT")
    pa = own.params(SELECT_BATCH, [BATCH_ROWS, BATCH_BYTES]).rows[0][0]
    pb = own2.params(SELECT_BATCH, [BATCH_ROWS, BATCH_BYTES]).rows[0][0]
    ia = plat.params(INSERT_HISTORY, [APP, pa]).cmd
    ib = plat2.params(INSERT_HISTORY, [APP, pb]).cmd
    da = own.params(DELETE_SHIPPED, [pa]).cmd
    db = own2.params(DELETE_SHIPPED, [pb]).cmd
    n = plat.params("SELECT count(*) FROM history WHERE new ->> 'title' = $1", ["two relays"]).rows[0][0]
    say(f"C7 two relays read the same batch, insert ({ia!r}, {ib!r}), delete ({da!r}, {db!r}): rows in history {n}, "
        f"outbox {own.val('SELECT count(*) FROM vm.outbox')}")
    own2.close()
    plat2.close()

    # C6: the history is out of every own-database role's reach.
    for role in ("a1_mig", "a1_rt", "a1_vm"):
        try:
            Conn(port, role, "platdb").close()
            say(f"C6 {role} connects to platdb: YES")
        except ConnectError as e:
            say(f"C6 {role} connects to platdb: refused ({str(e).split('FATAL:')[-1].strip().splitlines()[0]})")

    measure(port, own, plat, ident, rt)
    for c in (own, plat, rt, rt2):
        c.close()


def setup_today(port):
    """Database a0 with today's schema vm (crates/vm/src/own_db_schema.sql)
    for comparison, owned by one role as today."""
    pl = Conn(port, "plat", "postgres")
    pl.one("CREATE ROLE a0_owner LOGIN PASSWORD 'a0_owner'")
    pl.one("GRANT a0_owner TO plat WITH INHERIT FALSE, SET TRUE")
    r = pl.one("CREATE DATABASE a0 OWNER a0_owner TEMPLATE template0")
    assert r.ok, r
    pl.close()
    o = Conn(port, "a0_owner", "a0")
    schema = os.path.join(HERE, "..", "..", "..", "crates", "vm", "src", "own_db_schema.sql")
    with open(schema) as f:
        r = o.one(f.read())
    assert r.ok, r
    return o


BENCH = """
CREATE TABLE public.bench (id integer PRIMARY KEY, v integer, label text);
INSERT INTO public.bench SELECT i, i, 'row ' || i FROM generate_series(1, 100000) i;
"""


def measure(port, own, plat, ident, rt):
    today = setup_today(port)
    today_ident = Identity(port, "a0", "a0_owner")
    mig = Conn(port, "a1_mig", "a1")
    mig.run("BEGIN"); ident.attach(mig, "migration")
    assert mig.one(BENCH + "CREATE TABLE public.plain (LIKE public.bench INCLUDING ALL); "
                   "INSERT INTO public.plain SELECT * FROM public.bench; SELECT vm.capture_new_tables();").ok
    mig.one("DROP TRIGGER vm_capture_insert ON public.plain; DROP TRIGGER vm_capture_update ON public.plain; "
            "DROP TRIGGER vm_capture_delete ON public.plain; DROP TRIGGER vm_capture_truncate ON public.plain")
    mig.run("COMMIT")
    assert today.one(BENCH + "SELECT vm.capture_new_tables();").ok
    hardened = own.val("SELECT count(*) FROM pg_proc WHERE proname = 'capture' AND proconfig IS NOT NULL") == "1"

    def run_rb(c, idt, sql, actor="migration"):
        c.run("BEGIN"); idt.attach(c, actor)
        t = timed(lambda: c.one(sql))
        c.run("ROLLBACK")
        return t

    def med(c, idt, sql):
        return statistics.median(run_rb(c, idt, sql) for _ in range(3))

    for what, sql in (("UPDATE of 100,000 rows", "UPDATE public.{t} SET v = v + 1"),
                      ("INSERT of 100,000 rows", "INSERT INTO public.{t} SELECT i + 200000, i, 'new ' || i FROM generate_series(1, 100000) i")):
        none = med(mig, ident, sql.format(t="plain"))
        old = med(today, today_ident, sql.format(t="bench"))
        new = med(mig, ident, sql.format(t="bench"))
        per = lambda t: f"{(t - none) / 100000 * 1e6:.2f} us/row"  # noqa: E731
        say(f"M1 {what}, median of 3: no capture {ms(none)}; today's vm.history capture {ms(old)} ({per(old)} over none); "
            f"outbox capture{' (hardened)' if hardened else ''} {ms(new)} ({per(new)} over none)")

    # M2: shipping throughput, 100,000 changes from one statement.
    for rows, nbytes in ((1000, 8 << 20), (10000, 8 << 20), (10000, 256 << 10)):
        mig.run("BEGIN"); ident.attach(mig, "migration")
        mig.one("UPDATE public.bench SET v = v + 1")
        mig.run("COMMIT")
        n_out = own.val("SELECT count(*) FROM vm.outbox")
        size = own.val("SELECT pg_size_pretty(sum(octet_length(old::text) + octet_length(new::text))) FROM vm.outbox")
        batches = [0]
        for k in PHASES:
            PHASES[k] = 0

        def go():
            while True:
                n = ship_once(own, plat, rows=rows, nbytes=nbytes)
                if n == 0:
                    return
                batches[0] += 1
        t = timed(go)
        say(f"M2 shipping {n_out} changes ({size} of old+new jsonb text; {PHASES['bytes'] >> 10} KiB of batches sent), "
            f"batches of at most {rows} rows / {nbytes >> 10} KiB: {batches[0]} batches, {ms(t)}, {int(n_out) / t:,.0f} rows/s "
            f"(read {ms(PHASES['read'])}, insert+commit {ms(PHASES['insert'])}, delete+commit {ms(PHASES['delete'])})")

    # M3: what a single-row write's answer waits for.
    write, ship = [], []
    for i in range(200):
        def w():
            rt.run("BEGIN"); ident.attach(rt)
            rt.one(f"UPDATE public.items SET amount = {i} WHERE id = 1")
            rt.run("COMMIT")
        write.append(timed(w))
        ship.append(timed(lambda: relay(own, plat)))
    q = lambda xs: f"median {ms(statistics.median(xs))}, p95 {ms(sorted(xs)[int(len(xs) * 0.95)])}"  # noqa: E731
    say(f"M3 single-row write (BEGIN, attach, UPDATE, COMMIT): {q(write)}; then shipping it "
        f"(read, insert+commit in platdb, delete+commit in a1): {q(ship)}; 200 runs, fsync on, "
        f"data directory {'on tmpfs' if os.environ.get('CHR_TMPFS') else 'on disk'}")
    today.close()
    mig.close()


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[2] == "--ship-and-hang":
        main_hang(int(sys.argv[1]))
    else:
        main(int(sys.argv[1]))
