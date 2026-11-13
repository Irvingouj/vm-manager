// A correct use of transactions compiles with no diagnostics (CONTRACT.md,
// "Transactions: Through the SDK"): `transaction` on every handle (a shared
// database declared at read, one declared at write, the own database as the
// User and as the Application), Drizzle's second argument, `tx` with the
// same types as its handle, `tx.rollback()`, and the callback's value as
// what `transaction` resolves to.
import { defineApp, OperationFailed } from "@vm/sdk";
import { eq, sql } from "drizzle-orm";
import manifest from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";
import { employees } from "../resources/hr/schema.ts";
import { counters, notes } from "../src/db/schema.ts";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
function same<A, B>(_proof: Equal<A, B>): void {}

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const user = ctx.user();

    const moved = await user.ownDb().transaction(async (tx) => {
      const [note] = await tx.select().from(notes).where(eq(notes.id, 1));
      await tx.update(counters).set({ n: sql`${counters.n} + 1` }).where(eq(counters.name, "hits"));
      await tx.insert(notes).values({ incidentId: 1, body: "reassigned" });
      await tx.delete(notes).where(eq(notes.id, 2));
      return note ? note.id : 0;
    }, { isolationLevel: "serializable" });
    same<typeof moved, number>(true);

    const titles = await user.db("devops").transaction(async (tx) => {
      const open = await tx.select({ title: incidents.title }).from(incidents).where(eq(incidents.open, true));
      const counted = await tx.query.incidents.findMany();
      return { titles: open.map((i) => i.title), counted: counted.length };
    }, { isolationLevel: "repeatable read", accessMode: "read only" });
    same<typeof titles, { titles: string[]; counted: number }>(true);

    await user.db("hr").transaction(async (tx) => {
      await tx.insert(employees).values({ id: 8, name: "Ada" });
      await tx.update(employees).set({ team: "platform" }).where(eq(employees.id, 8));
      await tx.delete(employees).where(eq(employees.id, 7));
    }, { accessMode: "read write", isolationLevel: "read committed" });

    try {
      await ctx.app().ownDb().transaction(async (tx) => {
        await tx.update(counters).set({ n: 0 });
        tx.rollback();
      });
    } catch (error) {
      if (error instanceof OperationFailed) return Response.json({ kind: error.kind }, { status: 502 });
    }

    return Response.json({ moved, titles });
  },
});
