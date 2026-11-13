// The SDK's runtime: turns the host's raw interface (blackbox/CONTRACT.md,
// "What an Application is handed" and "The raw host interface") into the
// Trusted context of "The SDK: What Application code is given". The bundle's
// entry (tooling/bundle.js) calls `serve` with the Application and its
// structures and registers the result as globalThis.default.
//
// The context holds no identity: a handle only names its target (a Resource
// or the own database) and its actor (the User or the Application). Who is
// asking is decided by the host from the request in flight, on every
// operation.

import { createTableRelationsHelpers, extractTablesRelationalConfig, TransactionRollbackError } from "drizzle-orm";
import { PgDialect } from "drizzle-orm/pg-core";
import { PgProxyTransaction, PgRemoteDatabase, PgRemoteSession } from "drizzle-orm/pg-proxy";
import { failedOf, OperationFailed, Refused, refusedOf } from "./errors.js";
import { decodeRows, encodeParam, operationOf, rowsAsObjects } from "./values.js";

const freeze = Object.freeze;
const hasOwn = (object, key) => object !== null && typeof object === "object" && Object.hasOwn(object, key);

/** `env.VM.invoke`, read when an operation is made. */
function invoke(operation, args) {
  const vm = globalThis.env?.VM;
  if (vm === undefined || typeof vm.invoke !== "function") {
    throw new Error("the platform's host interface (env.VM) is not here: an Application runs on the platform");
  }
  return vm.invoke(operation, args);
}

/** What an operation's answer gives, or the error it rejects with. */
function settle(answer) {
  if (hasOwn(answer, "ok")) return answer.ok;
  if (hasOwn(answer, "refused")) throw refusedOf(answer.refused);
  if (hasOwn(answer, "error")) throw failedOf(answer.error);
  throw new Error("the platform answered the operation with something the SDK cannot read");
}

/**
 * Drizzle's proxy callback for one target: each statement is one host
 * operation, `query` for a select and `execute` otherwise, its values asked
 * for as text and read as Drizzle's columns expect them.
 */
function callback(target) {
  return async (sql, params, method) => {
    const answer = await invoke(operationOf(sql), {
      ...target,
      sql,
      params: params.map(encodeParam),
      values: "text",
    });
    const result = settle(answer);
    const rows = decodeRows(result.types, Array.isArray(result.rows) ? result.rows : []);
    return { rows: method === "all" ? rows : rowsAsObjects(result.columns ?? [], rows) };
  };
}

/**
 * Drizzle wraps what the callback throws in its own error (whose message
 * also holds the parameters). A refusal or a failure reaches Application
 * code as itself.
 */
function ownError(error) {
  for (let e = error, depth = 0; e !== null && typeof e === "object" && depth < 8; e = e.cause, depth += 1) {
    if (e instanceof Refused || e instanceof OperationFailed) return e;
  }
  return error;
}

/** What `tx.transaction(...)` rejects with: the host has no nesting. */
const NESTED =
  "Transactions cannot be nested: run these statements in the transaction you are in, through its `tx`";

/**
 * The `tx` a transaction's callback is given: a Drizzle database whose
 * statements carry the transaction's id. It refuses nesting without asking
 * the host.
 */
class HostTransaction extends PgProxyTransaction {
  async transaction() {
    throw new Error(NESTED);
  }

  /**
   * Drizzle's: throws its `TransactionRollbackError`, which makes
   * `transaction` roll back and reject with it. Drizzle 0.45 names that
   * error `DrizzleError`; here it is named for what it is.
   */
  rollback() {
    const error = new TransactionRollbackError();
    error.name = "TransactionRollbackError";
    throw error;
  }
}

class HostSession extends PgRemoteSession {
  /**
   * `target` is the handle's (`{ resource, as }` or `{ own: true, as }`);
   * `declared` its Access level as manifest.ts declares it (`write` on the
   * own database); `tx` the transaction's id, for a session inside one.
   */
  constructor(target, declared, tx, dialect, relational) {
    super(callback(tx === undefined ? target : freeze({ tx })), dialect, relational);
    this.hostTarget = target;
    this.hostDeclared = declared;
    this.hostTx = tx;
    this.hostDialect = dialect;
    this.hostRelational = relational;
  }

  prepareQuery(...args) {
    const prepared = super.prepareQuery(...args);
    for (const name of ["execute", "all", "values"]) {
      const run = prepared[name];
      if (typeof run !== "function") continue;
      prepared[name] = async function (...given) {
        try {
          return await run.apply(this, given);
        } catch (error) {
          throw ownError(error);
        }
      };
    }
    return prepared;
  }

