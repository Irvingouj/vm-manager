// A put's body is one of the six kinds — a ReadableStream, a string, an
// ArrayBuffer, an ArrayBufferView, a Blob or null; a number is none of them,
// and neither is `undefined`
// (CONTRACT.md, "Storage: Through the SDK").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const files = ctx.user().storage("marketing");
    // A value typed `number`, not the literal `5`: the compiler names a
    // literal's own type ('5') when the parameter's union holds `null`.
    const five: number = Number("5");
    // @expect TS2345 Argument of type 'number' is not assignable to parameter of type 'FileBody'.
    await files.put("reports/q4.pdf", five);
    // @expect TS2345 Argument of type 'undefined' is not assignable to parameter of type 'FileBody'.
    await files.put("reports/q4.pdf", undefined);
    return Response.json({ ok: true });
  },
});
