// Through a handle declared at read, `tx` offers reads only (CONTRACT.md,
// "Transactions: Through the SDK"): insert, update and delete on it are
// compile errors, each alone on its line as in insert-through-read.ts.
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    await ctx.user().db("devops").transaction(async (tx) => {
      await tx.select().from(incidents);
      // @expect TS*
      const inserting = tx.insert(incidents);
      // @expect TS*
      const updating = tx.update(incidents);
      // @expect TS*
      const deleting = tx.delete(incidents);
      return [inserting, updating, deleting].map(String);
    });
    return new Response("unreachable");
  },
});
