# Trusted context and SDK type shape: three candidates

Date: 2026-11-12 (UTC). Code: branch `spike/trusted-context-types`, under `prototypes/trusted-context/`. Input to the map ticket "Trusted context and SDK type shape". **No shape has been chosen: the choice is waiting for the product owner's reaction.**

Provenance: this is the report of the engineering agent that did the work, saved as returned. The driving session independently re-ran, on a fresh checkout of the branch:

- `npm run check`: `tsc` passes on all three candidates (log `/tmp/tc-verify-check.log`);
- `npm run errors`: the compiler messages quoted below for an undeclared name (A, B, C), writing through `read` (B), "no User" (B) and acting as itself on a shared Resource (C) appear as quoted (log `/tmp/tc-verify-errors.log`);
- `npm run run-b`: the fake host run ends with the refusals `no-grant`, `declared-lower` and `no-request` in its Operation log, and records alice as the User on the Application's own write (log `/tmp/tc-verify-run.log`).

Everything else is the agent's account. In the text below, [tsc] = the agent ran TypeScript 7.0.2; [docs] = read in this repo's docs; [run] = the agent ran the fake host; [judgment] = the agent's opinion.

## Recommendation

**The agent recommends candidate B, "defineApp with a TypeScript manifest".** One reason: it is the only shape where the manifest field the host enforces (`signIn`, and the Declared access) is the same value that types the context. So "may there be no User here?", "is this name declared?" and "is it declared at write?" are all compile errors, and each message names the Resource or lists the allowed names.

Take B with one refinement (see open question 7). Two caveats:

- Its messages get verbose when they have to print the manifest type.
- Its "no User" error is the bare `Object is possibly 'null'.`

Candidate C stops just as much at compile time, but its calls are noisier and its "acted as itself on a shared Resource" message points the wrong way. Candidate A is the vision's sketch as written and stops nothing about "no User" at compile time.

## What holds for all three

- **The context is only a handle.** None of the shapes can stop `{} as TrustedContext` [tsc: compiles in all three test files]. That costs nothing: the identity research found that identity lives only in the host, and every operation is charged to the request in flight when it is made [docs: `identity-pass-through.md`].
- **The fake host behaves the same way.** In B's fake host, the context holds no identity and asks the host at the moment of each call. A context kept after its request and used later was refused with `no-request` [run].
- **Writes through raw SQL get past every shape.** `db("devops").query({text:"delete ..."})` on a database declared at `read` compiles in all three [tsc]. The fake host refused it with `declared-lower` [run]. The real host must run read-declared handles on a read-only Postgres role [judgment].
- **Refusals share one error type.** `shared/refusal.ts` defines `class Refused extends Error { private constructor(); refusal: Refusal; fix: string }`. `Refusal` is a union tagged by `reason`: `no-user`, `no-request`, `not-declared`, `declared-lower` (declared, but at a lower level than needed), `no-grant` (says which level the User holds and which is needed), `not-own-database`, `not-registered`.
  - A refusal the handler doesn't catch becomes a 403 with `{refusal, fix}`, except `no-user`, which becomes a redirect to sign-in [run].
  - `new Refused()` does not compile [tsc].
  - `declared-lower` and `not-registered` were added beyond the conditions the brief listed [judgment].
- **Other compile-time checks in all three:**
  - The brand is a `unique symbol` that the SDK does not export, so an object literal cannot pass as a context [tsc].
  - `structure` is not a level an Application can declare. In B this is a compile error; in A and C it would have to be a check in the generator [judgment].

## Candidate A: the vision's sketch, literally (`a-vision-literal/`)

The handler is `export default { fetch(request, ctx) } satisfies App`. Declared access reaches the types through a generated `vm-env.d.ts` that augments the SDK's `DeclaredAccess` interface from `vm.manifest.json` (`gen-env.mjs` stands in for a `vm types` command). With no User, the code still compiles and the host throws at run time.

