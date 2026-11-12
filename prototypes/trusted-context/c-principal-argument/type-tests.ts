// Candidate C type tests. Every @ts-expect-error line must fail to compile;
// the "COMPILES" lines document holes that only the host (or nothing) stops.
import { db, ownDb, Refused, type TrustedContext, type User } from "@vm/sdk";

declare const ctx: TrustedContext;
declare const user: User; // as after `if (!ctx.user) return ...`

export async function tests() {
  // 1. Constructing a Trusted context, or a principal.
  // @ts-expect-error T1 the brand symbol is not exported, so no object literal can satisfy the type
  const forged: TrustedContext = { requestId: "r1", user: null, app: { kind: "application" } };
  // @ts-expect-error T1c a User principal cannot be written as a literal either
  const fakeUser: User = { kind: "user", id: "u1", email: "ceo@corp.example", displayName: "CEO" };
  // @ts-expect-error T1b Refused has a private constructor
  new Refused();

  // 2. Naming an undeclared Resource.
  // @ts-expect-error T2 "payroll" is not in vm.manifest.json
  db("payroll");

  // 3. Writing through a `read` declaration.
  // @ts-expect-error T3 devops is declared at read
  await db("devops").insert(user, "incidents", { title: "x" });
  await db("facilities").insert(user, "rooms", { name: "x" }); // declared at write: fine

  // 4. Acting as itself on a shared Resource.
  // @ts-expect-error T4 shared databases accept only a User principal
  await db("devops").select(ctx.app, "incidents");
  await ownDb().insert(ctx.app, "acks", { note: "x" }); // own database: fine

  // 5. Using the User when there may be no User.
  // @ts-expect-error T5 ctx.user is User | null
  await db("devops").select(ctx.user, "incidents");
  await db("devops").select(ctx.user!, "incidents"); // COMPILES. Non-null assertion; host refuses with no-user.

  // C-specific: handles live at module level, so code can run an operation
  // outside a request. Needs a principal; one kept from an earlier request COMPILES
  // and is attributed by the host to whichever request is in flight (or refused: no-request).

  // Holes common to all candidates (COMPILE, by design of TypeScript):
  const cast = {} as TrustedContext; // COMPILES. Gains nothing: identity is in the host, not in this object.
  void cast;
  await db("devops").query(user, { text: "delete from incidents", params: [] }); // COMPILES. Host must use a read-only role.
  void forged;
  void fakeUser;
}
