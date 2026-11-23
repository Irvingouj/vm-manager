// Calls the outside with `fetch` through the SDK (blackbox/tests/outbound_sdk.rs;
// CONTRACT.md "Outbound calls: What Application code does"). With the SDK a
// refusal rejects with `Refused` and a failure with `OperationFailed`; one that
// escapes the handler is the `403` with `{ refusal, fix }` or the platform's
// fixed `500`.
//
//   GET /caught?url=    the call's rejection described: { isRefused, isFailed,
//                       name, refusal, fix, kind, detail } (or { status })
//   GET /escape?url=    the call not caught
import { defineApp, OperationFailed, Refused } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(request) {
    const url = new URL(request.url);
    const target = url.searchParams.get("url") ?? "";
    switch (url.pathname) {
      case "/caught": {
        try {
          const answer = await fetch(target);
          return Response.json({ status: answer.status, text: await answer.text() });
        } catch (e) {
          const err = e as Record<string, unknown>;
          return Response.json({
            isRefused: e instanceof Refused,
            isFailed: e instanceof OperationFailed,
            isError: e instanceof Error,
            name: err.name,
            refusal: err.refusal ?? null,
            fix: err.fix ?? null,
            kind: err.kind ?? null,
            detail: err.detail ?? null,
            refused: err.refused ?? null,
            failure: err.failure ?? null,
          });
        }
      }
      case "/escape": {
        const answer = await fetch(target);
        return Response.json({ escaped: false, status: answer.status });
      }
      default:
        return Response.json({ route: url.pathname }, { status: 404 });
    }
  },
});
