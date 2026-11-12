// `devops` is declared but has no generated link: using its handle fails,
// and the message says why (CONTRACT.md, "The types").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";
import { employees } from "../resources/hr/schema.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS* The structure of 'devops' has not been pulled.
    const rows = await ctx.user().db("devops").select().from(incidents);
    // The pulled one is still usable.
    const people = await ctx.user().db("hr").select().from(employees);
    return Response.json({ rows, people });
  },
});
