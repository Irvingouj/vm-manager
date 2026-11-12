// Candidate C: the principal is an argument to every operation (the vision's
// "resource APIs require it as a parameter").
//   import { db, ownDb } from "@vm/sdk"
//   db("devops").select(ctx.user, "incidents")      ctx.user: User | null
//   ownDb().insert(ctx.app, "acks", row)            ctx.app: Application
// Handles are plain module-level values; who acts is chosen per call.
// Declared access comes from the generated vm-env.d.ts, as in candidate A.

import type { Row, RawSql } from "../shared/query-stub.ts";
export { Refused, type Refusal, type AccessLevel } from "../shared/refusal.ts";

declare const brand: unique symbol; // not exported

export interface DeclaredAccess {} // filled in by vm-env.d.ts
export type NameDeclaredInManifest = keyof DeclaredAccess & string;

export type UserId = string & { readonly [brand]: "UserId" };

/** The signed-in User, as a principal to pass to operations. A handle, not a credential. */
export interface User {
  readonly [brand]: "principal";
  readonly kind: "user";
  readonly id: UserId;
  readonly email: string;
  readonly displayName: string;
}
/** The Application itself. Accepted only by its own database. */
export interface Application {
  readonly [brand]: "principal";
  readonly kind: "application";
}

export interface SharedDatabaseDeclaredAtRead {
  readonly name: string;
  select<T extends Row = Row>(as: User, table: string, where?: Partial<T>): Promise<T[]>;
  query<T extends Row = Row>(as: User, sql: RawSql): Promise<T[]>;
}
export interface SharedDatabaseDeclaredAtWrite extends SharedDatabaseDeclaredAtRead {
  insert<T extends Row>(as: User, table: string, row: T): Promise<{ rowCount: number }>;
}
export interface OwnDatabase {
  select<T extends Row = Row>(as: User | Application, table: string, where?: Partial<T>): Promise<T[]>;
  insert<T extends Row>(as: User | Application, table: string, row: T): Promise<{ rowCount: number }>;
}

type DatabaseFor<N extends NameDeclaredInManifest> =
  DeclaredAccess[N] extends { access: "write" } ? SharedDatabaseDeclaredAtWrite : SharedDatabaseDeclaredAtRead;

/** A shared database by logical name. Getting the handle does nothing; each operation names its principal. */
export declare function db<N extends NameDeclaredInManifest>(name: N): DatabaseFor<N>;
export declare function ownDb(): OwnDatabase;

export interface TrustedContext {
  readonly [brand]: "TrustedContext";
  readonly requestId: string;
  /** null when the request has no signed-in User. */
  readonly user: User | null;
  readonly app: Application;
}

export interface App {
  fetch(request: Request, ctx: TrustedContext): Response | Promise<Response>;
}