```ts
declare const brand: unique symbol;                 // not exported
export interface DeclaredAccess {}                  // augmented by generated vm-env.d.ts
export type NameDeclaredInManifest = keyof DeclaredAccess & string;  // named for the error message
export interface DatabaseDeclaredAtRead extends ReadOps { readonly [brand]: "database"; readonly name: string }
export interface DatabaseDeclaredAtWrite extends DatabaseDeclaredAtRead, WriteOps {}
export interface OwnDatabase extends ReadOps, WriteOps { readonly [brand]: "own-database" }
type DatabaseFor<N extends NameDeclaredInManifest> =
  DeclaredAccess[N] extends { access: "write" } ? DatabaseDeclaredAtWrite : DatabaseDeclaredAtRead;
export interface ActingAsUser {
  readonly [brand]: "ActingAsUser"; readonly id: UserId; readonly email: string; readonly displayName: string;
  db<N extends NameDeclaredInManifest>(name: N): DatabaseFor<N>;
  ownDb(): OwnDatabase;
}
export interface ActingAsApplication { readonly [brand]: "ActingAsApplication"; ownDb(): OwnDatabase } // no db(name)
export interface TrustedContext {
  readonly [brand]: "TrustedContext"; readonly requestId: string;
  user(): ActingAsUser;                 // throws Refused { reason: "no-user" }
  userOrNull(): ActingAsUser | null;
  app(): ActingAsApplication;
}
export interface App { fetch(request: Request, ctx: TrustedContext): Response | Promise<Response> }
```

Handler (a):

```ts
import type { TrustedContext } from "@vm/sdk";
type Incident = { id: number; title: string; severity: string; status: string };
export async function listIncidents(_request: Request, ctx: TrustedContext): Promise<Response> {
  const incidents = await ctx.user().db("devops").select<Incident>("incidents", { status: "open" });
  return Response.json(incidents);
}
```

- **Stopped at compile time** [tsc]: building a context, an undeclared name, writing through a `read` declaration, the Application acting as itself on a shared Resource.
- **Stopped only at run time by the host:** no User, raw-SQL writes, a context used outside its request.
- **Not stopped at all:** a stale `vm-env.d.ts`, if someone edits the manifest and forgets to regenerate. The deploy would have to regenerate it [judgment].
- **Ease for a Coding agent:** easiest of the three. It matches the vision and Cloudflare habits. But "no User" fails silently until it runs [judgment].
- **Most common mistake, an undeclared name** [tsc]: `Argument of type '"payroll"' is not assignable to parameter of type 'NameDeclaredInManifest'.` It does not list the valid names. A mapped-type trick to make it list them was tried, and TypeScript still printed the alias [tsc].
- **Writing through `read`** [tsc]: `Property 'insert' does not exist on type 'DatabaseDeclaredAtRead'.`

## Candidate B: the manifest types the context (`b-define-app/`), recommended by the agent

`manifest.ts` exports `defineManifest({ signIn, resources })` with no code generation, and `index.ts` exports `defineApp(manifest, { fetch })`. With `signIn: "required"`, `ctx.user()` is never null, because the host signs the User in before it calls `fetch`. With `"optional"`, it is `| null`, and `requireUser()` throws `no-user`, which the host turns into a sign-in redirect.

```ts
export type DeclarableAccess = "read" | "write";    // "structure" is not declarable
export interface Manifest {
  readonly signIn: "required" | "optional";
  readonly resources: { readonly [name: string]: { readonly kind: "postgres"; readonly access: DeclarableAccess } };
}
export declare function defineManifest<const M extends Manifest>(manifest: M): M;
type WriteNeeded<N extends string> = `'${N}' is declared at 'read' in manifest.ts. Declare it at 'write' to change rows.`;
export interface Database<N extends string, A extends DeclarableAccess> extends ReadOps {
  readonly [brand]: "database"; readonly name: N;
  insert<T extends Row>(this: A extends "write" ? Database<N, A> : WriteNeeded<N>, table: string, row: T): Promise<{ rowCount: number }>;
}
type AccessOf<R, N extends keyof R> = R[N] extends { access: infer A extends DeclarableAccess } ? A : never;
export interface ActingAsUser<R extends Resources, Names extends keyof R & string> {  // Names pre-resolved => readable errors
  readonly [brand]: "ActingAsUser"; readonly id: UserId; readonly email: string; readonly displayName: string;
  db<N extends Names>(name: N): Database<N, AccessOf<R, N>>;
  ownDb(): OwnDatabase;
}
export interface ActingAsApplication { readonly [brand]: "ActingAsApplication"; ownDb(): OwnDatabase }
type UserOf<M extends Manifest> = ActingAsUser<M["resources"], keyof M["resources"] & string>;
export interface TrustedContext<M extends Manifest> {
  readonly [brand]: "TrustedContext"; readonly requestId: string;
  user(): M["signIn"] extends "required" ? UserOf<M> : UserOf<M> | null;
  requireUser(): UserOf<M>;
  app(): ActingAsApplication;
}
export interface Handlers<M extends Manifest> {
  fetch: (request: Request, ctx: TrustedContext<M>) => Response | Promise<Response>; // property syntax: contravariant
}
export declare function defineApp<const M extends Manifest>(manifest: M, handlers: Handlers<M>): Handlers<M> & { readonly manifest: M };
// manifest.ts also exports:  export type Ctx = TrustedContext<typeof manifest>;
```

