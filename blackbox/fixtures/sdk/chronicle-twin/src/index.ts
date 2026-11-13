// A second Application with a table `tasks`, as sdk-chronicle has
// (blackbox/tests/sdk_history_boundaries.rs). Routes (GET, as the User):
//
//   /tasks/put?id=&title=   insert a row
//   /tasks/set?id=&title=   update its title
//   /history?id=            history(tasks, { id }), each change described
import { defineApp } from "@vm/sdk";
import { eq } from "drizzle-orm";
import manifest from "../manifest.ts";
import { tasks } from "./db/schema.ts";
import { describe, describeError } from "./describe.ts";

const json = (value: unknown, status = 200) => Response.json(value, { status });

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const id = Number(url.searchParams.get("id"));
    const title = url.searchParams.get("title") ?? "";
    const db = ctx.user().ownDb();
    switch (url.pathname) {
      case "/tasks/put":
        await db.insert(tasks).values({ id, title });
        return json({ done: true });
      case "/tasks/set":
        await db.update(tasks).set({ title }).where(eq(tasks.id, id));
        return json({ done: true });
      case "/history":
        try {
          const changes = await db.history(tasks, { id });
          return json({
            changes: changes.map((c) => ({
              kind: describe(c.kind),
              old: describe(c.old),
              new: describe(c.new),
              userId: describe(c.userId),
              application: describe(c.application),
              actor: describe(c.actor),
            })),
          });
        } catch (error) {
          return json({ caught: describeError(error) });
        }
      default:
        return json({ error: `no route ${url.pathname}` }, 404);
    }
  },
});
