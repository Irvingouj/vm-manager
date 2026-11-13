"""Part D: reading. Rows of jsonb kept in the platform database, shaped into
the table's present row type in the own database with jsonb_populate_record
over a jsonb[] parameter, as the runtime role, in a read-only transaction.
Also: matching a row's key; and a fault in the capture's to_jsonb(t).
Usage: d_read.py PORT
"""
import os
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pq import Conn  # noqa: E402
from c_relay import Identity, relay, setup_today, HERE  # noqa: E402

PGV = os.environ.get("PGV", "?")

TABLE = """
CREATE TYPE public.mood AS ENUM ('ok', 'sad');
CREATE TABLE public.vals (
    id integer PRIMARY KEY, i integer, s smallint, se serial, b bigint, n numeric, r real,
    d double precision, bo boolean, tx text, vc varchar(20), u uuid, ts timestamp,
    tz timestamptz, dt date, js json, jb jsonb, ta text[], ia integer[], by bytea,
    iv interval, mo public.mood);
SELECT vm.capture_new_tables();
"""
ROWS = [
    """INSERT INTO public.vals VALUES (1, 2147483647, -32768, DEFAULT, 9007199254740993,
        12345678901234567890.123456789000, 0.1, 0.1, true, E'quote \\' "dq" \\\\ ünï 🙂', 'v',
        'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '2026-11-12 10:11:12.345678',
        '2026-11-12 10:11:12.345678+00', '2026-11-12', '{"b": 1,  "a": [1, "x"], "a": 2}', '"123"',
        ARRAY['a,b', '"q"', E'back\\\\slash', '{brace}', '', NULL], '{{1,2},{3,4}}', '\\x00ff',
        '1 year 2 mons 3 days 04:05:06.789', 'sad')""",
    """INSERT INTO public.vals VALUES (2, NULL, 0, DEFAULT, -9223372036854775808, 'NaN', 'NaN',
        '-Infinity', false, '', '', NULL, 'infinity', '-infinity', '0001-01-01 BC', '"123"', 'null',
        '{}', '{NULL}', '', '-1 days -00:00:01', 'ok')""",
    "INSERT INTO public.vals (id) VALUES (3)",
    """INSERT INTO public.vals VALUES (4, 0, 0, DEFAULT, 0, -0.0, -0.0, 1e-300, NULL, 'x', NULL, NULL,
        '1999-12-31 23:59:59.999999', '1999-12-31 23:59:59.999999-12:00', '2000-02-29', '[]', '{"k": [null, 1.50]}',
        ARRAY['NULL', 'null'], '{-1,0}', '\\x', '0', NULL)""",
]
SHAPE = """SELECT r.* FROM unnest($1::jsonb[]) WITH ORDINALITY AS u(j, n)
CROSS JOIN LATERAL jsonb_populate_record(NULL::public.vals, u.j) AS r ORDER BY u.n"""


def say(text):
    print(f"[pg{PGV}] {text}", flush=True)


def as_migration(mig, ident, sql):
    mig.run("BEGIN")
    ident.attach(mig, "migration")
    r = mig.one(sql)
    assert r.ok, r
    mig.run("COMMIT")


def shaped(rt, payload):
    rt.run("BEGIN READ ONLY")
    rt.run("SET LOCAL TimeZone = 'Asia/Tokyo'")
    r = rt.params(SHAPE, [payload])
    rt.run("COMMIT")
    return r


