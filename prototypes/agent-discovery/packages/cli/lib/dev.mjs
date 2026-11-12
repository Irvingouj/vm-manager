// The local host behind `vm dev`: runs the Application on this machine against
// local stand-ins, and enforces the same rules as the platform: the Manifest's
// Declared access and the test Users' Grants from vm.dev.json.
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";
import { PGlite, types } from "@electric-sql/pglite";
import { build } from "esbuild";
import { bundle, pulledNames } from "./bundle.mjs";
import { requireManifest } from "./manifest.mjs";
import { journal, readMigration, statements } from "./migrations.mjs";
import { rank } from "./installation.mjs";
import { VmError, ownRequire, readJson } from "./util.mjs";

const raw = (x) => x;
const PARSERS = {
  [types.TIMESTAMP]: raw,
  [types.TIMESTAMPTZ]: raw,
  [types.DATE]: raw,
  [types.TIME]: raw,
  [types.TIMETZ]: raw,
  [types.INTERVAL]: raw,
  [types.INT8]: raw,
  [types.NUMERIC]: raw,
};

const HOST = Symbol.for("vm.host");

/** read, write, or forbidden, from the statement text. */
export function classify(sql) {
  const s = sql
    .replace(/'(?:[^']|'')*'/g, "''")
    .replace(/"(?:[^"]|"")*"/g, '""')
    .replace(/--.*$/gm, "")
    .trim()
    .toLowerCase();
  if (/^(create|alter|drop|truncate|grant|revoke|comment|reindex|vacuum|cluster|copy|set|reset|do|call|lock|listen|notify|begin|start|commit|end|rollback|savepoint|release|prepare|execute|deallocate|discard|load|security|refresh|import)\b/.test(s)) return "forbidden";
  if (/\b(insert|update|delete|merge)\b/.test(s)) return "write";
  return "read";
}

function rewriteSchema(sql, schema) {
  return sql.replaceAll('"public".', `"${schema}".`);
}

function signature(root) {
  const h = createHash("sha256");
  const walk = (p) => {
    if (!existsSync(p)) return;
    const st = statSync(p);
    if (st.isDirectory()) {
      for (const f of readdirSync(p).sort()) if (f !== "node_modules" && !f.startsWith(".")) walk(join(p, f));
    } else h.update(`${p}:${st.size}:${st.mtimeMs}\n`);
  };
  for (const p of ["manifest.ts", "src", "resources", "seed", "migrations", "vm.dev.json", "tsconfig.json"]) walk(join(root, p));
  return h.digest("hex");
}

const fileHash = (...files) => {
  const h = createHash("sha256");
  for (const f of files) h.update(existsSync(f) ? readFileSync(f) : "");
  return h.digest("hex").slice(0, 16);
};

export class LocalHost {
  constructor(root, { log = () => {} } = {}) {
    this.root = root;
    this.dir = join(root, ".vm", "dev");
    this.log = log;
    this.inflight = new Map();
    this.txs = new Map();
    this.queue = Promise.resolve();
    this.sig = null;
    this.version = 0;
  }

  async open({ reset = false } = {}) {
    mkdirSync(this.dir, { recursive: true });
    const data = join(this.dir, "pgdata");
    if (reset) rmSync(data, { recursive: true, force: true });
    this.pg = await PGlite.create(data, { parsers: PARSERS });
    await this.pg.exec(`create schema if not exists vm_meta;
      create table if not exists vm_meta.standins (name text primary key, fingerprint text not null, rows integer not null);
      create table if not exists vm_meta.migrations (tag text primary key, hash text not null);
      create schema if not exists app;`);
    globalThis[HOST] = { invoke: (op, args) => this.invoke(op, args), user: (id) => this.userOf(id) };
  }

  async close() {
    if (this.pg) await this.pg.close();
    this.pg = null;
  }

  /** Rebuilds what changed since the last request: manifest, stand-ins, migrations, the bundle. */
  async refresh() {
    const sig = signature(this.root);
    if (sig === this.sig) return this.summary;
    this.manifest = requireManifest(this.root);
    this.dev = this.readDev();
    const standins = [];
    for (const name of pulledNames(this.root)) standins.push(await this.standin(name));
    this.standins = new Map(standins.map((s) => [s.name, s]));
    const migrations = await this.migrate();
    const out = await bundle(this.root, { mode: "dev" });
    const mod = await import(`${pathToFileURL(out.outfile).href}?v=${++this.version}`);
    this.app = mod.default;
    this.sig = sig;
    const missing = Object.keys(this.manifest.resources).filter((n) => !this.standins.has(n));
    this.summary = { manifest: this.manifest, standins, migrations, users: this.dev.users, missing };
    return this.summary;
  }