  /**
   * Drizzle's `db.transaction(callback, config)` (blackbox/CONTRACT.md,
   * "Transactions: Through the SDK"): `tx.begin` on the handle's target,
   * at `read` when the handle is declared at read or `accessMode` is
   * `read only`, else at `write`; the callback with a `tx` bound to the
   * transaction; then `tx.commit`, or `tx.rollback` when the callback threw
   * or rejected, and then that same error again. The host rolls back on its
   * own whatever a request leaves open.
   */
  async transaction(run, config) {
    if (this.hostTx !== undefined) throw new Error(NESTED);
    const readOnly = this.hostDeclared === "read" || config?.accessMode === "read only";
    const args = { ...this.hostTarget, access: readOnly ? "read" : "write" };
    if (config?.isolationLevel !== undefined) args.isolation = config.isolationLevel;
    const { tx: id } = settle(await invoke("tx.begin", args));
    const session = new HostSession(this.hostTarget, this.hostDeclared, id, this.hostDialect, this.hostRelational);
    const tx = new HostTransaction(this.hostDialect, session, this.hostRelational);
    withoutRawSql(tx);
    let result;
    try {
      result = await run(tx);
    } catch (error) {
      try {
        // Whatever the host says, the callback's error is what rejects.
        await invoke("tx.rollback", { tx: id });
      } catch {
        // The host rolls back what the request leaves open.
      }
      throw error;
    }
    settle(await invoke("tx.commit", { tx: id }));
    return result;
  }
}

/** The handles offer no raw SQL (blackbox/CONTRACT.md, "What Application
 * code is given"); the database role is the boundary, this is guidance. */
function withoutRawSql(db) {
  Object.defineProperty(db, "execute", { value: undefined });
}

/**
 * A Drizzle database on `target`, typed (at compile time) and related by
 * `schema`; `declared` is its Access level (`read` or `write`).
 */
function database(target, declared, schema) {
  const dialect = new PgDialect();
  let relational;
  if (schema !== undefined && Object.keys(schema).length > 0) {
    const tables = extractTablesRelationalConfig(schema, createTableRelationsHelpers);
    relational = { fullSchema: schema, schema: tables.tables, tableNamesMap: tables.tableNamesMap };
  }
  const session = new HostSession(freeze(target), declared, undefined, dialect, relational);
  const db = new PgRemoteDatabase(dialect, session, relational);
  withoutRawSql(db);
  return db;
}

/** The Trusted context of one request, over the platform's `raw` one. */
function context(raw, structures, declared) {
  const own = structures.own;
  const shared = structures.shared;
  const handles = new Map();
  const handle = (key, target, schema, access) => {
    let db = handles.get(key);
    if (db === undefined) {
      db = database(target, access, schema);
      handles.set(key, db);
    }
    return db;
  };
  const structureOf = (name) => (hasOwn(shared, name) ? shared[name] : undefined);
  // A Resource not declared is `write` here: the host refuses it anyway.
  const accessOf = (name) => (hasOwn(declared, name) && declared[name]?.access === "read" ? "read" : "write");
  return freeze({
    user() {
      const who = raw.user();
      return freeze({
        id: who.id,
        groups: who.groups,
        db: (name) =>
          handle(`user resource ${name}`, { resource: name, as: "user" }, structureOf(name), accessOf(name)),
        ownDb: () => handle("user own", { own: true, as: "user" }, own, "write"),
      });
    },
    app() {
      return freeze({
        ownDb: () => handle("application own", { own: true, as: "application" }, own, "write"),
        // Not in the types: code forced past them gets the host's
        // `not-own-database`, not a TypeError.
        db: (name) =>
          handle(
            `application resource ${name}`,
            { resource: name, as: "application" },
            structureOf(name),
            accessOf(name),
          ),
      });
    },
  });
}

/** The response of a Refused that escaped `fetch`. */
function refusedResponse(error) {
  return new Response(JSON.stringify({ refusal: error.refusal, fix: error.fix }), {
    status: 403,
    headers: { "content-type": "application/json" },
  });
}

/**
 * The handler the platform runs: `app` is what `defineApp` gave (anything
 * with `fetch(request, ctx)`), `structures` the own database's schema and
 * each shared database's schema with its relations, by Resource name.
 */
export function serve(app, structures) {
  const all = freeze({ own: structures?.own ?? {}, shared: structures?.shared ?? {} });
  // What manifest.ts declares, read once: a transaction through a handle
  // declared at `read` is begun read-only.
  const resources = app?.manifest?.resources;
  const declared = freeze(resources !== null && typeof resources === "object" ? { ...resources } : {});
  return freeze({
    async fetch(request, raw) {
      if (app === null || typeof app !== "object" || typeof app.fetch !== "function") {
        throw new Error("src/index.ts must export default defineApp(manifest, { fetch(request, ctx) { ... } })");
      }
      try {
        return await app.fetch(request, context(raw, all, declared));
      } catch (error) {
        if (error instanceof Refused) return refusedResponse(error);
        throw error;
      }
    },
  });
}