def main():
    port = int(sys.argv[1])
    own = Conn(port, "a1_vm", "a1")
    with open(os.path.join(HERE, "hardening.sql")) as f:
        assert own.one(f.read()).ok
    plat = Conn(port, "plat", "platdb")
    ident = Identity(port, "a1", "a1_vm")
    mig = Conn(port, "a1_mig", "a1")
    rt = Conn(port, "a1_rt", "a1")
    as_migration(mig, ident, TABLE)
    rt.run("SET TimeZone = 'America/New_York'")
    rt.run("SET extra_float_digits = -15")  # the writer's own settings; the capture pins its own
    for sql in ROWS:
        rt.run("BEGIN"); ident.attach(rt)
        r = rt.one(sql)
        assert r.ok, r
        rt.run("COMMIT")
    rt.run("RESET extra_float_digits")
    relay(own, plat)

    # D1: every value round-trips.
    payload = plat.val("SELECT array_agg(new ORDER BY change_id)::text FROM history WHERE table_name = 'vals' AND kind = 'insert'")
    got = shaped(rt, payload)
    rt.run("BEGIN READ ONLY"); rt.run("SET LOCAL TimeZone = 'Asia/Tokyo'")
    want = rt.one("SELECT * FROM public.vals ORDER BY id")
    cols = [x[0] for x in rt.one("SELECT attname FROM pg_attribute WHERE attrelid = 'public.vals'::regclass AND attnum > 0 AND NOT attisdropped ORDER BY attnum").rows]
    rt.run("COMMIT")
    assert got.ok, got
    diffs = []
    for gr, wr in zip(got.rows, want.rows):
        for c, g, w in zip(cols, gr, wr):
            if g != w:
                diffs.append(f"row {wr[0]} {c}: select() {w!r} / history {g!r}")
    say(f"D1 {len(want.rows)} rows x {len(cols)} columns shaped from history as a1_rt, read-only; values that differ "
        f"from select(): {len(diffs)}" + ("".join(f"\n[pg{PGV}]     {d}" for d in diffs)))

    # D2: the present shape after later migrations.
    as_migration(mig, ident, "ALTER TABLE public.vals ADD COLUMN added text; "
                             "ALTER TABLE public.vals RENAME COLUMN tx TO tx2; ALTER TABLE public.vals DROP COLUMN vc;")
    r = shaped(rt, payload.replace("public.vals", "public.vals"))
    cols2 = [x[0] for x in rt.one("SELECT attname FROM pg_attribute WHERE attrelid = 'public.vals'::regclass AND attnum > 0 AND NOT attisdropped ORDER BY attnum").rows]
    row1 = dict(zip(cols2, r.rows[0])) if r.ok else {}
    say(f"D2 after ADD added, RENAME tx TO tx2, DROP vc: ok={r.ok}; columns {len(cols2)} ('vc' present: {'vc' in cols2}); "
        f"row 1 added={row1.get('added')!r} tx2={row1.get('tx2')!r}")
    as_migration(mig, ident, "ALTER TABLE public.vals ALTER COLUMN u TYPE integer USING 0")
    r = shaped(rt, payload)
    say(f"D2 after u uuid -> integer: a read holding an old uuid fails whole: {r}")
    without = plat.val("SELECT array_agg(new ORDER BY change_id)::text FROM history WHERE table_name = 'vals' AND kind = 'insert' AND new ->> 'u' IS NULL")
    r2 = shaped(rt, without)
    say(f"D2 the same read without those rows: ok={r2.ok}, {len(r2.rows)} rows")
    as_migration(mig, ident, "ALTER TABLE public.vals ALTER COLUMN u TYPE uuid USING NULL; "
                             "ALTER TABLE public.vals RENAME COLUMN tx2 TO tx; ALTER TABLE public.vals DROP COLUMN added; "
                             "ALTER TABLE public.vals ADD COLUMN vc varchar(20)")

    # D3: time for 1000 rows.
    rt.run("BEGIN"); ident.attach(rt)
    cl = "i, s, b, n, r, d, bo, tx, vc, u, ts, tz, dt, js, jb, ta, ia, by, iv, mo"
    r = rt.one(f"INSERT INTO public.vals (id, {cl}) SELECT g, {cl} FROM public.vals v, generate_series(100, 1099) g WHERE v.id = 1")
    assert r.ok, r
    rt.run("COMMIT")
    relay(own, plat)
    q = ("SELECT array_agg(new ORDER BY change_id DESC)::text FROM (SELECT * FROM history "
         "WHERE application = 'a1' AND schema_name = 'public' AND table_name = 'vals' ORDER BY change_id DESC LIMIT 1000) h")
    fetch_t, shape_t, size = [], [], 0
    for _ in range(20):
        t0 = time.perf_counter()
        p = plat.val(q)
        t1 = time.perf_counter()
        r = shaped(rt, p)
        t2 = time.perf_counter()
        assert r.ok and len(r.rows) == 1000, r
        fetch_t.append(t1 - t0)
        shape_t.append(t2 - t1)
        size = len(p)
    say(f"D3 1000 rows ({size >> 10} KiB of jsonb[] text): fetch from platdb median {statistics.median(fetch_t) * 1000:.1f} ms, "
        f"shape in a1 (BEGIN READ ONLY, SET LOCAL, query, COMMIT) median {statistics.median(shape_t) * 1000:.1f} ms; 20 runs")

    keys(port, own, plat, ident, mig, rt)
    capture_t_column(port, own, plat, ident, mig, rt)
    for c in (own, plat, mig, rt):
        c.close()