  readDev() {
    const f = join(this.root, "vm.dev.json");
    if (!existsSync(f)) throw new VmError("vm.dev.json is missing. It lists the test Users and their Grants for `vm dev`.");
    let dev;
    try {
      dev = readJson(f);
    } catch (e) {
      throw new VmError(`vm.dev.json is not valid JSON: ${e.message}`);
    }
    if (!Array.isArray(dev.users) || dev.users.length === 0) throw new VmError("vm.dev.json must list at least one test User in \"users\".");
    for (const u of dev.users) {
      if (!u.id) throw new VmError("Every test User in vm.dev.json needs an \"id\".");
      u.groups ??= [];
      u.displayName ??= u.name ?? u.id;
      u.email ??= `${u.id}@example.com`;
    }
    dev.grants ??= [];
    for (const g of dev.grants) {
      if (!["structure", "read", "write"].includes(g.access)) throw new VmError(`vm.dev.json: grant access must be structure, read or write (found ${JSON.stringify(g.access)}).`);
    }
    return dev;
  }

  async standin(name) {
    const dir = join(this.root, "resources", name);
    const seedFile = join(this.root, "seed", `${name}.sql`);
    const fingerprint = fileHash(join(dir, "schema.ts"), join(dir, "relations.ts"), seedFile);
    const { rows } = await this.pg.query("select fingerprint, rows from vm_meta.standins where name = $1", [name]);
    if (rows[0]?.fingerprint === fingerprint) return { name, rows: rows[0].rows, rebuilt: false };

    const tmp = join(this.dir, "tmp");
    mkdirSync(tmp, { recursive: true });
    const outfile = join(tmp, `${name}.schema.mjs`);
    await build({ entryPoints: [join(dir, "schema.ts")], bundle: true, format: "esm", platform: "node", packages: "external", outfile, logLevel: "silent" });
    const mod = await import(`${pathToFileURL(outfile).href}?v=${fingerprint}`);
    const kit = ownRequire("drizzle-kit/api");
    const ddl = await kit.generateMigration(kit.generateDrizzleJson({}), kit.generateDrizzleJson({ ...mod }));
    const schema = `r_${name}`;
    let count = 0;
    await this.pg.transaction(async (t) => {
      await t.exec(`drop schema if exists "${schema}" cascade; create schema "${schema}"; set local search_path to "${schema}";`);
      for (const st of ddl) await t.exec(rewriteSchema(st, schema));
      if (existsSync(seedFile)) {
        try {
          await t.exec(readFileSync(seedFile, "utf8"));
        } catch (e) {
          throw new VmError(`seed/${name}.sql does not load into the local '${name}': ${e.message}`);
        }
      }
      const tables = await t.query(`select table_name from information_schema.tables where table_schema = $1`, [schema]);
      for (const { table_name } of tables.rows) {
        const r = await t.query(`select count(*)::int as n from "${schema}"."${table_name}"`);
        count += r.rows[0].n;
      }
      await t.query(
        "insert into vm_meta.standins (name, fingerprint, rows) values ($1, $2, $3) on conflict (name) do update set fingerprint = $2, rows = $3",
        [name, fingerprint, count],
      );
    });
    return { name, rows: count, rebuilt: true };
  }

