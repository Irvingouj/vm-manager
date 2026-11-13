// A change of structure through the SDK's types (CONTRACT.md, "The Change
// history in the platform database: Through the SDK"): `kind` is `"insert"
// | "update" | "delete" | "structure"`, and a change has one more property,
// `structure`, `null` for a row's change and for `structure` an object. The
// contract does not give `structure`'s type beyond that; what is asserted
// here is what any typing of it must allow: it may be null, and once it is
// not, it has `change`, readable as a string. Narrowing on `kind` compiles.
import { defineApp } from "@vm/sdk";
import type { Change } from "@vm/sdk";
import manifest from "../manifest.ts";
import { notes } from "../src/db/schema.ts";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
function same<A, B>(_proof: Equal<A, B>): void {}

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const changes: Change<typeof notes>[] = await ctx.user().ownDb().history(notes, { id: 7 });
    const said: string[] = [];
    for (const change of changes) {
      same<typeof change.kind, "insert" | "update" | "delete" | "structure">(true);
      const structure = change.structure;
      const maybeNull: typeof structure = null;
      if (change.kind === "structure") {
        if (change.structure !== null) {
          const what: string = change.structure.change;
          said.push(what);
        }
      } else {
        const title: string | undefined = change.new?.body ?? change.old?.body;
        said.push(`${change.kind}: ${title ?? ""}`);
      }
      void maybeNull;
    }
    return Response.json({ said });
  },
});
