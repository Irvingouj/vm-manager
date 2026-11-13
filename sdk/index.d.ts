// @vm/sdk: what an Application is written with (blackbox/CONTRACT.md, "The SDK").
//
//   manifest.ts:   export default defineManifest({ name, description, resources })
//   src/index.ts:  export default defineApp(manifest, { fetch(request, ctx) { ... } })
//
//   ctx.user().db("devops")   a shared database declared in manifest.ts, as the User
//   ctx.user().ownDb()        the Application's own database, as the User
//   ctx.app().ownDb()         the Application's own database, as the Application
//
// A handle is a Drizzle database typed with that database's structure: a
// shared one from resources/<name>/ (linked by its generated index.ts), the
// own one from src/db/schema.ts (tsconfig "paths": "@vm/own-structure").
// The types guide; the platform decides. Every operation is decided by the
// host from the request in flight, and the database role is the boundary.

import type { PgTransactionConfig } from "drizzle-orm/pg-core";
import type { PgRemoteDatabase } from "drizzle-orm/pg-proxy";
import type * as OwnStructureModule from "@vm/own-structure";

declare const contextBrand: unique symbol;

// ---------------------------------------------------------------- manifest

/** An Access level a Manifest declares. (`structure` is for building only.) */
export type DeclaredAccess = "read" | "write";

/** A shared database the Application uses. */
export interface ResourceDeclaration {
  readonly kind: "postgres";
  readonly access: DeclaredAccess;
}

export interface Manifest {
  /** The Application's name: a lower-case DNS label, as the platform's API takes it. */
  readonly name: string;
  /** One sentence saying what the Application does, and for whom. */
  readonly description: string;
  /** Every shared database the Application uses, by its Resource name. */
  readonly resources: { readonly [name: string]: ResourceDeclaration };
}

/** Write the argument as a plain literal: the platform reads manifest.ts without running it. */
export declare function defineManifest<const M extends Manifest>(manifest: M): M;

// ---------------------------------------------------------------- refusals and failures

/** How much of a Resource a Grant opens; each level includes the one before. */
export type AccessLevel = "structure" | "read" | "write";

/** Which condition refused the operation, and what else the host said about it. */
export type Refusal =
  | { readonly reason: "no-request" }
  | { readonly reason: "unknown-operation" }
  | { readonly reason: "not-own-database"; readonly resource: string }
  | { readonly reason: "not-declared"; readonly resource: string }
  | { readonly reason: "not-registered"; readonly resource: string }
  | {
      readonly reason: "declared-lower";
      readonly resource: string;
      readonly declared: AccessLevel;
      readonly needed: AccessLevel;
    }
  | {
      readonly reason: "no-grant";
      readonly resource: string;
      readonly user: string;
      readonly needed: AccessLevel;
      readonly held: AccessLevel | null;
      readonly contact: string;
    }
  | { readonly reason: "log-unavailable"; readonly resource?: string };

/**
 * An operation the platform refused. Escaping `fetch`, it becomes the
 * response 403 with `{ refusal, fix }`. Only the platform makes one.
 */
export declare class Refused extends Error {
  private constructor();
  readonly name: "Refused";
  /** Which condition failed, as the host gave it. */
  readonly refusal: Refusal;
  /** One sentence saying what to do. */
  readonly fix: string;
}

/** The host's error for an operation that failed without being refused. */
export interface OperationError {
  /** `database`, `statement`, `limit`, `unavailable`, `malformed`, ... */
  readonly kind: string;
  readonly [detail: string]: unknown;
}

/**
 * An operation that failed without being refused: Postgres refused or
 * failed the statement, the statement check refused it, a limit ended it,
 * or the database could not be reached. Escaping `fetch`, it is the
 * platform's fixed 500. Only the platform makes one.
 */
export declare class OperationFailed extends Error {
  private constructor();
  readonly name: "OperationFailed";
  /** The host's `error.kind`. */
  readonly kind: string;
  /** The SQLSTATE, when `kind` is `database`. */
  readonly code: string | undefined;
  /** The host's whole error object. */
  readonly detail: OperationError;
}

// ---------------------------------------------------------------- databases

/** Filled in by each pulled Resource's generated resources/<name>/index.ts. */
export interface SharedStructures {}

/** The own database's structure: src/db/schema.ts. */
export type OwnStructure = typeof OwnStructureModule;

type Structure = Record<string, unknown>;

/** What no handle offers: raw SQL. `transaction` is the SDK's own, below. */
type NotOffered = "execute" | "transaction";

/** Drizzle's `tx` on a database of structure `S`. */
type DrizzleTransaction<S extends Structure> = Parameters<Parameters<PgRemoteDatabase<S>["transaction"]>[0]>[0];

