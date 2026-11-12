// Candidate B: the manifest is a TypeScript value, and it types the context.
//   manifest.ts:  export default defineManifest({ signIn, resources })
//   index.ts:     export default defineApp(manifest, { fetch(request, ctx) })
//   ctx.user().db("devops")  /  ctx.app().ownDb()
// No codegen for Declared access. No User is a type-state driven by the same
// manifest field the host enforces: signIn "required" => user() is never null.

import type { ReadOps, WriteOps, Row } from "../shared/query-stub.ts";
export { Refused, type Refusal, type AccessLevel } from "../shared/refusal.ts";

declare const brand: unique symbol; // not exported

/** "structure" is a building-time level (pulling table structure); an Application cannot declare it. */
export type DeclarableAccess = "read" | "write";
export interface PostgresDeclaration { readonly kind: "postgres"; readonly access: DeclarableAccess }

export interface Manifest {
  /** "required": the host signs the User in before calling fetch. "optional": fetch may run with no User. */
  readonly signIn: "required" | "optional";
  readonly resources: { readonly [name: string]: PostgresDeclaration };
}

/** Identity at the type level; `const` keeps literal names and levels. The deploy reads the default export of manifest.ts. */
export declare function defineManifest<const M extends Manifest>(manifest: M): M;

type WriteNeeded<N extends string> = `'${N}' is declared at 'read' in manifest.ts. Declare it at 'write' to change rows.`;

export interface Database<N extends string, A extends DeclarableAccess> extends ReadOps {
  readonly [brand]: "database";
  readonly name: N;
  insert<T extends Row>(this: A extends "write" ? Database<N, A> : WriteNeeded<N>, table: string, row: T): Promise<{ rowCount: number }>;
  update<T extends Row>(this: A extends "write" ? Database<N, A> : WriteNeeded<N>, table: string, where: Partial<T>, set: Partial<T>): Promise<{ rowCount: number }>;
}
export interface OwnDatabase extends ReadOps, WriteOps { readonly [brand]: "own-database" }

export type UserId = string & { readonly [brand]: "UserId" };
type Resources = Manifest["resources"];
type AccessOf<R extends Resources, N extends keyof R> = R[N] extends { access: infer A extends DeclarableAccess } ? A : never;

/** `Names` is passed in already resolved so that compiler messages list the declared names. */
export interface ActingAsUser<R extends Resources, Names extends keyof R & string> {
  readonly [brand]: "ActingAsUser";
  readonly id: UserId;
  readonly email: string;
  readonly displayName: string;
  db<N extends Names>(name: N): Database<N, AccessOf<R, N>>;
  ownDb(): OwnDatabase;
}
/** Acting as the Application itself: own database only, so there is no db(name). */
export interface ActingAsApplication { readonly [brand]: "ActingAsApplication"; ownDb(): OwnDatabase }

type UserOf<M extends Manifest> = ActingAsUser<M["resources"], keyof M["resources"] & string>;

export interface TrustedContext<M extends Manifest> {
  readonly [brand]: "TrustedContext";
  readonly requestId: string;
  user(): M["signIn"] extends "required" ? UserOf<M> : UserOf<M> | null;
  /** For signIn "optional": the User, or throws Refused { reason: "no-user" }, which the host turns into a sign-in redirect. */
  requireUser(): UserOf<M>;
  app(): ActingAsApplication;
}

export interface Handlers<M extends Manifest> {
  // Property syntax, not method syntax: parameters are then checked contravariantly,
  // so a handler written for a context with more Resources is refused.
  fetch: (request: Request, ctx: TrustedContext<M>) => Response | Promise<Response>;
}
export declare function defineApp<const M extends Manifest>(manifest: M, handlers: Handlers<M>): Handlers<M> & { readonly manifest: M };