Handler (a):

```ts
import type { Ctx } from "../manifest.ts";
type Incident = { id: number; title: string; severity: string; status: string };
export async function listIncidents(_request: Request, ctx: Ctx): Promise<Response> {
  // signIn is "optional" in manifest.ts, so ctx.user() may be null; requireUser() sends a guest to sign-in.
  const incidents = await ctx.requireUser().db("devops").select<Incident>("incidents", { status: "open" });
  return Response.json(incidents);
}
```

- **Stopped at compile time** [tsc]:
  - everything A stops;
  - using the User when `signIn` is `"optional"`;
  - declaring `structure`;
  - a computed access level (a level held in a variable widens to `string`, which the manifest type refuses);
  - a handler typed against a different or stale manifest. That test failed to stop the mistake while `fetch` was written as a method, because of TypeScript's method bivariance. It works with property syntax [tsc].
- **Stopped only at run time by the host:** `ctx.user()!` (the non-null assertion opts out of the check), raw-SQL writes, a context used outside its request, and no Grant [run].
- **Not stopped at all:** a manifest that a Coding agent builds with spreads or imports. It still type-checks, so the deploy must evaluate `manifest.ts` on its own and require a plain literal [judgment].
- **Ease for a Coding agent:** good. There is one extra concept, `import type { Ctx } from "../manifest.ts"`, and the SDK's generics are harder to read [judgment].
- **Messages** [tsc]:
  - Undeclared name: `Argument of type '"payroll"' is not assignable to parameter of type '"devops" | "facilities"'.`
  - Writing through `read`: `The 'this' context of type 'Database<"devops", "read">' is not assignable to method's 'this' of type '"'devops' is declared at 'read' in manifest.ts. Declare it at 'write' to change rows."'.`
  - Weak spot, "no User": `Object is possibly 'null'.`
  - Weak spot, forging a context or a stale handler: the message prints the whole manifest type.

**Fake host run** [run]: B's real handler files run under Node 22, which strips the types itself, against a fake host (`npm run run-b`).

- alice → 200. bob → 403 `no-grant` with a `fix` sentence, or the handler's own 403 in sample (c). No User → 302.
- POST `/ack` as the Application → 201, and the fake Change history records alice as the User.
- A raw delete through `read` → `declared-lower`. The kept context → `no-request`.

## Candidate C: the principal is an argument (`c-principal-argument/`)

This follows the vision's wording, "resource APIs require it as a parameter". You call `db("devops").select(ctx.user, ...)` and `ownDb().insert(ctx.app, ...)`. `ctx.user` is `User | null`. Database handles are plain values at module level. Declared access comes from the generated file, as in A.

```ts
export interface User { readonly [brand]: "principal"; readonly kind: "user"; readonly id: UserId; readonly email: string; readonly displayName: string }
export interface Application { readonly [brand]: "principal"; readonly kind: "application" }
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
export declare function db<N extends NameDeclaredInManifest>(name: N): DatabaseFor<N>;
export declare function ownDb(): OwnDatabase;
export interface TrustedContext {
  readonly [brand]: "TrustedContext"; readonly requestId: string;
  readonly user: User | null; readonly app: Application;
}
```

