// Glue between the platform host and an Application. The platform's build step
// wraps the Application with `serve(app, structures)`; Application code does not
// import this module.
//
// The context holds no identity. Every operation names the request it belongs
// to, and the host decides who is acting and whether the operation is allowed.
import { createTableRelationsHelpers, extractTablesRelationalConfig } from "drizzle-orm";
import { PgDialect } from "drizzle-orm/pg-core";
import { PgProxyTransaction, PgRemoteDatabase, PgRemoteSession } from "drizzle-orm/pg-proxy";
import { Refused } from "./refused.js";

const HOST = Symbol.for("vm.host");

function host() {
  const h = globalThis[HOST];
  if (!h) throw new Error("No platform host is present. Run the Application with `npx vm dev`.");
  return h;
}

async function invoke(op, args) {
  const result = await host().invoke(op, args);
  if (result && result.refused) throw new Refused(result.refused.refusal, result.refused.fix);
  return result;
}

function encodeParam(v) {
  if (typeof v === "bigint") return v.toString();
  if (v instanceof Date) return v.toISOString();
  if (v instanceof Uint8Array) {
    let hex = "\\x";
    for (const b of v) hex += b.toString(16).padStart(2, "0");
    return hex;
  }
  return v;
}

function callback(target, tx) {
  return async (sql, params, method) => {
    const { rows } = await invoke("sql", { ...target, sql, params: params.map(encodeParam), method, tx });
    return { rows };
  };
}

class HostTransaction extends PgProxyTransaction {
  async transaction() {
    throw new Error("Nested transactions are not supported.");
  }
}

class HostSession extends PgRemoteSession {
  constructor(target, dialect, rel, txId) {
    super(callback(target, txId), dialect, rel);
    this.target = target;
    this.dialectRef = dialect;
    this.rel = rel;
    this.txId = txId;
  }

  // Drizzle wraps every driver error in DrizzleQueryError. A refusal must reach
  // the Application as Refused itself, so it is unwrapped here.
  prepareQuery(...args) {
    const q = super.prepareQuery(...args);
    for (const m of ["execute", "all", "values"]) {
      if (typeof q[m] !== "function") continue;
      const orig = q[m].bind(q);
      q[m] = async (...a) => {
        try {
          return await orig(...a);
        } catch (e) {
          for (let c = e; c; c = c.cause) if (c instanceof Refused) throw c;
          throw e;
        }
      };
    }
    return q;
  }

  async transaction(fn, config) {
    if (this.txId !== undefined) throw new Error("Nested transactions are not supported.");
    const { tx: id } = await invoke("tx.begin", { ...this.target, config: config ?? null });
    const session = new HostSession(this.target, this.dialectRef, this.rel, id);
    const tx = new HostTransaction(this.dialectRef, session, this.rel);
    let result;
    try {
      result = await fn(tx);
    } catch (e) {
      await invoke("tx.rollback", { ...this.target, tx: id });
      throw e;
    }
    await invoke("tx.commit", { ...this.target, tx: id });
    return result;
  }
}

function database(target, schema) {
  const dialect = new PgDialect();
  let rel;
  if (schema && Object.keys(schema).length > 0) {
    const tables = extractTablesRelationalConfig(schema, createTableRelationsHelpers);
    rel = { fullSchema: schema, schema: tables.tables, tableNamesMap: tables.tableNamesMap };
  }
  const session = new HostSession(target, dialect, rel);
  return new PgRemoteDatabase(dialect, session, rel);
}

function createContext(requestId, structures) {
  const cache = new Map();
  const handle = (key, target, schema) => {
    if (!cache.has(key)) cache.set(key, database(target, schema));
    return cache.get(key);
  };
  const ctx = {
    requestId,
    user() {
      const result = host().user(requestId);
      if (result.refused) throw new Refused(result.refused.refusal, result.refused.fix);
      const u = result.user;
      return {
        id: u.id,
        email: u.email,
        displayName: u.displayName,
        db: (name) => handle(`user:${name}`, { requestId, as: "user", db: name }, structures.shared[name] ?? {}),
        ownDb: () => handle("user:own", { requestId, as: "user", db: null }, structures.own),
      };
    },
    app() {
      return {
        ownDb: () => handle("app:own", { requestId, as: "app", db: null }, structures.own),
      };
    },
  };
  return Object.freeze(ctx);
}

/** Wraps an Application for the host. Uncaught refusals become a 403 that says which condition failed. */
export function serve(app, structures) {
  return {
    manifest: app.manifest,
    async fetch(request, platform) {
      const ctx = createContext(platform.requestId, structures);
      try {
        return await app.fetch(request, ctx);
      } catch (e) {
        if (e instanceof Refused) {
          return new Response(JSON.stringify({ error: "refused", refusal: e.refusal, fix: e.fix }, null, 2), {
            status: 403,
            headers: { "content-type": "application/json; charset=utf-8" },
          });
        }
        throw e;
      }
    },
  };
}
