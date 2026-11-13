// A change of structure through the SDK's types (CONTRACT.md, "The Change
// history in the platform database: Through the SDK"): `kind` is `"insert"
// | "update" | "delete" | "structure"`, and a change has one more property,
// `structure`. Its type is exported as `StructureChange`: a union
// discriminated by `change`, each member with exactly the properties the
// table of "Changes of structure that change data" gives it (`column`,
// `from`, `to`, `type`, `partition` strings; `columns` a `string[]`).
// `Change<T>` is narrowed by `kind`: for `"structure"`, `structure` is a
// `StructureChange` and `old`, `new` and `changed` are `null`; for the
// others `structure` is `null`. This case must compile with no error.
import { defineApp } from "@vm/sdk";
import type { Change, StructureChange } from "@vm/sdk";
import manifest from "../manifest.ts";
import { notes } from "../src/db/schema.ts";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
function same<A, B>(_proof: Equal<A, B>): void {}

type Member<C extends string> = Extract<StructureChange, { change: C }>;

// The discriminant: the table's eight words, no other.
same<
  StructureChange["change"],
  | "column-type"
  | "column-dropped"
  | "table-dropped"
  | "table-replaced"
  | "table-rewritten"
  | "enum-value-renamed"
  | "partition-attached"
  | "partition-detached"
>(true);

// Each member: exactly its properties, of their types.
same<keyof Member<"column-type">, "change" | "column" | "from" | "to">(true);
same<Member<"column-type">["column"], string>(true);
same<Member<"column-type">["from"], string>(true);
same<Member<"column-type">["to"], string>(true);
same<keyof Member<"column-dropped">, "change" | "column">(true);
same<Member<"column-dropped">["column"], string>(true);
same<keyof Member<"table-dropped">, "change">(true);
same<keyof Member<"table-replaced">, "change">(true);
same<keyof Member<"table-rewritten">, "change">(true);
same<keyof Member<"enum-value-renamed">, "change" | "type" | "from" | "to" | "columns">(true);
same<Member<"enum-value-renamed">["type"], string>(true);
same<Member<"enum-value-renamed">["from"], string>(true);
same<Member<"enum-value-renamed">["to"], string>(true);
same<Member<"enum-value-renamed">["columns"], string[]>(true);
same<keyof Member<"partition-attached">, "change" | "partition">(true);
same<Member<"partition-attached">["partition"], string>(true);
same<keyof Member<"partition-detached">, "change" | "partition">(true);
same<Member<"partition-detached">["partition"], string>(true);

// Values of each member are written as the contract's table writes them.
const written: StructureChange[] = [
  { change: "column-type", column: "price", from: "numeric(10,2)", to: "numeric(12,3)" },
  { change: "column-dropped", column: "tags" },
  { change: "table-dropped" },
  { change: "table-replaced" },
  { change: "table-rewritten" },
  { change: "enum-value-renamed", type: "public.mood", from: "sad", to: "blue", columns: ["a", "b"] },
  { change: "partition-attached", partition: "public.measures_2027" },
  { change: "partition-detached", partition: "public.measures_2026" },
];

/** Narrowing by `change`: each member's own properties, read as it has them. */
function say(structure: StructureChange): string {
  switch (structure.change) {
    case "column-type":
      return `${structure.column}: ${structure.from} -> ${structure.to}`;
    case "column-dropped":
      return structure.column;
    case "table-dropped":
    case "table-replaced":
    case "table-rewritten":
      return structure.change;
    case "enum-value-renamed": {
      const columns: string[] = structure.columns;
      return `${structure.type} ${structure.from} -> ${structure.to} in ${columns.join(", ")}`;
    }
    case "partition-attached":
    case "partition-detached":
      return structure.partition;
    default: {
      // Every member handled: what is left is `never`.
      const none: never = structure;
      return none;
    }
  }
}

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const changes: Change<typeof notes>[] = await ctx.user().ownDb().history(notes, { id: 7 });
    const said: string[] = written.map(say);
    for (const change of changes) {
      same<typeof change.kind, "insert" | "update" | "delete" | "structure">(true);
      if (change.kind === "structure") {
        // Narrowed by `kind`: a `StructureChange`, never null; no row.
        same<typeof change.structure, StructureChange>(true);
        same<typeof change.old, null>(true);
        same<typeof change.new, null>(true);
        same<typeof change.changed, null>(true);
        said.push(say(change.structure));
      } else {
        same<typeof change.structure, null>(true);
        const title: string | undefined = change.new?.body ?? change.old?.body;
        said.push(`${change.kind}: ${title ?? ""}`);
      }
    }
    return Response.json({ said });
  },
});
