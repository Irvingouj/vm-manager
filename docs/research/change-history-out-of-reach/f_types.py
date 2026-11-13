"""Part F, `types`: with whose rights does Postgres run a user-defined type's
own code later? A range type carries a `subtype_diff` (and may carry a
`canonical`) function; ANALYZE of a table with a column of that range type runs
`subtype_diff`, and autovacuum's ANALYZE does the same, as the table's owner.
An IMMUTABLE function may not write, but may call a volatile one that does, so
this is the same owner-rights-without-login path as an index expression. The
catalog shows the function's owner (pg_range.rngsubdiff -> pg_proc.proowner),
so the platform can refuse such a type. Operator classes and a range's
canonical function are not a way in: each needs a superuser (shown below)."""
import sys
import time
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from pq import Conn
port = int(sys.argv[1])
mig = Conn(port, "a1_mig", "a1"); rt = Conn(port, "a1_rt", "a1"); su = Conn(port, "postgres", "a1", password="postgres")
def show(c, sql):
    r = c.one(sql); print(f"{('ok ' + str(r.rows))[:70] if r.ok else str(r)[:70]:72} <- {sql[:120]}"); return r

print("-- operator classes and families are a superuser's to make or alter, as the migration role")
for sql in [
    "CREATE FUNCTION cmp(int, int) RETURNS int LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN btint4cmp($1, $2); END $f$",
    "CREATE OPERATOR FAMILY fam USING btree",
    "CREATE OPERATOR CLASS opc FOR TYPE int USING btree AS OPERATOR 1 <, FUNCTION 1 cmp(int, int)",
    "ALTER OPERATOR FAMILY integer_ops USING btree ADD FUNCTION 1 (int, int) cmp(int, int)",
]:
    show(mig, sql)

print("-- a canonical function needs a shell type first, and a shell type is a superuser's to make")
for sql in [
    "CREATE TYPE cr",
    "CREATE FUNCTION canon(cr) RETURNS cr LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN $1; END $f$",
    "CREATE TYPE cr AS RANGE (subtype = int, canonical = canon)",
]:
    show(mig, sql)

print("-- a range type whose subtype_diff the migration role owns; the runtime role adds rows; the superuser's ANALYZE stands in for autovacuum")
for sql in [
    "CREATE TABLE seen (who text)",
    "GRANT INSERT ON seen TO a1_rt",
    "CREATE FUNCTION note() RETURNS void LANGUAGE plpgsql AS $f$ BEGIN INSERT INTO public.seen VALUES (current_user); END $f$",
    "CREATE FUNCTION diff_fn(int, int) RETURNS float8 LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN PERFORM public.note(); RETURN ($1 - $2)::float8; END $f$",
    "CREATE TYPE span AS RANGE (subtype = int, subtype_diff = diff_fn)",
    "CREATE TABLE t (id int, s span)",
    "GRANT INSERT ON t TO a1_rt",
]:
    show(mig, sql)
show(rt, "INSERT INTO t SELECT g, span(g, g + 10) FROM generate_series(1, 500) g")
print("rows recorded by the owner's code at insert time, as the runtime role:", su.one("SELECT count(*), bool_and(who='a1_rt') FROM public.seen").rows)
show(su, "DELETE FROM public.seen")
show(su, "SELECT pg_get_userbyid(relowner) FROM pg_class WHERE oid='public.t'::regclass")
show(su, "ANALYZE public.t")
print(">>> after a superuser ANALYZE, subtype_diff ran as:", su.one("SELECT who, count(*) FROM public.seen GROUP BY who").rows)
show(su, "DELETE FROM public.seen")
print("-- leave the table for autovacuum: the runtime role makes many rows, naptime is 1s")
show(rt, "INSERT INTO t SELECT g, span(g, g + 10) FROM generate_series(1, 5000) g")
for _ in range(60):
    if su.one("SELECT count(*) FROM public.seen").rows[0][0] != "0":
        break
    time.sleep(0.5)
print(">>> after autovacuum's ANALYZE, subtype_diff ran as:", su.one("SELECT who, count(*) FROM public.seen GROUP BY who").rows)
print("-- what the platform can see to refuse it: the range's functions and their owners")
print("user-defined ranges:", su.one(
    "SELECT g.rngtypid::regtype::text, pg_get_userbyid(sd.proowner), sd.proowner IN (SELECT oid FROM pg_roles WHERE rolsuper), "
    "pg_get_userbyid(cn.proowner) "
    "FROM pg_range g JOIN pg_proc sd ON sd.oid = g.rngsubdiff LEFT JOIN pg_proc cn ON cn.oid = NULLIF(g.rngcanonical,0) "
    "WHERE g.rngtypid >= 16384").rows)
print("built-in ranges' functions are all a superuser's:", su.one(
    "SELECT bool_and(o.rolsuper) FROM pg_range g JOIN pg_proc p ON p.oid IN (g.rngsubdiff, g.rngcanonical) "
    "JOIN pg_roles o ON o.oid = p.proowner WHERE g.rngtypid < 16384").rows)
