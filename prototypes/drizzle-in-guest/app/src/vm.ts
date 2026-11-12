// A stand-in for the platform SDK's database part, as small as it can be.
//
// - `database(resource, schema)`: a real Drizzle database object (drizzle-orm's
//   own PgRemoteDatabase, the pg-proxy driver's class) whose every statement is
//   one host operation, `env.HOST.invoke("sql", ...)`.
// - Transactions: pg-proxy throws on `db.transaction(...)`. HostSession
//   replaces that one method: BEGIN/COMMIT/ROLLBACK are host operations, and
//   the statements in between carry the transaction id, so the host runs them
//   on the connection that holds the transaction.
// - `platform(resources).user().db(name)`: the shape of a typed wrapper like
//   `ctx.user().db("devops")`; read-only Resources come back without
//   insert/update/delete/execute/transaction at the type level.
import {
  createTableRelationsHelpers,
  extractTablesRelationalConfig,
  type ExtractTablesWithRelations,
} from "drizzle-orm";
import { PgDialect, type PgTransactionConfig } from "drizzle-orm/pg-core";
import { PgProxyTransaction, PgRemoteDatabase, PgRemoteSession, type RemoteCallback } from "drizzle-orm/pg-proxy";

type Invoke = (name: string, args: unknown) => Promise<any>;
declare const env: { HOST: { invoke: Invoke } };

// What JSON cannot carry, as Postgres text input (the host sends every
// parameter in text format and lets the server infer its type).
function encodeParam(v: unknown): unknown {
  if (typeof v === "bigint") return v.toString();
  if (v instanceof Date) return v.toISOString();
  if (v instanceof Uint8Array) {
    let hex = "\\x";
    for (const b of v) hex += b.toString(16).padStart(2, "0");
    return hex;
  }
  return v;
}

function callback(resource: string, tx?: number): RemoteCallback {
  return async (sql, params, method) => {
    const { rows } = await env.HOST.invoke("sql", {
      db: resource,
      sql,
      params: params.map(encodeParam),
      method,
      tx,
    });
    return { rows };
  };
}

type RelSchema = { fullSchema: Record<string, unknown>; schema: any; tableNamesMap: Record<string, string> };

class HostTransaction<TFull extends Record<string, unknown>, TSchema extends Record<string, any>> extends PgProxyTransaction<
  TFull,
  TSchema
> {
  override async transaction<T>(): Promise<T> {
    throw new Error("nested transactions (savepoints) are not supported in this spike");
  }
}

class HostSession<TFull extends Record<string, unknown>, TSchema extends Record<string, any>> extends PgRemoteSession<
  TFull,
  TSchema
> {
  constructor(
    readonly resource: string,
    readonly dialectRef: PgDialect,
    readonly relSchema: RelSchema | undefined,
    readonly txId?: number,
  ) {
    super(callback(resource, txId), dialectRef, relSchema as any);
  }

  override async transaction<T>(
    fn: (tx: PgProxyTransaction<TFull, TSchema>) => Promise<T>,
    config?: PgTransactionConfig,
  ): Promise<T> {
    if (this.txId !== undefined) throw new Error("already in a transaction");
    const { tx: id } = await env.HOST.invoke("tx.begin", { db: this.resource, config: config ?? null });
    const session = new HostSession<TFull, TSchema>(this.resource, this.dialectRef, this.relSchema, id);
    const tx = new HostTransaction<TFull, TSchema>(this.dialectRef, session, this.relSchema as any);
    let result: T;
    try {
      result = await fn(tx);
    } catch (e) {
      await env.HOST.invoke("tx.rollback", { tx: id });
      throw e;
    }
    await env.HOST.invoke("tx.commit", { tx: id });
    return result;
  }
}

export type Database<S extends Record<string, unknown>> = PgRemoteDatabase<S>;

export function database<S extends Record<string, unknown>>(resource: string, schema: S): Database<S> {
  const dialect = new PgDialect();
  const tables = extractTablesRelationalConfig(schema, createTableRelationsHelpers);
  const rel: RelSchema = { fullSchema: schema, schema: tables.tables, tableNamesMap: tables.tableNamesMap };
  const session = new HostSession<S, ExtractTablesWithRelations<S>>(resource, dialect, rel);
  return new PgRemoteDatabase<S>(dialect, session as any, rel as any);
}

// Read-only at the type level: only the reading half of the API.
type Reading = "select" | "selectDistinct" | "selectDistinctOn" | "$count" | "query" | "$with";
export type ReadOnlyDatabase<S extends Record<string, unknown>> = Pick<Database<S>, Reading> & {
  with: (
    ...queries: Parameters<Database<S>["with"]>
  ) => Pick<ReturnType<Database<S>["with"]>, "select" | "selectDistinct" | "selectDistinctOn">;
};

type Declared = Record<string, { schema: Record<string, unknown>; access: "read" | "write" }>;
type DbFor<R extends Declared, K extends keyof R> = R[K]["access"] extends "write"
  ? Database<R[K]["schema"]>
  : ReadOnlyDatabase<R[K]["schema"]>;

// What a platform-generated module could give the Application, from its
// Declared access: one typed database per Resource.
export function platform<const R extends Declared>(resources: R) {
  const cache = new Map<string, unknown>();
  return {
    user() {
      return {
        db<K extends keyof R & string>(name: K): DbFor<R, K> {
          if (!cache.has(name)) cache.set(name, database(name, resources[name].schema));
          return cache.get(name) as DbFor<R, K>;
        },
      };
    },
  };
}
