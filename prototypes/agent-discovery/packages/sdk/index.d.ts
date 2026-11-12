// @vm/sdk: the only way an Application reaches company Resources.
//
//   manifest.ts:   export default defineManifest({ name, description, resources })
//   src/index.ts:  export default defineApp(manifest, { fetch(request, ctx) { ... } })
//
//   ctx.user().db("devops")   a shared database, acting as the signed-in User
//   ctx.user().ownDb()        the Application's own database, acting as the User
//   ctx.app().ownDb()         the Application's own database, acting as itself
//
// Database handles are Drizzle databases typed with that database's structure:
// shared ones from resources/<name>/ (written by `vm resources pull <name>`),
// the own one from src/db/schema.ts.

import type { PgRemoteDatabase } from "drizzle-orm/pg-proxy";
import type * as OwnSchema from "@vm/own-structure";

declare const brand: unique symbol;

// ---------------------------------------------------------------- refusals

/** How much of a Resource a Grant opens. Each level includes the one before. */
export type AccessLevel = "structure" | "read" | "write";

/** Which condition failed. */
export type Refusal =
  /** No request was in flight (module top level, or a timer after the response was sent). */
  | { reason: "no-request" }
  /** manifest.ts does not declare this Resource. */
  | { reason: "not-declared"; resource: string }
  /** manifest.ts declares the Resource at a lower Access level than the operation needs. */
  | { reason: "declared-lower"; resource: string; declared: AccessLevel; needed: AccessLevel }
  /** None of the User's Groups holds a Grant on the Resource at the needed Access level. */
  | { reason: "no-grant"; resource: string; user: string; needed: AccessLevel; held: AccessLevel | null }
  /** The Application tried to act as itself on a Resource other than its own database. */
  | { reason: "not-own-database"; resource: string }
  /** The Resource is declared but is not registered (removed, renamed, or its structure was never pulled). */
  | { reason: "not-registered"; resource: string };

/**
 * Thrown by every operation the platform refuses. If a handler lets it escape,
 * the platform answers 403 with `{ refusal, fix }`.
 */
export declare class Refused extends Error {
  private constructor();
  readonly name: "Refused";
  readonly refusal: Refusal;
  /** One sentence saying how to fix it. */
  readonly fix: string;
}

// ---------------------------------------------------------------- manifest

/** "structure" is a level for building only; an Application declares "read" or "write". */
export type DeclarableAccess = "read" | "write";

export interface PostgresDeclaration {
  readonly kind: "postgres";
  readonly access: DeclarableAccess;
}

export interface Manifest {
  /** Lowercase letters, digits and dashes. Unique in the company; it becomes the address https://<name>.apps.<domain>. */
  readonly name: string;
  /** One sentence for the people who will find this Application in the list. */
  readonly description: string;
  /** Every shared Resource the Application uses, by its registered name. Nothing else is reachable. */
  readonly resources: { readonly [name: string]: PostgresDeclaration };
}

/** Write the argument as a plain literal: the platform reads it without running your code. */
export declare function defineManifest<const M extends Manifest>(manifest: M): M;

// ---------------------------------------------------------------- databases

/** Filled in by the generated resources/<name>/index.ts of each pulled Resource. */
export interface SharedStructures {}

type Schema = Record<string, unknown>;
export type OwnStructure = typeof OwnSchema;

/** A Drizzle database. */
export type Database<S extends Schema> = PgRemoteDatabase<S>;

type DeclaredAtRead<N extends string> = `'${N}' is declared at 'read' in manifest.ts. Declare it at 'write' to change rows.`;
type Reading = "select" | "selectDistinct" | "selectDistinctOn" | "$count" | "query" | "$with";

/** A Drizzle database that only reads: the Resource is declared at "read". */
export type ReadOnlyDatabase<N extends string, S extends Schema> = Pick<Database<S>, Reading> & {
  with: (
    ...queries: Parameters<Database<S>["with"]>
  ) => Pick<ReturnType<Database<S>["with"]>, "select" | "selectDistinct" | "selectDistinctOn">;
  insert(this: DeclaredAtRead<N>, ...args: never[]): never;
  update(this: DeclaredAtRead<N>, ...args: never[]): never;
  delete(this: DeclaredAtRead<N>, ...args: never[]): never;
  execute(this: DeclaredAtRead<N>, ...args: never[]): never;
};

type NotPulled<N extends string> = `The structure of '${N}' has not been pulled. Run: npx vm resources pull ${N}`;

type Declared = Manifest["resources"];
type SharedDb<R extends Declared, N extends keyof R & string> = N extends keyof SharedStructures
  ? SharedStructures[N] extends infer S extends Schema
    ? R[N]["access"] extends "write"
      ? Database<S>
      : ReadOnlyDatabase<N, S>
    : never
  : NotPulled<N>;

// ---------------------------------------------------------------- context

/** Acting as the signed-in User: shared databases they hold a Grant on, and the own database. */
export interface ActingAsUser<R extends Declared, Names extends string> {
  readonly [brand]: "ActingAsUser";
  readonly id: string;
  readonly email: string;
  readonly displayName: string;
  /** A shared database declared in manifest.ts, reached with this User's Access. */
  db<N extends Names & keyof R & string>(name: N): SharedDb<R, N>;
  /** The Application's own database, on behalf of this User. */
  ownDb(): Database<OwnStructure>;
}

/** Acting as the Application itself: its own database only. */
export interface ActingAsApplication {
  readonly [brand]: "ActingAsApplication";
  ownDb(): Database<OwnStructure>;
}

/** Created by the platform for each request. Application code cannot construct one. */
export interface TrustedContext<M extends Manifest> {
  readonly [brand]: "TrustedContext";
  readonly requestId: string;
  /** The signed-in User. Every route requires sign-in, so there always is one. */
  user(): ActingAsUser<M["resources"], keyof M["resources"] & string>;
  /** The Application acting as itself. */
  app(): ActingAsApplication;
}

export interface Handlers<M extends Manifest> {
  fetch: (request: Request, ctx: TrustedContext<M>) => Response | Promise<Response>;
}

export declare function defineApp<const M extends Manifest>(
  manifest: M,
  handlers: Handlers<M>,
): Handlers<M> & { readonly manifest: M };
