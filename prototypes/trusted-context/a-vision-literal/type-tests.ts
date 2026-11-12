// Candidate A type tests. Every @ts-expect-error line must fail to compile;
// the "COMPILES" lines document holes that only the host (or nothing) stops.
import type { TrustedContext, ActingAsUser, App } from "@vm/sdk";
import { Refused } from "@vm/sdk";

declare const ctx: TrustedContext;

export async function tests() {
  // 1. Constructing a Trusted context.
  // @ts-expect-error T1 the brand symbol is not exported, so no object literal can satisfy the type
  const forged: TrustedContext = { requestId: "r1", user: () => ({}) as ActingAsUser, userOrNull: () => null, app: () => ({}) as never };
  // @ts-expect-error T1b Refused has a private constructor
  new Refused();

  // 2. Naming an undeclared Resource.
  // @ts-expect-error T2 "payroll" is not in vm.manifest.json
  ctx.user().db("payroll");

  // 3. Writing through a `read` declaration.
  // @ts-expect-error T3 devops is declared at read
  await ctx.user().db("devops").insert("incidents", { title: "x" });
  await ctx.user().db("facilities").insert("rooms", { name: "x" }); // declared at write: fine

  // 4. Acting as itself on a shared Resource.
  // @ts-expect-error T4 ActingAsApplication has only ownDb()
  ctx.app().db("devops");

  // 5. Using a User handle when there may be no User: NOT stopped by A.
  ctx.user().db("devops"); // COMPILES. Host throws Refused { reason: "no-user" } at run time.

  // Holes common to all candidates (COMPILE, by design of TypeScript):
  const cast = {} as TrustedContext; // COMPILES. Gains nothing: identity is in the host, not in this object.
  void cast;
  await ctx.user().db("devops").query({ text: "delete from incidents", params: [] }); // COMPILES. Host must run read-declared handles on a read-only role.
  void forged;
}

// Handler shape: a handler that ignores ctx's type cannot pass the wrong thing.
export default {
  // @ts-expect-error T6 ctx is a TrustedContext, not a string
  fetch(_request, ctx: string) { return new Response(ctx); },
} satisfies App;
