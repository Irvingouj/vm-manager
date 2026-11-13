// A key the types can check is checked (CONTRACT.md, "Reading the Change
// history: The call"): for a primary key declared on its column with
// `.primaryKey()`, the wrong property and the wrong type of value are
// compile errors. The contract pins no wording, so any error will do. The
// right keys, first, compile: without them every line below would be an
// error only because `history` is missing.
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { counters, notes } from "../src/db/schema.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    await ctx.user().ownDb().history(notes, { id: 7 });
    await ctx.app().ownDb().history(counters, { name: "hits" });
    // @expect TS*
    await ctx.user().ownDb().history(notes, { nope: 7 });
    // @expect TS*
    await ctx.user().ownDb().history(notes, { id: "7" });
    // @expect TS*
    await ctx.app().ownDb().history(notes, { incidentId: 7 });
    // @expect TS*
    await ctx.app().ownDb().history(counters, { name: 1 });
    // @expect TS*
    await ctx.user().ownDb().history(counters, { id: 1 });
    return new Response("unreachable");
  },
});