  async migrate() {
    const dir = join(this.root, "migrations");
    const entries = journal(dir);
    const { rows: applied } = await this.pg.query("select tag, hash from vm_meta.migrations");
    const appliedMap = new Map(applied.map((r) => [r.tag, r.hash]));
    const tags = new Set(entries.map((e) => e.tag));
    for (const r of applied) {
      if (!tags.has(r.tag)) throw new VmError(`The local own database has migration ${r.tag}, which is no longer in migrations/. Run \`npx vm dev --reset\` to rebuild it (this clears local test data).`);
    }
    const newly = [];
    for (const e of entries) {
      const sql = readMigration(dir, e.tag);
      const hash = createHash("sha256").update(sql).digest("hex").slice(0, 16);
      if (appliedMap.has(e.tag)) {
        if (appliedMap.get(e.tag) !== hash) throw new VmError(`migrations/${e.tag}.sql changed after it was applied locally. Migrations are not edited once generated. Run \`npx vm dev --reset\` to rebuild the local own database.`);
        continue;
      }
      try {
        await this.pg.transaction(async (t) => {
          await t.exec(`set local search_path to app`);
          for (const st of statements(sql)) await t.exec(rewriteSchema(st, "app"));
          await t.query("insert into vm_meta.migrations (tag, hash) values ($1, $2)", [e.tag, hash]);
        });
      } catch (err) {
        if (err instanceof VmError) throw err;
        throw new VmError(`migrations/${e.tag}.sql failed on the local own database: ${err.message}`);
      }
      newly.push(e.tag);
    }
    return { total: entries.length, applied: newly };
  }

  heldLevel(user, resource) {
    let best = "none";
    for (const g of this.dev.grants) {
      if (g.resource === resource && user.groups.includes(g.group) && rank(g.access) > rank(best)) best = g.access;
    }
    return best;
  }

  userOf(requestId) {
    const u = this.inflight.get(requestId);
    if (!u) return { refused: noRequest() };
    return { user: { id: u.id, email: u.email, displayName: u.displayName } };
  }

  /** Decides whether an operation may run, and where. */
  authorize(args, user, needed) {
    if (args.db === null || args.db === undefined) return { schema: "app", readOnly: false, label: "own" };
    const name = args.db;
    if (args.as === "app") {
      return refuse({ reason: "not-own-database", resource: name }, `An Application acts as itself only in its own database. Use ctx.user().db("${name}") to reach '${name}' as the signed-in User.`);
    }
    const decl = this.manifest.resources[name];
    if (!decl) {
      return refuse({ reason: "not-declared", resource: name }, `'${name}' is not declared in manifest.ts. Add it under resources with access "read" or "write", then run: npx vm resources pull ${name}`);
    }
    if (rank(needed) > rank(decl.access)) {
      return refuse(
        { reason: "declared-lower", resource: name, declared: decl.access, needed },
        `'${name}' is declared at '${decl.access}' in manifest.ts and this operation needs '${needed}'. Declare it at '${needed}' if the Application must change rows there.`,
      );
    }
    const held = this.heldLevel(user, name);
    if (rank(held) < rank(needed)) {
      const has = held === "none" ? "holds no Grant on" : `holds only '${held}' on`;
      return refuse(
        { reason: "no-grant", resource: name, user: user.id, needed, held: held === "none" ? null : held },
        `${user.id} ${has} '${name}' and this needs '${needed}'. Access comes from a Grant to one of the User's Groups and cannot be changed by the Application. (Locally, test Users' Groups and Grants are in vm.dev.json.)`,
      );
    }
    if (!this.standins.has(name)) {
      return refuse({ reason: "not-registered", resource: name }, `The structure of '${name}' has not been pulled. Run: npx vm resources pull ${name}`);
    }
    return { schema: `r_${name}`, readOnly: decl.access === "read", label: name };
  }

  async invoke(op, args) {
    const user = this.inflight.get(args.requestId);
    if (!user) return { refused: noRequest() };
    if (op === "sql") return this.sql(args, user);
    if (op === "tx.begin") {
      const where = this.authorize(args, user, "read");
      if (where.refused) return this.logRefused(user, args, where, "begin");
      const id = this.txs.size + 1 + Math.floor(Math.random() * 1e6);
      await this.pg.exec(`begin${where.readOnly ? " read only" : ""}; set local search_path to "${where.schema}";`);
      this.txs.set(id, { ...where, requestId: args.requestId });
      return { tx: id };
    }
    if (op === "tx.commit" || op === "tx.rollback") {
      const t = this.txs.get(args.tx);
      if (!t) throw new Error("No such transaction.");
      this.txs.delete(args.tx);
      await this.pg.exec(op === "tx.commit" ? "commit" : "rollback");
      return {};
    }
    throw new Error(`Unknown host operation ${op}`);
  }