Handler (a):

```ts
import { db, type TrustedContext } from "@vm/sdk";
type Incident = { id: number; title: string; severity: string; status: string };
export async function listIncidents(_request: Request, ctx: TrustedContext): Promise<Response> {
  if (!ctx.user) return new Response("Sign in to see incidents", { status: 401 });
  const incidents = await db("devops").select<Incident>(ctx.user, "incidents", { status: "open" });
  return Response.json(incidents);
}
```

- **Stopped at compile time** [tsc]: everything A stops, plus "no User", plus a hand-written `User` literal (because `UserId` is branded).
- **Stopped only at run time:** `ctx.user!`, raw-SQL writes, a principal kept from an earlier request. The host charges that to whichever request is in flight, or refuses with `no-request` [judgment, consistent with the identity research].
- **Not stopped at all:** a stale generated file, as in A. Also, because handles live at module level, they invite operations at the top level of a module. Those still need a principal and fail at run time [judgment].
- **Ease for a Coding agent:** the null check is natural TypeScript. But a principal on every call is noisy, and `null` is forced even on Applications where every route requires sign-in [judgment].
- **Messages** [tsc]:
  - No User: `Argument of type 'User | null' is not assignable to parameter of type 'User'. Type 'null' is not assignable to type 'User'.` This is the best "no User" message of the three.
  - Acting as itself on a shared Resource: `Type 'Application' is missing the following properties from type 'User': id, email, displayName`. This misleads an agent into thinking it lacks fields.

## Comparison

| | A vision-literal | B defineApp (agent's pick) | C principal argument |
|---|---|---|---|
| Handler | `{fetch} satisfies App` | `defineApp(manifest, {fetch})` | `{fetch} satisfies App` |
| As the User / as the Application | `ctx.user().db()` / `ctx.app().ownDb()` | same | `db(n).op(ctx.user)` / `ownDb().op(ctx.app)` |
| No User | run time (throws) | compile time if `signIn: "optional"`; impossible if `"required"` | compile time, always (`\| null`) |
| Where Declared access comes from | JSON + generated `.d.ts` | the TypeScript value itself | JSON + generated `.d.ts` |
| Undeclared name message lists the valid names | no (alias name) | yes | no (alias name) |
| Write-through-`read` message names the fix | no | yes | no |
| Stale manifest or handler caught | only if regenerated | yes | only if regenerated |
| Expected errors confirmed (each fails when its `@ts-expect-error` is removed) [tsc] | 6 | 9 | 7 |

All three pass `tsc` with no errors [tsc].

## Open product questions (not decided here)

1. Will Milestone 1 have routes without sign-in at all? If not, B's `signIn` can be fixed at `"required"` and the null case disappears.
2. Is "sign-in optional" set per Application or per route?
3. Does a User need a Grant to act on an Application's own database, or does access to the Application imply it? Who decides?
4. Does an Application have exactly one own database, and is it listed in the manifest?
5. When an Application acts as itself during a request, does the Operation log or Change history still record the User in flight? The fake host records it.
6. Should a refusal's `fix` text, shown to Users, reveal Group or Grant details?
7. Is the manifest a TypeScript file the deploy evaluates (B), or JSON (A, C)? This is also about what IT reads in a review. A refinement suggested for B: a better "no User" message by making `user()` return a value tagged `signedIn` instead of `null`, which costs the plain `if (!user)` check.
8. Should a raw-SQL escape hatch exist on read-declared handles at all?

## Branch, commits, how to run

- Branch `spike/trusted-context-types`, pushed to `origin`. `main` was not touched.
- Commits:
  - `1bf46a5`: three candidates, sample handlers, type tests, generator, error script.
  - `4e62300`: fake host for B.
- To run:
  - `cd prototypes/trusted-context && npm install`
  - `npm run check` runs `tsc -p` on each candidate.
  - `npm run errors` removes each `@ts-expect-error` in turn and prints the compiler message it was hiding.
  - `npm run run-b` runs B's handlers against the fake host.
  - `npm run gen` regenerates `vm-env.d.ts` for A and C.
