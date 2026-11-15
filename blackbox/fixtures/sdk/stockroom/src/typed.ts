// Typed queries through ctx.user().db("stock"), checked by `tsc` only (no
// route runs them): what a definition of `stock` must give the compiler
// (CONTRACT.md, "Pulling a Resource's structure: The definition").
// blackbox/tests/pull_compiles.rs also checks that removing an
// `@ts-expect-error` line makes `tsc` fail, so that each one bites.
import { defineApp } from "@vm/sdk";
import { eq } from "drizzle-orm";
import manifest from "../manifest.ts";
import { crmAccounts, crmTier, items, movements, openItems, siteTotals, sites, status } from "../resources/stock/schema.ts";

/** True exactly when A and B are the same type (`any` and `unknown` told apart). */
type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;

type Status = "open" | "in progress" | "closed";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const db = ctx.user().db("stock");

    const [item] = await db.select().from(items).where(eq(items.id, 1));
    if (item !== undefined) {
      const columns: [
        Equal<typeof item.id, number>,
        Equal<typeof item.siteId, number>,
        Equal<typeof item.sku, string>,
        Equal<typeof item.label, string>,
        Equal<typeof item.status, Status>,
        Equal<typeof item.price, string | null>,
        Equal<typeof item.weight, number | null>,
        Equal<typeof item.ratio, number | null>,
        Equal<typeof item.qty, number>,
        Equal<typeof item.tags, string[] | null>,
        Equal<typeof item.grid, number[] | null>,
        Equal<typeof item.statuses, Status[] | null>,
        Equal<typeof item.addedAt, string>,
        Equal<typeof item.checkedAt, string | null>,
        Equal<typeof item.opens, string | null>,
        Equal<typeof item.opensTz, string | null>,
        Equal<typeof item.shelfLife, string | null>,
        Equal<typeof item.ref, string | null>,
        Equal<typeof item.photo, unknown>,
        Equal<typeof item.stocked, unknown>,
        Equal<typeof item.quantity, unknown>,
        Equal<typeof item.labelUpper, string | null>,
      ] = [true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true, true];
      void columns;
    }

    const [site] = await db.select().from(sites).limit(1);
    if (site !== undefined) {
      const columns: [
        Equal<typeof site.id, number>,
        Equal<typeof site.code, string>,
        Equal<typeof site.openedOn, string | null>,
        Equal<typeof site.area, unknown>,
        Equal<typeof site.active, boolean>,
      ] = [true, true, true, true, true];
      void columns;
    }

    // A join through the foreign key items.site_id -> sites.id.
    const joined = await db
      .select({ label: items.label, site: sites.name, siteCode: sites.code })
      .from(items)
      .innerJoin(sites, eq(items.siteId, sites.id));
    const joinedRow: Equal<(typeof joined)[number], { label: string; site: string; siteCode: string }> = true;
    void joinedRow;

    // The enums' labels, as unions.
    const labels: [
      Equal<(typeof status.enumValues)[number], Status>,
      Equal<(typeof crmTier.enumValues)[number], "bronze" | "silver" | "gold">,
    ] = [true, true];
    void labels;
    const [account] = await db.select().from(crmAccounts);
    if (account !== undefined) {
      const tier: Equal<typeof account.tier, "bronze" | "silver" | "gold"> = true;
      void tier;
    }

    // A view and a materialized view.
    const open = await db.select().from(openItems);
    const openRow: Equal<(typeof open)[number]["status"], Status | null> = true;
    void openRow;
    const totals = await db.select().from(siteTotals);
    const totalsRow: Equal<(typeof totals)[number]["itemCount"], number | null> = true;
    void totalsRow;

    // Inserts without the columns that have a default, an identity, a
    // serial or a generation.
    await db.insert(items).values({ siteId: 1, sku: "ZZ-9", label: "Washers" });
    await db.insert(sites).values({ code: "east", name: "East depot" });
    await db.insert(movements).values({ itemId: 1, seq: 1, amount: 5 });
    await db.insert(crmAccounts).values({ name: "Initech" });

    // And not without a required one.
    // @ts-expect-error `label` is NOT NULL and has no default.
    await db.insert(items).values({ siteId: 1, sku: "ZZ-9" });
    // @ts-expect-error `amount` is NOT NULL and has no default.
    await db.insert(movements).values({ itemId: 1, seq: 2 });
    // @ts-expect-error "lost" is no label of `status`.
    await db.insert(items).values({ siteId: 1, sku: "ZZ-9", label: "Washers", status: "lost" });

    return Response.json({});
  },
});