KEYS = [
    ("k_int", "integer", "7", '{"id": 7}'),
    ("k_big", "bigint", "9007199254740993", '{"id": "9007199254740993"}'),
    ("k_num", "numeric", "1.50", '{"id": "1.5"}'),
    ("k_tz", "timestamptz", "'2026-11-12 10:00:00+00'", '{"id": "2026-11-12T10:00:00.000Z"}'),
    ("k_uuid", "uuid", "'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'", '{"id": "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"}'),
]
CANON = """SELECT jsonb_object_agg(k, to_jsonb(r) -> k)
  FROM jsonb_populate_record(NULL::public.{t}, $1::jsonb) r, unnest($2::text[]) k"""


def keys(port, own, plat, ident, mig, rt):
    """D4: the SDK names a row by its key as JSON; the platform database
    knows no column types. Match as given, and after shaping the key in the
    own database and turning it back into jsonb there."""
    as_migration(mig, ident, "".join(f"CREATE TABLE public.{t} (id {ty} PRIMARY KEY, v text);" for t, ty, _, _ in KEYS)
                 + "SELECT vm.capture_new_tables();")
    rt.run("SET TimeZone = 'America/New_York'")
    for t, _, lit, _ in KEYS:
        rt.run("BEGIN"); ident.attach(rt)
        assert rt.one(f"INSERT INTO public.{t} VALUES ({lit}, 'x')").ok
        rt.run("COMMIT")
    relay(own, plat)
    for t, _, _, given in KEYS:
        stored = plat.params("SELECT key::text FROM history WHERE table_name = $1", [t]).rows[0][0]
        raw = plat.params("SELECT count(*) FROM history WHERE table_name = $1 AND key = $2::jsonb", [t, given]).rows[0][0]
        rt.run("BEGIN READ ONLY"); rt.run("SET LOCAL TimeZone = 'Asia/Tokyo'")
        c1 = rt.params(CANON.format(t=t), [given, "{id}"])
        rt.run("COMMIT")
        rt.run("BEGIN READ ONLY"); rt.run("SET LOCAL TimeZone = 'UTC'")
        c2 = rt.params(CANON.format(t=t), [given, "{id}"])
        rt.run("COMMIT")
        m1 = plat.params("SELECT count(*) FROM history WHERE table_name = $1 AND key = $2::jsonb", [t, c1.rows[0][0]]).rows[0][0] if c1.ok else c1
        m2 = plat.params("SELECT count(*) FROM history WHERE table_name = $1 AND key = $2::jsonb", [t, c2.rows[0][0]]).rows[0][0] if c2.ok else c2
        say(f"D4 {t}: stored {stored}; given {given}: matches as given {raw}; shaped in a1 under Tokyo time "
            f"{c1.rows[0][0] if c1.ok else c1} matches {m1}; under UTC {c2.rows[0][0] if c2.ok else c2} matches {m2}")


