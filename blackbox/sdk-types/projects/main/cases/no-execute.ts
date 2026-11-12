// The handles offer no raw SQL: there is no `execute` on any of them
// (CONTRACT.md, "What Application code is given" and "The types").
import { defineApp } from "@vm/sdk";
import { sql } from "drizzle-orm";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS2339 Property 'execute' does not exist on type
    await ctx.user().db("devops").execute(sql`select 1`);
    // @expect TS2339 Property 'execute' does not exist on type
    await ctx.user().db("hr").execute(sql`delete from employees`);
    // @expect TS2339 Property 'execute' does not exist on type
    await ctx.user().ownDb().execute(sql`delete from notes`);
    // @expect TS2339 Property 'execute' does not exist on type
    await ctx.app().ownDb().execute(sql`delete from notes`);
    return new Response("unreachable");
  },
});
