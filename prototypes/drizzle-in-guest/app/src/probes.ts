// Routes that measure or poke at the bridge; not what an Application writes.
import { count, eq, sql } from "drizzle-orm";
import { tasks, users } from "./own/schema";
import { services } from "./devops/schema";
import { database } from "./vm";
import { vm } from "./resources";
import * as own from "./own/schema";

declare const env: { HOST: { invoke(name: string, args: unknown): Promise<any> } };

const settle = async (f: () => Promise<unknown>) => {
  try {
    return { ok: await f() };
  } catch (e: any) {
    return { err: String(e?.message ?? e), cause: e?.cause ? String(e.cause?.message ?? e.cause) : undefined };
  }
};

// What a value is in the guest, in a JSON-safe form.
function describe(v: unknown) {
  const type = v === null ? "null" : typeof v;
  const ctor = v !== null && typeof v === "object" ? (v as object).constructor?.name : undefined;
  let value: unknown = v;
  if (typeof v === "bigint") value = v.toString();
  else if (v instanceof Date) value = isNaN(v.getTime()) ? "Invalid Date" : v.toISOString();
  else if (v instanceof Uint8Array) value = Array.from(v);
  return { type, ctor, value };
}

const sample = {
  title: "types",
  note: null,
  estimate: "12345678901234.123456",
  bigCount: 9007199254740993n,
  smallCount: 42,
  priority: -7,
  dueAt: new Date("2026-01-02T03:04:05.678Z"),
  dueAtTz: new Date("2026-01-02T03:04:05.678Z"),
  dueAtString: "2026-01-02 03:04:05.123456",
  dueDate: "2026-01-02",
  meta: { labels: ["a", "b"], weight: 1.5 },
  metaJson: { nested: { deep: [1, "two", null] } },
  tags: ["plain", "with,comma", 'with"quote', "with\\backslash", "with space", ""],
  scores: [1, -2, 2147483647],
  blob: new Uint8Array([0, 1, 127, 128, 255]),
};

