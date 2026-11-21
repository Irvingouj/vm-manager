// Naming a Storage Resource the Manifest does not declare: the message
// lists the names declared `storage` (CONTRACT.md, "Storage: Through the
// SDK").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS2345 is not assignable to parameter of type '"marketing" | "archive"'
    const payroll = ctx.user().storage("payroll");
    return Response.json({ payroll: String(payroll) });
  },
});
