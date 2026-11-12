// The /types-nested query run with Drizzle's own node-postgres driver in Node,
// no host and no bridge: is the precision loss in nested relations Drizzle's
// or ours?   node kit/rqb-node-postgres.mjs <url>
import pg from "pg";
import { eq } from "drizzle-orm";
import { drizzle } from "drizzle-orm/node-postgres";
import * as schema from "../src/own/schema.ts";

const client = new pg.Client({ connectionString: process.argv[2] });
await client.connect();
const db = drizzle(client, { schema });
const [owner] = await db.select().from(schema.users).limit(1);
await db.delete(schema.tasks).where(eq(schema.tasks.title, "types-nodepg"));
await db.insert(schema.tasks).values({
  ownerId: owner.id,
  title: "types-nodepg",
  estimate: "12345678901234.123456",
  bigCount: 9007199254740993n,
  dueAtString: "2026-01-02 03:04:05.123456",
});
const rows = await db.query.users.findMany({
  with: {
    tasks: {
      columns: { estimate: true, bigCount: true, dueAtString: true },
      where: eq(schema.tasks.title, "types-nodepg"),
    },
  },
});
const t = rows.flatMap((r) => r.tasks)[0];
const [top] = await db
  .select({ estimate: schema.tasks.estimate, bigCount: schema.tasks.bigCount, dueAtString: schema.tasks.dueAtString })
  .from(schema.tasks)
  .where(eq(schema.tasks.title, "types-nodepg"));
const show = (o) => ({ estimate: o.estimate, bigCount: String(o.bigCount), dueAtString: o.dueAtString });
console.log(JSON.stringify({ nested: show(t), topLevel: show(top) }));
await client.end();
