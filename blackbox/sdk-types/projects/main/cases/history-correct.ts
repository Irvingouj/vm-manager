// Reading the Change history compiles with no diagnostics (CONTRACT.md,
// "Reading the Change history"): `history` on both handles of the own
// database, with and without a key and options; it resolves to
// `Change<typeof table>[]`, `Change` being a type @vm/sdk exports;
// `changed` is `string[] | null`; a
// change's `old` and `new` are the table's row as `select()` types it, or
// null; `kind` and `actor` are their three words.
import { defineApp } from "@vm/sdk";
import type { Change } from "@vm/sdk";
import manifest from "../manifest.ts";
import { counters, notes } from "../src/db/schema.ts";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
function same<A, B>(_proof: Equal<A, B>): void {}

type NoteRow = typeof notes.$inferSelect;

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const changes: Change<typeof notes>[] = await ctx.user().ownDb().history(notes, { id: 7 });
    const back: Change<typeof notes>[] = changes;

    const asApplication = await ctx.app().ownDb().history(notes, { id: 7 }, { limit: 10, before: "42" });
    same<typeof asApplication, Change<typeof notes>[]>(true);
    const all = await ctx.user().ownDb().history(notes);
    same<typeof all, Change<typeof notes>[]>(true);
    const paged = await ctx.user().ownDb().history(counters, undefined, { limit: 1000 });
    same<typeof paged, Change<typeof counters>[]>(true);
    const byName = await ctx.app().ownDb().history(counters, { name: "hits" });
    same<typeof byName, Change<typeof counters>[]>(true);

    const shown: string[] = [];
    for (const change of changes) {
      same<typeof change.kind, "insert" | "update" | "delete">(true);
      same<typeof change.actor, "user" | "application" | "migration">(true);
      same<typeof change.id, string>(true);
      same<typeof change.at, Date>(true);
      same<typeof change.userId, string>(true);
      same<typeof change.application, string>(true);
      same<typeof change.request, string | null>(true);
      same<typeof change.migration, string | null>(true);
      same<NonNullable<typeof change.new>["body"], string>(true);
      same<NonNullable<typeof change.old>["createdAt"], Date>(true);
      same<NonNullable<typeof change.new>["id"], number>(true);
      const after: NoteRow | null = change.new;
      const before: NoteRow | null = change.old;
      const row: NoteRow | null = null;
      const asNew: typeof change.new = row;
      const asOld: typeof change.old = row;
      same<typeof change.changed, string[] | null>(true);
      const changed: string[] | null = change.changed;
      const title: string | undefined = change.new?.body;
      shown.push(`${change.kind} by ${change.userId} at ${change.at.toISOString()}: ${title ?? ""}`);
      void [after, before, asNew, asOld, changed];
    }
    return Response.json({ shown, back: back.length, all: all.length, byName: byName[0]?.new?.n ?? null });
  },
});