/**
 * The second argument of `transaction`, Drizzle's: `isolationLevel`
 * (`read committed`, `repeatable read`, `serializable`) and `accessMode`
 * (`read write`, `read only`).
 */
export type TransactionConfig = Pick<PgTransactionConfig, "isolationLevel" | "accessMode">;

/**
 * The `tx` a transaction's callback is given: the same database, inside the
 * transaction. Transactions cannot be nested, so it has no `transaction`;
 * `tx.rollback()` rolls back and makes `transaction` reject with Drizzle's
 * `TransactionRollbackError`. Its isolation level and access mode are the
 * second argument of `transaction`: Drizzle's `setTransaction` sends a `SET`
 * statement, which the platform refuses, so it is not offered either.
 */
export type Transaction<S extends Structure> = Omit<DrizzleTransaction<S>, NotOffered | "setTransaction">;

/**
 * A Drizzle database on a database the Application may change.
 * `transaction(callback, config?)` commits when the callback returns (and
 * resolves to what it returned), and rolls everything back when it throws
 * (and rejects with that same error).
 */
export type Database<S extends Structure> = Omit<PgRemoteDatabase<S>, NotOffered> & {
  transaction<T>(callback: (tx: Transaction<S>) => Promise<T>, config?: TransactionConfig): Promise<T>;
};

type Reading = "select" | "selectDistinct" | "selectDistinctOn" | "$count" | "query" | "$with";

type DeclaredAtRead<N extends string> =
  `'${N}' is declared at 'read' in manifest.ts. Declare it at 'write' to change rows.`;

/** What a database declared at `read` offers besides its reads: no change of rows. */
type OnlyReads<N extends string, S extends Structure> = {
  with(
    ...queries: Parameters<PgRemoteDatabase<S>["with"]>
  ): Pick<ReturnType<PgRemoteDatabase<S>["with"]>, "select" | "selectDistinct" | "selectDistinctOn">;
  insert(this: DeclaredAtRead<N>, ...args: unknown[]): never;
  update(this: DeclaredAtRead<N>, ...args: unknown[]): never;
  delete(this: DeclaredAtRead<N>, ...args: unknown[]): never;
};

/** The `tx` of a transaction through a handle declared at `read`: reads only, always read-only. */
export type ReadOnlyTransaction<N extends string, S extends Structure> = Pick<
  DrizzleTransaction<S>,
  Reading | "rollback"
> &
  OnlyReads<N, S>;

/**
 * A Drizzle database that only reads: the Resource is declared at `read`.
 * Its transactions are read-only whatever `accessMode` says.
 */
export type ReadOnlyDatabase<N extends string, S extends Structure> = Pick<PgRemoteDatabase<S>, Reading> &
  OnlyReads<N, S> & {
    transaction<T>(
      callback: (tx: ReadOnlyTransaction<N, S>) => Promise<T>,
      config?: TransactionConfig,
    ): Promise<T>;
  };

/** What `db(name)` is when the Resource has no generated link. */
type NotPulled<N extends string> =
  `The structure of '${N}' has not been pulled. Add resources/${N}/schema.ts and its generated index.ts.`;

type Declarations = Manifest["resources"];

type SharedDatabase<R extends Declarations, N extends keyof R & string> = N extends keyof SharedStructures
  ? SharedStructures[N] extends infer S extends Structure
    ? R[N]["access"] extends "write"
      ? Database<S>
      : ReadOnlyDatabase<N, S>
    : never
  : NotPulled<N>;

// ---------------------------------------------------------------- context

/** Acting as the User of the request: the shared databases declared in manifest.ts, and the own database. */
export interface ActingAsUser<R extends Declarations> {
  readonly id: string;
  readonly groups: readonly string[];
  /** A shared database declared in manifest.ts, with this User's Access. */
  db<N extends keyof R & string>(name: N): SharedDatabase<R, N>;
  /** The Application's own database, as this User. */
  ownDb(): Database<OwnStructure>;
}

/** Acting as the Application itself: its own database only. */
export interface ActingAsApplication {
  /** The Application's own database, as the Application. */
  ownDb(): Database<OwnStructure>;
}

/** Made by the platform for each request; Application code cannot make one. */
export interface TrustedContext<M extends Manifest> {
  readonly [contextBrand]: "TrustedContext";
  /** The User of the request in flight. */
  user(): ActingAsUser<M["resources"]>;
  /** The Application acting as itself. */
  app(): ActingAsApplication;
}

export interface Handlers<M extends Manifest> {
  fetch(request: Request, ctx: TrustedContext<M>): Response | Promise<Response>;
}

/** The Application, as src/index.ts exports it by default. */
export interface App<M extends Manifest> {
  readonly manifest: M;
  fetch(request: Request, ctx: TrustedContext<M>): Response | Promise<Response>;
}

export declare function defineApp<const M extends Manifest>(manifest: M, handlers: Handlers<M>): App<M>;
