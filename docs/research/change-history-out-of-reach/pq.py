"""A minimal Postgres client over the libpq that ships with the Postgres
binaries in ~/.cache/vm-blackbox (there is no psql there). Standard library
only. Used by every script of this research; run Python with -I.

    c = Conn(port, user, db)            # password = the role's name (scratch clusters only)
    c.run(sql)        -> [Result]       # simple-query protocol, may hold many statements
    c.params(sql, [..]) -> Result       # extended protocol, one statement, text parameters
    c.status()        -> 'idle' | 'intrans' | 'inerror' | 'active' | 'unknown'
"""
import ctypes
import os

LIB = os.environ["PQ_LIB"]  # path to libpq.so, set by run.sh
pq = ctypes.CDLL(LIB)
P = ctypes.c_void_p
pq.PQconnectdb.restype = P
pq.PQconnectdb.argtypes = [ctypes.c_char_p]
pq.PQstatus.argtypes = [P]
pq.PQerrorMessage.restype = ctypes.c_char_p
pq.PQerrorMessage.argtypes = [P]
pq.PQfinish.argtypes = [P]
pq.PQsendQuery.argtypes = [P, ctypes.c_char_p]
pq.PQgetResult.restype = P
pq.PQgetResult.argtypes = [P]
pq.PQresultStatus.argtypes = [P]
pq.PQresultErrorField.restype = ctypes.c_char_p
pq.PQresultErrorField.argtypes = [P, ctypes.c_int]
pq.PQntuples.argtypes = [P]
pq.PQnfields.argtypes = [P]
pq.PQgetvalue.restype = ctypes.c_char_p
pq.PQgetvalue.argtypes = [P, ctypes.c_int, ctypes.c_int]
pq.PQgetisnull.argtypes = [P, ctypes.c_int, ctypes.c_int]
pq.PQcmdStatus.restype = ctypes.c_char_p
pq.PQcmdStatus.argtypes = [P]
pq.PQclear.argtypes = [P]
pq.PQtransactionStatus.argtypes = [P]
pq.PQbackendPID.argtypes = [P]
pq.PQexecParams.restype = P
pq.PQexecParams.argtypes = [P, ctypes.c_char_p, ctypes.c_int, P,
                            ctypes.POINTER(ctypes.c_char_p), P, P, ctypes.c_int]
pq.PQputCopyData.argtypes = [P, ctypes.c_char_p, ctypes.c_int]
pq.PQputCopyEnd.argtypes = [P, ctypes.c_char_p]
NOTICE = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_char_p)
pq.PQsetNoticeProcessor.argtypes = [P, NOTICE, ctypes.c_void_p]

PG_DIAG_SQLSTATE = ord("C")
PG_DIAG_MESSAGE_PRIMARY = ord("M")
OK = {1, 2, 4}  # PGRES_COMMAND_OK, TUPLES_OK, COPY_IN
FATAL = 7
TX = {0: "idle", 1: "active", 2: "intrans", 3: "inerror", 4: "unknown"}


class Result:
    def __init__(self, ok, sqlstate=None, message=None, rows=None, cmd=None):
        self.ok, self.sqlstate, self.message = ok, sqlstate, message
        self.rows, self.cmd = rows or [], cmd

    def __repr__(self):
        if self.ok:
            return f"ok {self.cmd} {self.rows[:3]}"
        return f"ERROR {self.sqlstate} {self.message}"


def _take(res):
    st = pq.PQresultStatus(res)
    if st in OK:
        rows = []
        for i in range(pq.PQntuples(res)):
            rows.append(tuple(None if pq.PQgetisnull(res, i, j) else
                              pq.PQgetvalue(res, i, j).decode()
                              for j in range(pq.PQnfields(res))))
        return Result(True, rows=rows, cmd=pq.PQcmdStatus(res).decode())
    state = pq.PQresultErrorField(res, PG_DIAG_SQLSTATE)
    msg = pq.PQresultErrorField(res, PG_DIAG_MESSAGE_PRIMARY)
    return Result(False, state.decode() if state else None,
                  msg.decode() if msg else f"status {st}")


class ConnectError(Exception):
    pass


class Conn:
    def __init__(self, port, user, db, password=None):
        self.notices = []
        info = (f"host=127.0.0.1 port={port} user={user} dbname={db} "
                f"password={password or user} connect_timeout=5 application_name=research")
        self.c = pq.PQconnectdb(info.encode())
        if pq.PQstatus(self.c) != 0:
            err = pq.PQerrorMessage(self.c).decode().strip()
            pq.PQfinish(self.c)
            self.c = None
            raise ConnectError(err)
        self._np = NOTICE(lambda _a, m: self.notices.append(m.decode().strip()))
        pq.PQsetNoticeProcessor(self.c, self._np, None)
        self.pid = pq.PQbackendPID(self.c)

    def run(self, sql):
        """Every result of a simple query (several statements allowed)."""
        if not pq.PQsendQuery(self.c, sql.encode()):
            return [Result(False, None, pq.PQerrorMessage(self.c).decode().strip())]
        out = []
        while True:
            r = pq.PQgetResult(self.c)
            if not r:
                break
            out.append(_take(r))
            pq.PQclear(r)
        return out

    def one(self, sql):
        """The last result of a simple query, or its first error."""
        rs = self.run(sql)
        for r in rs:
            if not r.ok:
                return r
        return rs[-1] if rs else Result(True)

    def val(self, sql):
        r = self.one(sql)
        if not r.ok:
            raise RuntimeError(f"{sql!r}: {r}")
        return r.rows[0][0] if r.rows else None

    def params(self, sql, values):
        arr = (ctypes.c_char_p * max(1, len(values)))(
            *[None if v is None else str(v).encode() for v in values])
        r = pq.PQexecParams(self.c, sql.encode(), len(values), None, arr, None, None, 0)
        out = _take(r)
        pq.PQclear(r)
        return out

    def copy_in(self, sql, data):
        """COPY ... FROM STDIN with `data` (text format)."""
        pq.PQsendQuery(self.c, sql.encode())
        r = pq.PQgetResult(self.c)
        st = pq.PQresultStatus(r)
        pq.PQclear(r)
        if st != 4:
            return Result(False, None, f"no copy-in ({st})")
        b = data.encode()
        pq.PQputCopyData(self.c, b, len(b))
        pq.PQputCopyEnd(self.c, None)
        out = Result(True)
        while True:
            r = pq.PQgetResult(self.c)
            if not r:
                break
            out = _take(r)
            pq.PQclear(r)
        return out

    def status(self):
        return TX.get(pq.PQtransactionStatus(self.c), "?")

    def close(self):
        if self.c:
            pq.PQfinish(self.c)
            self.c = None