def capture_t_column(port, own, plat, ident, mig, rt):
    """D5: the capture writes to_jsonb(t) over `FROM vm_new t`: a column
    named t wins over the alias t. Today's own_db_schema.sql has the same
    text; checked on it too (database a0)."""
    as_migration(mig, ident, "CREATE TABLE public.tcol (id integer PRIMARY KEY, t text, x integer); SELECT vm.capture_new_tables();")
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("INSERT INTO public.tcol VALUES (1, 'just t', 5)")
    rt.run("COMMIT")
    got = own.val("SELECT new::text FROM vm.outbox WHERE table_name = 'tcol'")
    today = setup_today(port)
    tid = Identity(port, "a0", "a0_owner")
    today.run("BEGIN"); tid.attach(today, "migration")
    today.one("CREATE TABLE public.tcol (id integer PRIMARY KEY, t text, x integer); SELECT vm.capture_new_tables();")
    today.run("COMMIT")
    today.run("BEGIN"); tid.attach(today, "migration")
    today.one("INSERT INTO public.tcol VALUES (1, 'just t', 5)")
    today.run("COMMIT")
    got0 = today.val("SELECT new::text FROM vm.history WHERE table_name = 'tcol'")
    say(f"D5 a row (1, 'just t', 5) of a table with a column named t is recorded as: outbox design {got}; "
        f"today's crates/vm/src/own_db_schema.sql {got0}")
    # The writer's own settings and what today's capture (no pinned
    # settings) records.
    today.run("BEGIN"); tid.attach(today, "migration")
    today.one("CREATE TABLE public.f (id integer PRIMARY KEY, d double precision, z timestamptz, iv interval); SELECT vm.capture_new_tables();")
    today.run("COMMIT")
    today.run("BEGIN"); tid.attach(today, "migration")
    today.one("SET LOCAL extra_float_digits = -15; SET LOCAL TimeZone = 'Pacific/Chatham'; SET LOCAL IntervalStyle = 'sql_standard';"
              "INSERT INTO public.f VALUES (1, 0.1234567890123, '2026-11-12 10:00:00+00', '1 year 2 mons -3 days 04:05:06')")
    today.run("COMMIT")
    rec = today.val("SELECT new::text FROM vm.history WHERE table_name = 'f'")
    say(f"D5 today's capture, writer with extra_float_digits -15, TimeZone Pacific/Chatham, IntervalStyle sql_standard, "
        f"row (1, 0.1234567890123, 2026-11-12 10:00:00+00, 1 year 2 mons -3 days 04:05:06) recorded as {rec}")
    # The capture with a whole-row reference to an alias no column is likely
    # to have, written alias.*, then tables with columns named t and vm_row.
    body = own.val("SELECT pg_get_functiondef('vm.capture()'::regprocedure)")
    fixed = (body.replace("pg_catalog.to_jsonb(t) AS j", "pg_catalog.to_jsonb(vm_row.*) AS j")
             .replace("FROM vm_new t)", "FROM vm_new vm_row)").replace("FROM vm_old t)", "FROM vm_old vm_row)"))
    assert fixed != body
    assert own.one(fixed).ok
    as_migration(mig, ident, "CREATE TABLE public.tcol2 (id integer PRIMARY KEY, t text, vm_row text); "
                             "CREATE TYPE public.pair AS (a integer, b integer); "
                             "CREATE TABLE public.tcol3 (id integer PRIMARY KEY, vm_row public.pair); "
                             "SELECT vm.capture_new_tables();")
    rt.run("BEGIN"); ident.attach(rt)
    rt.one("INSERT INTO public.tcol2 VALUES (1, 'just t', 'just vm_row'); INSERT INTO public.tcol3 VALUES (1, ROW(1, 2))")
    rt.run("COMMIT")
    g2 = own.val("SELECT new::text FROM vm.outbox WHERE table_name = 'tcol2'")
    g3 = own.val("SELECT new::text FROM vm.outbox WHERE table_name = 'tcol3'")
    say(f"D5 with to_jsonb(vm_row.*) over FROM vm_new vm_row: columns t and vm_row text: {g2}; "
        f"a composite column named vm_row: {g3}")
    today.close()


if __name__ == "__main__":
    main()
