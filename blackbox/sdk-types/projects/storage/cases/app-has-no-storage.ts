// Acting as the Application is only for the own database: ctx.app() has no
// storage(name) (CONTRACT.md, "Storage: Through the SDK").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS2339 Property 'storage' does not exist on type 'ActingAsApplication'.
    const marketing = ctx.app().storage("marketing");
    return Response.json({ marketing: String(marketing) });
  },
});
