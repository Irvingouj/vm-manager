// `db(name)` takes only names declared `postgres`: a name declared
// `storage` is refused at compile time with the names declared `postgres`
// (CONTRACT.md, "Storage: Through the SDK").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS2345 is not assignable to parameter of type '"devops" | "hr"'
    const marketing = ctx.user().db("marketing");
    return Response.json({ marketing: String(marketing) });
  },
});
