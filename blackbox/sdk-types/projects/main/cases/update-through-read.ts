// `update` through a handle declared at read: the message names the fix
// (CONTRACT.md, "The types").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const devops = ctx.user().db("devops");
    // @expect TS* 'devops' is declared at 'read' in manifest.ts. Declare it at 'write' to change rows.
    const changing = devops.update(incidents);
    return Response.json({ changing: String(changing) });
  },
});
