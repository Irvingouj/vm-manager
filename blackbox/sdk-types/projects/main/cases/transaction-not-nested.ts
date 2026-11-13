// Nesting is refused, and `transaction` is not in the types of `tx`; nor is
// raw SQL (CONTRACT.md, "Transactions: Through the SDK": `tx` has the same
// types as its handle, which has no `execute`).
import { defineApp } from "@vm/sdk";
import { sql } from "drizzle-orm";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    await ctx.user().ownDb().transaction(async (tx) => {
      // @expect TS2339 Property 'transaction' does not exist on type
      await tx.transaction(async () => 1);
      // @expect TS2339 Property 'execute' does not exist on type
      await tx.execute(sql`delete from notes`);
    });
    await ctx.user().db("hr").transaction(async (tx) => {
      // @expect TS2339 Property 'transaction' does not exist on type
      await tx.transaction(async () => 1);
    });
    await ctx.app().ownDb().transaction(async (tx) => {
      // @expect TS2339 Property 'transaction' does not exist on type
      await tx.transaction(async () => 1);
    });
    await ctx.user().db("devops").transaction(async (tx) => {
      // @expect TS2339 Property 'transaction' does not exist on type
      await tx.transaction(async () => 1);
    });
    return new Response("unreachable");
  },
});
