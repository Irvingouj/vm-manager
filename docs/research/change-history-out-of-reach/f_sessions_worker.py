"""One client of f_sessions.py's throughput run: small write transactions for
SECONDS, with or without the look at the session's own strong locks."""
import sys
import time
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from pq import Conn
from f_sessions import LOCKS
port, i, checked, seconds = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3] == "1", float(sys.argv[4])
c = Conn(port, "a1_mig", "a1")
sql = f"BEGIN; INSERT INTO w{i % 16} VALUES (1)" + (f"; {LOCKS}; COMMIT" if checked else "; COMMIT")
sys.stdin.readline()  # all clients start together
stop = time.perf_counter() + seconds
n = 0
while time.perf_counter() < stop:
    c.run(sql)
    n += 1
print(n)
