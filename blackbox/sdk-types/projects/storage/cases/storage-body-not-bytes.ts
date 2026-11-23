// A file's body is the ReadableStream the host feeds, not the file whole:
// what a Uint8Array has is not on it (CONTRACT.md, "Storage: Through the
// SDK").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const file = await ctx.user().storage("archive").get("reports/q3.pdf");
    if (file === null) return Response.json({ none: true });
    // @expect TS2339 Property 'byteLength' does not exist on type 'ReadableStream<Uint8Array<ArrayBufferLike>>'
    const length = file.body.byteLength;
    return Response.json({ length });
  },
});
