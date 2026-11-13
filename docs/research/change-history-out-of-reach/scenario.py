"""Runs a scenario file: SQL steps over named connections, each with what is
expected of it, printing one line per step. Usage: scenario.py PORT FILE...

Directives (each a line of its own):
  -- @conn NAME USER DB          open (once) and switch to a connection;
                                 the password is the role's name
  -- @close NAME
  -- @step ID EXPECT TITLE       the SQL up to the next directive, sent as
                                 one simple query (several statements allowed)
  -- @attach ACTOR [MIGRATION]   attach a signed identity to the current
                                 connection's open transaction, as the host
                                 would (keys read as a1_vm)
  -- @login ID EXPECT USER DB PASSWORD   try to log in
  -- @file ID EXPECT PATH        send a file (relative to this script) as one query
  -- @tx ID STATUS               the current connection's transaction status
EXPECT: ok | err | err:SQLSTATE | any | rows:N | has:TEXT | lacks:TEXT |
        notice:TEXT | nonotice:TEXT | same:ID | differs:ID
An error left open is rolled back after its step.
"""
import hashlib
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pq import Conn, ConnectError  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
PGV = os.environ.get("PGV", "?")


class Runner:
    def __init__(self, port):
        self.port = port
        self.conns = {}
        self.cur = None
        self.outputs = {}
        self.unexpected = 0
        self.keys = None

    def show(self, sid, verdict, got, title):
        got = got.replace("\n", " ")
        if len(got) > 230:
            got = got[:227] + "..."
        print(f"[pg{PGV}] {sid:<6} {verdict:<11} {got}  -- {title}", flush=True)

    def judge(self, expect, res, notices):
        out = repr(res)
        if expect == "any":
            return True
        if expect == "ok":
            return res.ok
        if expect == "err":
            return not res.ok
        if expect.startswith("err:"):
            return (not res.ok) and res.sqlstate == expect[4:]
        if expect.startswith("rows:"):
            return res.ok and len(res.rows) == int(expect[5:])
        if expect.startswith("has:"):
            return res.ok and expect[4:] in str(res.rows)
        if expect.startswith("lacks:"):
            return res.ok and expect[6:] not in str(res.rows)
        if expect.startswith("notice:"):
            return any(expect[7:] in n for n in notices)
        if expect.startswith("nonotice:"):
            return not any(expect[9:] in n for n in notices)
        if expect.startswith("same:"):
            return res.ok and self.outputs.get(expect[5:]) == res.rows
        if expect.startswith("differs:"):
            return res.ok and self.outputs.get(expect[8:]) != res.rows
        raise ValueError(f"unknown expectation {expect}")

    def step(self, sid, expect, title, sql):
        c = self.conns[self.cur]
        c.notices.clear()
        res = c.one(sql)
        notices = list(c.notices)
        self.outputs[sid] = res.rows if res.ok else None
        ok = self.judge(expect, res, notices)
        got = repr(res)
        if notices:
            got += " NOTICES " + " | ".join(notices)
        if c.status() == "inerror":
            c.run("ROLLBACK")
            got += " (rolled back)"
        if not ok:
            self.unexpected += 1
        self.show(sid, "as-expected" if ok else "UNEXPECTED", f"[{self.cur}] {got}", f"{title} (expect {expect})")

    def attach(self, actor, migration):
        if self.keys is None:
            k = Conn(self.port, "a1_vm", "a1")
            row = k.one("SELECT encode(k1,'hex'), encode(k2,'hex') FROM vm.keys").rows[0]
            self.keys = (bytes.fromhex(row[0]), bytes.fromhex(row[1]))
            k.close()
        c = self.conns[self.cur]
        txkey = c.val("SELECT vm.transaction_key()")
        who = json.dumps({"user_id": "contributor@example", "application": "a1", "actor": actor,
                          "request": None if actor == "migration" else "req-1",
                          "migration": migration}, separators=(",", ":"))
        inner = hashlib.sha256(self.keys[0] + f"vm.ctx v1\n{txkey}\n{who}".encode()).digest()
        tag = hashlib.sha256(self.keys[1] + inner).hexdigest()
        r = c.params("SELECT pg_catalog.set_config('vm.ctx', $1, true)", [tag + who])
        if not r.ok:
            raise RuntimeError(f"attach failed: {r}")

    def directive(self, words, body):
        kind = words[0]
        if kind == "conn":
            name, user, db = words[1:4]
            if name not in self.conns:
                self.conns[name] = Conn(self.port, user, db)
            self.cur = name
        elif kind == "close":
            self.conns.pop(words[1]).close()
        elif kind == "step":
            self.step(words[1], words[2], " ".join(words[3:]), body)
        elif kind == "attach":
            self.attach(words[1], words[2] if len(words) > 2 else None)
        elif kind == "file":
            with open(os.path.join(HERE, words[3])) as f:
                self.step(words[1], words[2], f"file {words[3]}", f.read())
        elif kind == "tx":
            st = self.conns[self.cur].status()
            ok = st == words[2]
            self.unexpected += 0 if ok else 1
            self.show(words[1], "as-expected" if ok else "UNEXPECTED", f"[{self.cur}] status {st}", f"transaction status (expect {words[2]})")
        elif kind == "login":
            sid, expect, user, db, pw = words[1:6]
            try:
                Conn(self.port, user, db, pw).close()
                ok, got = expect == "ok", "logged in"
            except ConnectError as e:
                ok, got = expect == "err", f"refused: {e}"
            self.unexpected += 0 if ok else 1
            self.show(sid, "as-expected" if ok else "UNEXPECTED", got, f"log in as {user} (expect {expect})")
        else:
            raise ValueError(kind)

    def run_file(self, path):
        print(f"[pg{PGV}] ==== {os.path.basename(path)}", flush=True)
        pending, body = None, []
        with open(path) as f:
            lines = f.read().split("\n")
        for line in lines + ["-- @end"]:
            if line.startswith("-- @"):
                if pending:
                    self.directive(pending, "\n".join(body))
                words = line[4:].split()
                pending, body = (None if words[0] == "end" else words), []
            elif pending:
                body.append(line)


def main():
    r = Runner(int(sys.argv[1]))
    for path in sys.argv[2:]:
        r.run_file(path)
    for c in r.conns.values():
        c.close()
    print(f"[pg{PGV}] steps not as expected: {r.unexpected}", flush=True)


if __name__ == "__main__":
    main()
