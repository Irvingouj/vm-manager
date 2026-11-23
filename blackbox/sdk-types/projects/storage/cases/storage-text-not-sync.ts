// `text()` reads the body to its end, so it answers a Promise, not a
// string as it did when the body was the bytes whole (CONTRACT.md,
// "Storage: Through the SDK").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const file = await ctx.user().storage("archive").get("reports/q3.pdf");
    if (file === null) return Response.json({ none: true });
    // @expect TS2322 Type 'Promise<string>' is not assignable to type 'string'.
    const text: string = file.text();
    return Response.json({ text });
  },
});
