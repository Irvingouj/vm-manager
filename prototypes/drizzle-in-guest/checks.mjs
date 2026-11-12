// Checks against a running dig-host (strict mode). Prints PASS/FAIL lines,
// writes the statement capture to $CAPTURE_OUT, exits non-zero on a failure.
//   node checks.mjs http://127.0.0.1:8792
import { writeFileSync } from "node:fs";

const base = process.argv[2];
let failed = 0;
const get = async (path) => {
  const r = await fetch(base + path);
  const text = await r.text();
  try {
    return { status: r.status, body: JSON.parse(text), headers: r.headers };
  } catch {
    return { status: r.status, body: text, headers: r.headers };
  }
};
const check = (name, ok, detail) => {
  if (ok) console.log(`PASS ${name}`);
  else {
    failed++;
    console.log(`FAIL ${name}: ${JSON.stringify(detail)}`);
  }
};
const j = JSON.stringify;

// 1. Drizzle runs in the guest: select, insert, update, join, relational query
await get("/reset");
let r = await get("/tasks?owner=ada@example.com");
check("select with join and where", r.status === 200 && j(r.body.map((t) => t.title)) === j(["write the spec", "review the spec"]), r.body);
r = await get("/add?title=new");
check("insert returning", r.status === 200 && r.body.title === "new" && typeof r.body.id === "number", r.body);
r = await get("/done?title=ship%20it");
check("update returning", r.status === 200 && r.body.length === 1 && r.body[0].done === true, r.body);
r = await get("/per-user");
check("left join + count + group by", r.status === 200 && j(r.body) === j([{ name: "Ada", open: 2 }, { name: "Grace", open: 1 }]), r.body);
r = await get("/users-with-tasks");
check(
  "relational query (db.query ... with)",
  r.status === 200 && r.body.length === 2 && r.body[1].tasks.map((t) => t.title).includes("new"),
  r.body,
);
r = await get("/select-drizzle");
const viaRaw = await get("/select-raw");
check("Drizzle select equals the same raw host call's rows", r.status === 200 && r.body.length === viaRaw.body.length, { r: r.body, raw: viaRaw.body });

// Type fidelity
r = await get("/types");
check("types: every field round-trips as Drizzle's type promises", r.status === 200 && Object.values(r.body.expectSame ?? {}).every(Boolean), r.body.expectSame ?? r.body);
console.log("     types as the guest sees them: " + j(r.body.fields));
console.log("     row as the host sent it:      " + j(r.body.rawFromHost));
r = await get("/types-nested");
console.log("     relational (nested JSON) path: " + j(r.body));
check(
  "relational query, nested: numeric and bigint lose precision, timestamp string changes format (known, see run.sh node-postgres comparison)",
  r.body.fields?.estimate?.value === "12345678901234.123" &&
    r.body.fields?.bigCount?.value === "9007199254740992" &&
    r.body.fields?.dueAtString?.value === "2026-01-02T03:04:05.123456" &&
    r.body.expectSame?.blob === true && r.body.expectSame?.tags === true && r.body.expectSame?.dueAt === true,
  r.body,
);

// 2. Transactions
await get("/reset");
const count = async () => (await get("/count")).body.n;
let n0 = await count();
r = await get("/reassign");
check("transaction commits two writes", r.body.moved === 1 && (await count()) === n0 + 1, r.body);
n0 = await count();
r = await get("/reassign?fail=1");
const n1 = await count();
check("transaction rolls back both writes on a thrown error", r.status === 500 && n1 === n0, { r: r.body, n0, n1 });
r = await get("/tx-commit");
check("tx: reads its own writes inside", r.body.insideTx === 2, r.body);
r = await get("/tx-throw");
check("tx: error in callback rejects", r.body.err === "application error after two inserts", r.body);
r = await get("/tx-rollback-call");
check("tx: tx.rollback() rejects with Rollback", r.body.err === "Rollback", r.body);
r = await get("/tx-db-error");
check("tx: a database error rejects (FK violation)", (r.body.cause ?? "").startsWith("23503"), r.body);
r = await get("/tx-config");
check(
  "tx: isolation level and read only reach Postgres",
  r.body.ok?.iso === "serializable" && r.body.ok?.ro === "on" && (r.body.ok?.write?.cause ?? "").startsWith("25006"),
  r.body,
);
r = await get("/tx-nested");
check("tx: nested transaction refused (not built)", (r.body.err ?? "").includes("nested"), r.body);
r = await get("/tx-abandon");
check("tx: a transaction left open is rolled back by the host at request end", r.headers.get("x-abandoned-txs") === "1", r.headers.get("x-abandoned-txs"));
r = await get("/tx-titles");
check("tx: only committed rows exist", j(r.body) === j(["tx-a", "tx-b"]), r.body);

