// Only to read (CONTRACT.md, "Reading the Change history: Only its own, and
// only to read"): @vm/sdk exports no table object (a Drizzle table) for
// vm.history, nor any other, and the handles of the own database have no
// call about history but `history` itself. This case compiles with no
// diagnostics when that holds.
import * as sdk from "@vm/sdk";
import { defineApp } from "@vm/sdk";
import type { Table } from "drizzle-orm";
import manifest from "../manifest.ts";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
function same<A, B>(_proof: Equal<A, B>): void {}

type Exports = typeof sdk;
type TableExports = { [K in keyof Exports]: Exports[K] extends Table ? K : never }[keyof Exports];
same<TableExports, never>(true);

type HistoryNames<T> = Extract<keyof T, `${string}istory${string}` | `${string}ISTORY${string}`>;

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const asUser = ctx.user().ownDb();
    const asApplication = ctx.app().ownDb();
    same<HistoryNames<typeof asUser>, "history">(true);
    same<HistoryNames<typeof asApplication>, "history">(true);
    return new Response("ok");
  },
});