  async sql(args, user) {
    const kind = classify(args.sql);
    if (kind === "forbidden") {
      throw new Error("Statements that change structure, transactions or session settings are not allowed at run time. Change the own database's structure in src/db/schema.ts and run `npx vm db generate`; use db.transaction(...) for transactions.");
    }
    const where = this.authorize(args, user, kind);
    if (where.refused) return this.logRefused(user, args, where, args.sql);
    const opts = { rowMode: args.method === "all" ? "array" : "object" };
    let result;
    try {
      if (args.tx !== undefined && args.tx !== null) {
        const t = this.txs.get(args.tx);
        if (!t || t.requestId !== args.requestId) throw new Error("The transaction is not open.");
        result = await this.pg.query(args.sql, args.params, opts);
      } else if (this.txs.size > 0) {
        await this.pg.exec(`set local search_path to "${where.schema}"`);
        result = await this.pg.query(args.sql, args.params, opts);
      } else {
        result = await this.pg.transaction(async (t) => {
          if (where.readOnly) await t.exec("set transaction read only");
          await t.exec(`set local search_path to "${where.schema}"`);
          return t.query(args.sql, args.params, opts);
        });
      }
    } catch (e) {
      if (e.code === "25006" && where.label !== "own") {
        const decl = this.manifest.resources[where.label];
        return this.logRefused(user, args, refuse(
          { reason: "declared-lower", resource: where.label, declared: decl.access, needed: "write" },
          `'${where.label}' is declared at '${decl.access}' in manifest.ts and this statement changes rows. Declare it at 'write' if the Application must change rows there.`,
        ), args.sql);
      }
      this.oplog(user, args, `error: ${e.message}`, args.sql);
      throw e;
    }
    const n = result.affectedRows || result.rows.length;
    this.oplog(user, args, `ok ${n} row${n === 1 ? "" : "s"}`, args.sql);
    return { rows: result.rows };
  }

  oplog(user, args, outcome, statement) {
    const entry = {
      at: new Date().toISOString(),
      user: user.id,
      as: args.as === "app" ? "application" : "user",
      resource: args.db ?? "(own database)",
      statement: String(statement).replace(/\s+/g, " ").slice(0, 200),
      outcome,
    };
    try {
      appendFileSync(join(this.dir, "operations.log"), JSON.stringify(entry) + "\n");
    } catch {}
    this.log(`  ${entry.as === "user" ? entry.user : "application"} -> ${entry.resource}: ${entry.statement.slice(0, 90)}  [${outcome}]`);
  }

  logRefused(user, args, where, statement) {
    this.oplog(user, args, `refused ${where.refused.refusal.reason}`, statement);
    return where;
  }

  /** Runs one request as a test User, one at a time. */
  handle(request, userId) {
    const run = async () => {
      await this.refresh();
      const user = this.dev.users.find((u) => u.id === userId);
      if (!user) {
        return new Response(`No test User '${userId}' in vm.dev.json. Test Users: ${this.dev.users.map((u) => u.id).join(", ")}`, { status: 400 });
      }
      const requestId = randomUUID();
      this.inflight.set(requestId, user);
      try {
        const res = await this.app.fetch(request, { requestId });
        if (!(res instanceof Response)) throw new Error("fetch must return a Response.");
        const body = await res.arrayBuffer();
        return new Response(body, { status: res.status, statusText: res.statusText, headers: res.headers });
      } catch (e) {
        this.log(`  error: ${e.stack ?? e}`);
        return new Response(`Uncaught error in the Application:\n${e.stack ?? e}\n${e.cause ? `Caused by: ${e.cause.message ?? e.cause}\n` : ""}`, {
          status: 500,
          headers: { "content-type": "text/plain; charset=utf-8" },
        });
      } finally {
        this.inflight.delete(requestId);
        for (const [id, t] of this.txs) {
          if (t.requestId === requestId) {
            this.txs.delete(id);
            await this.pg.exec("rollback");
          }
        }
      }
    };
    const p = this.queue.then(run, run);
    this.queue = p.catch(() => {});
    return p;
  }
}

function refuse(refusal, fix) {
  return { refused: { refusal, fix } };
}

function noRequest() {
  return {
    refusal: { reason: "no-request" },
    fix: "No request is in flight. Use ctx only inside fetch while the request is being handled, not at module top level or after the response.",
  };
}