// 3. Raw SQL and the host's statement check
r = await get("/raw");
const raw = r.body;
check("raw: db.execute(sql`...`) is reachable from Application code", raw.execute?.ok?.[0]?.answer === 42, raw.execute);
check("raw: sql`` inside a typed query is reachable", Array.isArray(raw.rawInWhere?.ok), raw.rawInWhere);
check("strict host refuses a multi-statement string", (raw.multiStatement?.cause ?? "").includes("2 statements"), raw.multiStatement);
check("strict host refuses SET", (raw.setRole?.cause ?? "").startsWith("refused"), raw.setRole);
check("strict host refuses DDL", (raw.ddl?.cause ?? "").startsWith("refused"), raw.ddl);
check("but set_config() inside a SELECT passes the kind check", raw.setConfigInSelect?.ok?.[0]?.v === "from-the-guest", raw.setConfigInSelect);
check("read-only Resource: execute() hidden by types still exists; the role refuses", (raw.devopsExecuteDelete?.cause ?? "").startsWith("42501"), raw.devopsExecuteDelete);
check("read-only Resource: DELETE in a CTE passes the kind check (it is a SELECT); the role refuses", (raw.devopsDeleteInCte?.cause ?? "").startsWith("42501"), raw.devopsDeleteInCte);
r = await get("/appname");
check("session state set in one request is seen by the next one (shared connection)", r.body?.[0]?.v === "from-the-guest", r.body);

// 4/6. The pulled shared database, read-only
r = await get("/devops/services");
check("pulled schema: typed join on the shared database", r.status === 200 && r.body[0].service === "gateway" && r.body[0].cost === "1234.50", r.body);
r = await get("/devops/recent");
check("pulled schema: relational query + view", r.status === 200 && r.body.rows.length === 2 && r.body.summary.length === 2, r.body);

// The capture
const capture = (await get("/__capture")).body;
if (process.env.CAPTURE_OUT) writeFileSync(process.env.CAPTURE_OUT, capture.map((c) => j(c)).join("\n") + "\n");
const stmts = capture.filter((c) => c.sql !== undefined);
const placeholders = (s) => new Set(s.match(/\$\d+/g) ?? []).size;
const notOne = stmts.filter((c) => c.class.parsed !== true || c.class.count !== 1);
const paramMismatch = stmts.filter((c) => placeholders(c.sql) !== c.params.length);
const unparsed = stmts.filter((c) => c.class.parsed !== true);
console.log(`     capture: ${stmts.length} statements, ${capture.length - stmts.length} transaction operations`);
check("capture: the host's parser parsed every statement Drizzle sent", unparsed.length === 0, unparsed.map((c) => c.sql));
check(
  "capture: every statement but the deliberate multi-statement probe is one statement",
  notOne.length === 1 && notOne[0].sql === "select 1; select 2",
  notOne.map((c) => c.sql),
);
check("capture: every statement's params match its $n placeholders", paramMismatch.length === 0, paramMismatch.map((c) => c.sql));

console.log(failed ? `checks: ${failed} FAILED` : "checks: all passed");
process.exit(failed ? 1 : 0);
