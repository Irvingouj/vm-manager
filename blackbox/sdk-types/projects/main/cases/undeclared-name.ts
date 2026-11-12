// Naming a Resource the Manifest does not declare: the message lists the
// declared names (CONTRACT.md, "The types").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS2345 is not assignable to parameter of type '"devops" | "hr"'
    const payroll = ctx.user().db("payroll");
    return Response.json({ payroll: String(payroll) });
  },
});
