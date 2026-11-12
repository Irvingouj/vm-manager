// Candidate B type tests. Every @ts-expect-error line must fail to compile;
// the "COMPILES" lines document holes that only the host (or nothing) stops.
import { defineApp, defineManifest, Refused, type TrustedContext } from "@vm/sdk";
import manifest, { type Ctx } from "./manifest.ts";

declare const ctx: Ctx; // signIn: "optional"

const signedInOnly = defineManifest({ signIn: "required", resources: { devops: { kind: "postgres", access: "read" } } });
declare const ctxRequired: TrustedContext<typeof signedInOnly>;

export async function tests() {
  // 1. Constructing a Trusted context.
  // @ts-expect-error T1 the brand symbol is not exported, so no object literal can satisfy the type
  const forged: Ctx = { requestId: "r1", user: () => null, requireUser: () => ({}) as never, app: () => ({}) as never };
  // @ts-expect-error T1b Refused has a private constructor
  new Refused();

  // 2. Naming an undeclared Resource.
  // @ts-expect-error T2 "payroll" is not in manifest.ts
  ctx.requireUser().db("payroll");

  // 3. Writing through a `read` declaration.
  // @ts-expect-error T3 devops is declared at read
  await ctx.requireUser().db("devops").insert("incidents", { title: "x" });
  await ctx.requireUser().db("facilities").insert("rooms", { name: "x" }); // declared at write: fine

  // 4. Acting as itself on a shared Resource.
  // @ts-expect-error T4 ActingAsApplication has only ownDb()
  ctx.app().db("devops");

  // 5. Using a User handle when there may be no User (signIn: "optional").
  // @ts-expect-error T5 user() is ActingAsUser | null
  ctx.user().db("devops");
  ctxRequired.user().db("devops"); // signIn "required": never null, and the host guarantees it
  ctx.user()!.db("devops"); // COMPILES. Non-null assertion opts out; host throws Refused { reason: "no-user" }.

  // 6. Declaring what an Application cannot declare, or computing the manifest.
  // @ts-expect-error T6 structure is not a declarable level for an Application
  defineManifest({ signIn: "required", resources: { devops: { kind: "postgres", access: "structure" } } });
  const level: string = "write";
  // @ts-expect-error T7 a computed level widens to string and is refused
  defineManifest({ signIn: "required", resources: { devops: { kind: "postgres", access: level } } });

  // 7. A handler typed against another manifest (e.g. stale after the manifest was edited).
  const bigger = defineManifest({ signIn: "optional", resources: { ...manifest.resources, payroll: { kind: "postgres", access: "read" } } });
  const handlerForBigger = (_r: Request, c: TrustedContext<typeof bigger>) => c.requireUser().db("payroll").select("salaries").then(Response.json);
  // @ts-expect-error T8 this App's context does not offer "payroll"
  defineApp(manifest, { fetch: handlerForBigger });

  // Holes common to all candidates (COMPILE, by design of TypeScript):
  const cast = {} as Ctx; // COMPILES. Gains nothing: identity is in the host, not in this object.
  void cast;
  await ctx.requireUser().db("devops").query({ text: "delete from incidents", params: [] }); // COMPILES. Host must use a read-only role.
  void forged;
}
