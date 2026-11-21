// `delete` through a handle declared at read: the message names the fix
// (CONTRACT.md, "Storage: Through the SDK").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const archive = ctx.user().storage("archive");
    // @expect TS* 'archive' is declared at 'read' in manifest.ts. Declare it at 'write' to change files.
    const deleting = archive.delete("reports/old.pdf");
    return Response.json({ deleting: String(deleting) });
  },
});
