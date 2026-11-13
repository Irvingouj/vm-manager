"""psql-like: q.py PORT ROLE DB [-c SQL]... ; with no -c, reads SQL from stdin. Prints rows, or the error."""
import sys
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from pq import Conn
port, role, db = sys.argv[1:4]
rest = sys.argv[4:]
sqls = [rest[i + 1] for i in range(0, len(rest), 2)] if rest else [sys.stdin.read()]
c = Conn(int(port), role, db)
for sql in sqls:
    for r in c.run(sql):
        if r.ok:
            for row in r.rows:
                print("|".join("" if v is None else str(v) for v in row))
        else:
            print(f"ERROR {r.sqlstate}: {r.message}")
