// The Application: plain Drizzle, as a Coding agent writes it, against two
// Resources: its own database ("app", schema in ./own) and a shared one
// ("devops", schema pulled by drizzle-kit into ./devops, read-only).
import { and, count, desc, eq } from "drizzle-orm";
import { users, tasks } from "./own/schema";
import { services, teams, deployments, serviceSummary } from "./devops/schema";
import { vm } from "./resources";
import { probes } from "./probes";

type Handler = (url: URL) => Promise<unknown>;

const routes: Record<string, Handler> = {
  async "/noop"() {
    return { ok: true };
  },

  async "/reset"() {
    const db = vm.user().db("app");
    await db.delete(tasks);
    await db.delete(users);
    const [ada, grace] = await db
      .insert(users)
      .values([
        { name: "Ada", email: "ada@example.com" },
        { name: "Grace", email: "grace@example.com" },
      ])
      .returning();
    await db.insert(tasks).values([
      { ownerId: ada.id, title: "write the spec", priority: 1 },
      { ownerId: ada.id, title: "review the spec", priority: 2 },
      { ownerId: grace.id, title: "ship it", priority: 1 },
    ]);
    return { ada: ada.id, grace: grace.id };
  },

  // select
  async "/tasks"(url) {
    const db = vm.user().db("app");
    const owner = url.searchParams.get("owner") ?? "";
    return db
      .select({ id: tasks.id, title: tasks.title, done: tasks.done })
      .from(tasks)
      .innerJoin(users, eq(users.id, tasks.ownerId))
      .where(and(eq(users.email, owner), eq(tasks.done, false)))
      .orderBy(tasks.priority);
  },

  // insert ... returning
  async "/add"(url) {
    const db = vm.user().db("app");
    const [owner] = await db.select().from(users).where(eq(users.email, "grace@example.com"));
    const [task] = await db
      .insert(tasks)
      .values({ ownerId: owner.id, title: url.searchParams.get("title") ?? "untitled", priority: 3 })
      .returning();
    return task;
  },

  // update ... returning
  async "/done"(url) {
    const db = vm.user().db("app");
    return db
      .update(tasks)
      .set({ done: true })
      .where(eq(tasks.title, url.searchParams.get("title") ?? ""))
      .returning({ id: tasks.id, done: tasks.done });
  },

  // join + aggregate
  async "/per-user"() {
    const db = vm.user().db("app");
    return db
      .select({ name: users.name, open: count(tasks.id) })
      .from(users)
      .leftJoin(tasks, and(eq(tasks.ownerId, users.id), eq(tasks.done, false)))
      .groupBy(users.name)
      .orderBy(users.name);
  },

  // relational query API (db.query)
  async "/users-with-tasks"() {
    const db = vm.user().db("app");
    return db.query.users.findMany({
      columns: { name: true, email: true },
      with: { tasks: { columns: { title: true, done: true }, orderBy: [tasks.priority] } },
      orderBy: [users.name],
    });
  },

  // an atomic multi-statement write
  async "/reassign"(url) {
    const db = vm.user().db("app");
    const fail = url.searchParams.get("fail") === "1";
    return db.transaction(async (tx) => {
      const [grace] = await tx.select().from(users).where(eq(users.email, "grace@example.com"));
      const moved = await tx
        .update(tasks)
        .set({ ownerId: grace.id })
        .where(eq(tasks.title, "write the spec"))
        .returning({ id: tasks.id });
      await tx.insert(tasks).values({ ownerId: grace.id, title: "reassigned note" });
      if (fail) throw new Error("something went wrong after two writes");
      return { moved: moved.length };
    });
  },

  // the shared database, read-only, through the pulled definition
  async "/devops/services"() {
    const devops = vm.user().db("devops");
    return devops
      .select({ service: services.name, team: teams.name, tier: services.tier, cost: services.monthlyCost })
      .from(services)
      .innerJoin(teams, eq(teams.id, services.teamId))
      .orderBy(services.name);
  },

  async "/devops/recent"() {
    const devops = vm.user().db("devops");
    const rows = await devops.query.services.findMany({
      columns: { name: true, tags: true, config: true, createdAt: true },
      with: { deployments: { orderBy: [desc(deployments.id)], limit: 1 } },
      orderBy: [services.name],
    });
    const summary = await devops.select().from(serviceSummary).orderBy(serviceSummary.name);
    return { rows: rows.map((r) => ({ ...r, deployments: r.deployments.map((d) => ({ ...d, id: String(d.id), idType: typeof d.id })) })), summary };
  },

  ...probes,
};

(globalThis as any).default = {
  async fetch(request: Request) {
    const url = new URL(request.url);
    const route = routes[url.pathname];
    if (!route) return Response.json({ error: "no route " + url.pathname }, { status: 404 });
    try {
      return Response.json(await route(url));
    } catch (e: any) {
      return Response.json(
        { error: String(e?.message ?? e), cause: e?.cause ? String(e.cause?.message ?? e.cause) : undefined },
        { status: 500 },
      );
    }
  },
};
