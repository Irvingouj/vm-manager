import sys
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from pq import Conn
port = int(sys.argv[1])
mig = Conn(port, "a1_mig", "a1"); su = Conn(port, "postgres", "a1", password="postgres")
for sql in ["CREATE EXTENSION pg_trgm",
            "CREATE FUNCTION evil(int) RETURNS int LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN $1; END $f$",
            "ALTER EXTENSION pg_trgm ADD FUNCTION evil(int)",
            "CREATE OR REPLACE FUNCTION evil(int) RETURNS int LANGUAGE plpgsql IMMUTABLE AS $f$ BEGIN RETURN $1 + 1; END $f$",
            "CREATE OR REPLACE FUNCTION similarity(text, text) RETURNS real LANGUAGE sql IMMUTABLE AS 'SELECT 1::real'",
            "ALTER FUNCTION similarity(text, text) OWNER TO a1_mig",
            "CREATE TABLE te (id int)", "CREATE INDEX te_evil ON te (evil(id))"]:
    print(f"{str(mig.one(sql))[:110]:112} <- {sql[:70]}")
print(su.run("SELECT e.extname, pg_get_userbyid(e.extowner) FROM pg_extension e")[0].rows)
print(su.run("SELECT p.proname, pg_get_userbyid(p.proowner), (SELECT r.rolsuper FROM pg_roles r WHERE r.oid = p.proowner), EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e') AS member FROM pg_proc p WHERE p.proname IN ('evil', 'similarity')")[0].rows)
