// Acting as the Application is only for the own database: ctx.app() has no
// db(name) (CONTRACT.md, "The types").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS2339 Property 'db' does not exist on type 'ActingAsApplication'.
    const devops = ctx.app().db("devops");
    return Response.json({ devops: String(devops) });
  },
});
