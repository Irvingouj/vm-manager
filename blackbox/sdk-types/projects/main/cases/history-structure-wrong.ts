// What `StructureChange` refuses (CONTRACT.md, "The Change history in the
// platform database: Through the SDK": "a union discriminated by `change`,
// each member with exactly the properties the table below gives it"; and
// "`Change<T>` is narrowed by `kind`"). The contract pins no wording, so
// any error will do. The right forms, first, compile: without them every
// line below would be an error only because the type is missing.
import { defineApp } from "@vm/sdk";
import type { Change, StructureChange } from "@vm/sdk";
import manifest from "../manifest.ts";
import { notes } from "../src/db/schema.ts";

const right: StructureChange = { change: "column-dropped", column: "tags" };

// A `change` the table does not have.
// @expect TS*
const renamed: StructureChange = { change: "column-renamed", column: "tags" };
// A member's property missing (`to`).
// @expect TS*
const half: StructureChange = { change: "column-type", column: "price", from: "integer" };
// A property the member does not have.
// @expect TS*
const extra: StructureChange = { change: "table-dropped", column: "tags" };
// Another member's property, not narrowed to it.
// @expect TS*
const partitionOf = (s: StructureChange): string => s.partition;
// `columns` is a list of names, not one.
// @expect TS*
const one: StructureChange = { change: "enum-value-renamed", type: "public.mood", from: "a", to: "b", columns: "a" };

/** Comparing `change` with a word the union does not have. */
function wrong(s: StructureChange): boolean {
  // @expect TS*
  return s.change === "table-truncated";
}

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const changes: Change<typeof notes>[] = await ctx.user().ownDb().history(notes, { id: 7 });
    for (const change of changes) {
      if (change.kind === "structure") {
        // Narrowed to a change of structure: no row.
        // @expect TS*
        const body: string = change.new.body;
        void body;
      } else {
        // Narrowed to a row's change: `structure` is null.
        // @expect TS*
        const what: string = change.structure.change;
        void what;
      }
    }
    void [right, renamed, half, extra, partitionOf, one, wrong];
    return new Response("unreachable");
  },
});
