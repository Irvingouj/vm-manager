// `storage(name)` takes only names declared `storage`: a name declared
// `postgres` is refused at compile time with the names declared `storage`
// (CONTRACT.md, "Storage: Through the SDK").
import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // @expect TS2345 is not assignable to parameter of type '"marketing" | "archive"'
    const devops = ctx.user().storage("devops");
    return Response.json({ devops: String(devops) });
  },
});
