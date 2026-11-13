// `history` is only on the handles of the own database (CONTRACT.md,
// "Reading the Change history: The call"): not on a shared database's
// handle, whatever it is declared at, and not on a transaction's `tx`. The
// contract pins no wording, so any error will do.
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";
import { employees } from "../resources/hr/schema.ts";
import { notes } from "../src/db/schema.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS*
    await ctx.user().db("devops").history(incidents);
    // @expect TS*
    await ctx.user().db("hr").history(employees, { id: 1 });
    await ctx.user().ownDb().transaction(async (tx) => {
      // @expect TS*
      await tx.history(notes);
    });
    await ctx.app().ownDb().transaction(async (tx) => {
      // @expect TS*
      await tx.history(notes, { id: 1 });
    });
    await ctx.user().db("hr").transaction(async (tx) => {
      // @expect TS*
      await tx.history(employees);
    });
    return new Response("unreachable");
  },
});
