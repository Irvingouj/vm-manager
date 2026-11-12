import { defineApp, Refused } from "@vm/sdk";
import { and, desc, eq, ne } from "drizzle-orm";
import manifest, { type Ctx } from "../manifest.ts";
import { incidents, services } from "../resources/devops/schema.ts";
import { notes } from "./db/schema.ts";
import { runtimeProbes } from "./probe.ts";

async function openIncidents(ctx: Ctx) {
  const devops = ctx.user().db("devops");
  return devops
    .select({ id: incidents.id, title: incidents.title, severity: incidents.severity, service: services.name })
    .from(incidents)
    .innerJoin(services, eq(services.id, incidents.serviceId))
    .where(and(ne(incidents.status, "resolved")))
    .orderBy(incidents.severity);
}

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    try {
      if (request.method === "GET" && url.pathname === "/incidents") {
        return Response.json(await openIncidents(ctx));
      }
      if (request.method === "POST" && url.pathname === "/notes") {
        const input = (await request.json()) as { incidentId: number; body: string };
        const user = ctx.user();
        const [row] = await user
          .ownDb()
          .insert(notes)
          .values({ incidentId: input.incidentId, author: user.email, body: input.body })
          .returning();
        return Response.json(row, { status: 201 });
      }
      if (request.method === "GET" && url.pathname === "/notes") {
        const rows = await ctx.app().ownDb().select().from(notes).orderBy(desc(notes.id));
        return Response.json(rows);
      }
      if (request.method === "GET" && url.pathname === "/probe") {
        return Response.json(await runtimeProbes(ctx));
      }
      if (request.method === "GET" && url.pathname === "/notes-in-tx") {
        const count = await ctx.user().ownDb().transaction(async (tx) => {
          const rows = await tx.select().from(notes).where(eq(notes.author, ctx.user().email));
          return rows.length;
        });
        return Response.json({ mine: count });
      }
    } catch (e) {
      if (e instanceof Refused && e.refusal.reason === "no-grant") {
        return Response.json({ message: "You do not have access to incidents.", fix: e.fix }, { status: 403 });
      }
      throw e;
    }
    return new Response("Not found", { status: 404 });
  },
});