export const probes: Record<string, (url: URL) => Promise<unknown>> = {
  // The same query as /select-drizzle without Drizzle: one host call.
  async "/select-raw"() {
    const { rows } = await env.HOST.invoke("sql", {
      db: "app",
      sql: 'select "id", "title" from "tasks" where "tasks"."priority" = $1',
      params: [1],
      method: "all",
    });
    return rows;
  },
  async "/select-drizzle"() {
    const db = vm.user().db("app");
    return db.select({ id: tasks.id, title: tasks.title }).from(tasks).where(eq(tasks.priority, 1));
  },

  // Type fidelity: write one row of every type, read it back two ways.
  async "/types"() {
    const db = vm.user().db("app");
    const [owner] = await db.select().from(users).limit(1);
    const [inserted] = await db
      .insert(tasks)
      .values({ ...sample, ownerId: owner.id })
      .returning({ id: tasks.id });
    const [row] = await db.select().from(tasks).where(eq(tasks.id, inserted.id));
    const [withUser] = await db
      .select({ id: users.id, createdAt: users.createdAt })
      .from(users)
      .where(eq(users.id, owner.id));
    const raw = await env.HOST.invoke("sql", {
      db: "app",
      sql: "select * from tasks where id = $1",
      params: [inserted.id],
      method: "execute",
    });
    const fields: Record<string, unknown> = {};
    for (const [k, v] of Object.entries({ ...row, userId: withUser.id, userCreatedAt: withUser.createdAt })) {
      fields[k] = describe(v);
    }
    const expectSame: Record<string, boolean> = {
      estimate: row.estimate === sample.estimate,
      bigCount: row.bigCount === sample.bigCount,
      smallCount: row.smallCount === sample.smallCount,
      dueAt: row.dueAt?.getTime() === sample.dueAt.getTime(),
      dueAtTz: row.dueAtTz?.getTime() === sample.dueAtTz.getTime(),
      dueAtString: row.dueAtString === sample.dueAtString,
      dueDate: row.dueDate === sample.dueDate,
      meta: JSON.stringify(row.meta) === JSON.stringify(sample.meta),
      metaJson: JSON.stringify(row.metaJson) === JSON.stringify(sample.metaJson),
      tags: JSON.stringify(row.tags) === JSON.stringify(sample.tags),
      scores: JSON.stringify(row.scores) === JSON.stringify(sample.scores),
      blob: JSON.stringify(Array.from(row.blob ?? [])) === JSON.stringify(Array.from(sample.blob)),
      note: row.note === null,
      createdAtIsDate: withUser.createdAt instanceof Date && !isNaN(withUser.createdAt.getTime()),
    };
    return { fields, expectSame, rawFromHost: raw.rows[0] };
  },

  // The same row through the relational API: nested rows come back as
  // Postgres JSON (json_build_array), not as columns.
  async "/types-nested"() {
    const db = vm.user().db("app");
    const rows = await db.query.users.findMany({
      with: {
        tasks: {
          columns: { estimate: true, bigCount: true, dueAt: true, dueAtString: true, blob: true, tags: true },
          where: eq(tasks.title, "types"),
        },
      },
    });
    const t = rows.flatMap((r) => r.tasks)[0];
    if (!t) return { error: "run /types first" };
    const fields: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(t)) fields[k] = describe(v);
    return {
      fields,
      expectSame: {
        estimate: t.estimate === sample.estimate,
        bigCount: t.bigCount === sample.bigCount,
        dueAt: t.dueAt?.getTime() === sample.dueAt.getTime(),
        dueAtString: t.dueAtString === sample.dueAtString,
        blob: JSON.stringify(Array.from((t.blob as Uint8Array | null) ?? [])) === JSON.stringify(Array.from(sample.blob)),
        tags: JSON.stringify(t.tags) === JSON.stringify(sample.tags),
      },
    };
  },

  // Session state set through a SELECT outlives the request on a shared connection.
  async "/appname"() {
    const db = vm.user().db("app");
    return db.execute(sql`select current_setting('application_name') as v`);
  },

  // Transactions
  async "/count"() {
    const db = vm.user().db("app");
    const [{ n }] = await db.select({ n: count() }).from(tasks);
    return { n };
  },
  async "/tx-commit"() {
    const db = vm.user().db("app");
    const [owner] = await db.select().from(users).limit(1);
    return db.transaction(async (tx) => {
      await tx.insert(tasks).values({ ownerId: owner.id, title: "tx-a" });
      await tx.insert(tasks).values({ ownerId: owner.id, title: "tx-b" });
      const [{ n }] = await tx.select({ n: count() }).from(tasks).where(sql`${tasks.title} like 'tx-%'`);
      return { insideTx: n };
    });
  },
  async "/tx-throw"() {
    const db = vm.user().db("app");
    const [owner] = await db.select().from(users).limit(1);
    return settle(() =>
      db.transaction(async (tx) => {
        await tx.insert(tasks).values({ ownerId: owner.id, title: "tx-throw-1" });
        await tx.insert(tasks).values({ ownerId: owner.id, title: "tx-throw-2" });
        throw new Error("application error after two inserts");
      }),
    );
  },
  async "/tx-rollback-call"() {
    const db = vm.user().db("app");
    const [owner] = await db.select().from(users).limit(1);
    return settle(() =>
      db.transaction(async (tx) => {
        await tx.insert(tasks).values({ ownerId: owner.id, title: "tx-rb" });
        tx.rollback();
      }),
    );
  },
  async "/tx-db-error"() {
    const db = vm.user().db("app");
    const [owner] = await db.select().from(users).limit(1);
    return settle(() =>
      db.transaction(async (tx) => {
        await tx.insert(tasks).values({ ownerId: owner.id, title: "tx-dberr" });
        // violates the foreign key: the second statement fails in Postgres
        await tx.insert(tasks).values({ ownerId: "00000000-0000-0000-0000-000000000000", title: "tx-dberr-2" });
      }),
    );
  },
  async "/tx-config"() {
    const db = vm.user().db("app");
    return settle(() =>
      db.transaction(
        async (tx) => {
          const [r] = await tx.execute<{ iso: string; ro: string }>(
            sql`select current_setting('transaction_isolation') as iso, current_setting('transaction_read_only') as ro`,
          );
          const write = await settle(() => tx.update(tasks).set({ done: true }));
          return { ...r, write };
        },
        { isolationLevel: "serializable", accessMode: "read only" },
      ),
    );
  },
  async "/tx-nested"() {
    const db = vm.user().db("app");
    return settle(() => db.transaction(async (tx) => tx.transaction(async () => 1)));
  },
  // A transaction the request never finishes: the host must roll it back
  // when the request ends.
  async "/tx-abandon"() {
    const db = vm.user().db("app");
    const [owner] = await db.select().from(users).limit(1);
    let inserted = false;
    void db.transaction(async (tx) => {
      await tx.insert(tasks).values({ ownerId: owner.id, title: "tx-abandoned" });
      inserted = true;
      await new Promise(() => {});
    });
    while (!inserted) await new Promise((r) => setTimeout(r, 5));
    return { leftOpen: true };
  },
  async "/tx-titles"() {
    const db = vm.user().db("app");
    const rows = await db.select({ title: tasks.title }).from(tasks).where(sql`${tasks.title} like 'tx-%'`);
    return rows.map((r) => r.title).sort();
  },

  // Raw SQL reachable from Application code
  async "/raw"() {
    const db = vm.user().db("app");
    const devops = vm.user().db("devops");
    const ownDb = database("app", own);
    const dataModifyingCte = devops
      .$with("gone")
      .as(ownDb.delete(services).where(eq(services.name, "gateway")).returning({ name: services.name }));
    return {
      execute: await settle(() => db.execute(sql`select current_user as who, 1 + ${41} as answer`)),
      rawInWhere: await settle(() =>
        db.select({ id: tasks.id }).from(tasks).where(sql`${tasks.id} > 0 or ${"x"} = 'x'`).limit(1),
      ),
      multiStatement: await settle(() => db.execute(sql.raw("select 1; select 2"))),
      setRole: await settle(() => db.execute(sql`set role postgres`)),
      setConfigInSelect: await settle(() =>
        db.execute(sql`select set_config('application_name', 'from-the-guest', false) as v`),
      ),
      ddl: await settle(() => db.execute(sql`create table evil (id int)`)),
      // read-only Resource: the types hide execute, the object still has it
      devopsExecuteDelete: await settle(() => (devops as any).execute(sql`delete from deployments`)),
      // read-only types, a write still built: a data-modifying CTE in a SELECT
      devopsDeleteInCte: await settle(() => devops.with(dataModifyingCte).select().from(dataModifyingCte)),
      appNameAfter: await settle(() => db.execute(sql`select current_setting('application_name') as v`)),
    };
  },
};

// Type-level checks (compiled by `tsc --noEmit`, never run).
export function typeChecks() {
  const devops = vm.user().db("devops");
  const app = vm.user().db("app");
  // @ts-expect-error a read-only Resource has no insert
  devops.insert(services);
  // @ts-expect-error ... no update
  devops.update(services);
  // @ts-expect-error ... no delete
  devops.delete(services);
  // @ts-expect-error ... no execute
  devops.execute(sql`select 1`);
  // @ts-expect-error ... no transaction
  devops.transaction(async () => {});
  // The own database has all of them.
  app.insert(tasks);
  app.execute(sql`select 1`);
  // @ts-expect-error a column that does not exist
  app.select({ x: tasks.nope }).from(tasks);
  // @ts-expect-error a Resource not declared
  vm.user().db("payroll");
}
