// Reads the fixture database `stock` through its pulled definition
// (blackbox/tests/pull_compiles.rs). Every route answers JSON:
//
//   /items        items joined to their sites through the foreign key
//   /open         the view open_items
//   /accounts     crm.accounts, with its enum
//   /unsupported  a column of a type the puller cannot express (polygon)
import { defineApp } from "@vm/sdk";
import { asc, eq } from "drizzle-orm";
import manifest from "../manifest.ts";
import { crmAccounts, items, openItems, sites } from "../resources/stock/schema.ts";

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const db = ctx.user().db("stock");
    switch (url.pathname) {
      case "/items": {
        const rows = await db
          .select({ id: items.id, site: sites.code, label: items.label, status: items.status, price: items.price })
          .from(items)
          .innerJoin(sites, eq(items.siteId, sites.id))
          .orderBy(asc(items.id));
        return Response.json({ rows });
      }
      case "/open": {
        const rows = await db.select({ id: openItems.id, status: openItems.status }).from(openItems).orderBy(asc(openItems.id));
        return Response.json({ rows });
      }
      case "/accounts": {
        const rows = await db.select({ name: crmAccounts.name, tier: crmAccounts.tier }).from(crmAccounts).orderBy(asc(crmAccounts.name));
        return Response.json({ rows });
      }
      case "/unsupported": {
        const rows = await db.select({ id: sites.id, area: sites.area }).from(sites).orderBy(asc(sites.id));
        return Response.json({ rows });
      }
    }
    return Response.json({ error: "no such route" }, { status: 404 });
  },
});
