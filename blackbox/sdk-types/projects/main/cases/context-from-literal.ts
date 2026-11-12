// A context cannot be made by Application code from an object literal, even
// one holding the real context's own methods (CONTRACT.md, "The types").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS*
    const forged: typeof ctx = { user: () => ctx.user(), app: () => ctx.app() };
    return Response.json({ same: forged === ctx });
  },
});
