// Candidate A: the vision's sketch, literally.
//   export default { fetch(request, ctx) } satisfies App
//   ctx.user().db("devops")      as the User, on a shared database
//   ctx.app().ownDb()            as the Application, own database only
// Declared access reaches the types through a generated vm-env.d.ts that
// augments `DeclaredAccess` (like `wrangler types`). No User => runtime throw.

import type { ReadOps, WriteOps } from "../shared/query-stub.ts";
export { Refused, type Refusal, type AccessLevel } from "../shared/refusal.ts";

declare const brand: unique symbol; // not exported: no code outside this file can name it

/** Filled in by the generated vm-env.d.ts from vm.manifest.json. Empty means nothing is reachable. */
export interface DeclaredAccess {}

/** Named for the compiler message an agent sees when it uses an undeclared name. */
export type NameDeclaredInManifest = keyof DeclaredAccess & string;

export interface DatabaseDeclaredAtRead extends ReadOps {
  readonly [brand]: "database";
  readonly name: string;
}
export interface DatabaseDeclaredAtWrite extends DatabaseDeclaredAtRead, WriteOps {}
export interface OwnDatabase extends ReadOps, WriteOps {
  readonly [brand]: "own-database";
}

type DatabaseFor<N extends NameDeclaredInManifest> =
  DeclaredAccess[N] extends { access: "write" } ? DatabaseDeclaredAtWrite : DatabaseDeclaredAtRead;

export type UserId = string & { readonly [brand]: "UserId" };

/** Acting as the signed-in User. Identity itself stays in the host; these fields are for display and records. */
export interface ActingAsUser {
  readonly [brand]: "ActingAsUser";
  readonly id: UserId;
  readonly email: string;
  readonly displayName: string;
  db<N extends NameDeclaredInManifest>(name: N): DatabaseFor<N>;
  ownDb(): OwnDatabase;
}

/** Acting as the Application itself. Only its own database: there is deliberately no db(name). */
export interface ActingAsApplication {
  readonly [brand]: "ActingAsApplication";
  ownDb(): OwnDatabase;
}

export interface TrustedContext {
  readonly [brand]: "TrustedContext";
  readonly requestId: string;
  /** Throws Refused { reason: "no-user" } when the request has no signed-in User. */
  user(): ActingAsUser;
  /** The same, or null when no User is signed in. */
  userOrNull(): ActingAsUser | null;
  app(): ActingAsApplication;
}

export interface App {
  fetch(request: Request, ctx: TrustedContext): Response | Promise<Response>;
}
