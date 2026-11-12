// A correct Application compiles with no diagnostics (CONTRACT.md, "What
// Application code is given"): typed selects on a shared database declared
// at read (a join, a relational query, a `with`), writes on one declared at
// write, the own database as the User and as the Application, and Refused
// and OperationFailed in a catch. The rows have the types Drizzle declares.
import { defineApp, OperationFailed, Refused } from "@vm/sdk";
import { and, asc, desc, eq, sql } from "drizzle-orm";
import manifest from "../manifest.ts";
import { incidents, services, teams } from "../resources/devops/schema.ts";
import { employees } from "../resources/hr/schema.ts";
import { counters, notes } from "../src/db/schema.ts";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
function same<A, B>(_proof: Equal<A, B>): void {}

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const user = ctx.user();
    same<typeof user.id, string>(true);
    same<typeof user.groups, readonly string[]>(true);

    // A shared database declared at read: selects.
    const devops = user.db("devops");
    const open = await devops
      .select()
      .from(incidents)
      .where(and(eq(incidents.open, true), eq(incidents.severity, 2)))
      .orderBy(desc(incidents.reportedAt));
    const first = open[0];
    if (first) {
      same<typeof first.id, number>(true);
      same<typeof first.title, string>(true);
      same<typeof first.reportedAt, Date>(true);
      same<typeof first.cost, string | null>(true);
      same<typeof first.views, bigint | null>(true);
      same<typeof first.open, boolean>(true);
    }
    const joined = await devops
      .select({ service: services.name, team: teams.name })
      .from(services)
      .innerJoin(teams, eq(teams.id, services.teamId))
      .orderBy(asc(services.name));
    const withTeams = await devops.query.services.findMany({ with: { team: true } });
    const firstService = withTeams[0];
    if (firstService) same<typeof firstService.team.name, string>(true);
    const severe = devops.$with("severe").as(devops.select().from(incidents).where(eq(incidents.severity, 3)));
    const fromWith = await devops.with(severe).select().from(severe);

    // A shared database declared at write: changes.
    const hr = user.db("hr");
    const [hired] = await hr.insert(employees).values({ id: 7, name: "Grace" }).returning();
    await hr.update(employees).set({ team: "platform" }).where(eq(employees.id, 7));
    await hr.delete(employees).where(eq(employees.id, 7));
    if (hired) same<typeof hired.name, string>(true);

    // The own database, as the User and as the Application.
    const [note] = await user.ownDb().insert(notes).values({ incidentId: 1, body: "seen" }).returning();
    if (note) same<typeof note.createdAt, Date>(true);
    await ctx.app().ownDb().update(counters).set({ n: sql`${counters.n} + 1` }).where(eq(counters.name, "hits"));
    const mine = await ctx.app().ownDb().query.notes.findMany({ orderBy: [asc(notes.id)] });
    await user.ownDb().delete(notes).where(eq(notes.id, 1));

    // Refusals and failures.
    try {
      await devops.select().from(incidents);
    } catch (error) {
      if (error instanceof Refused) {
        const reason: string = error.refusal.reason;
        const fix: string = error.fix;
        const name: string = error.name;
        return Response.json({ reason, fix, name, message: error.message }, { status: 403 });
      }
      if (error instanceof OperationFailed) {
        const kind: string = error.kind;
        const code: string | undefined = error.code ?? undefined;
        const detail: unknown = error.detail;
        return Response.json({ kind, code, detail }, { status: 502 });
      }
      throw error;
    }

    return Response.json({ url: request.url, open, joined, withTeams, fromWith, mine: mine.length });
  },
});
