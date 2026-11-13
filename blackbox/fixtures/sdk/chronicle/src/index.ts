// The SDK fixture of reading the Change history (CONTRACT.md, "Reading the
// Change history"). Every route answers JSON that
// blackbox/tests/sdk_history_*.rs asserts on. Values are described as they
// were inside the Application (./describe.ts), so a BigInt, a Date and a
// JSON string survive the trip.
//
// `?as=user|application` picks the handle of the own database (`user` by
// default). Routes (GET):
//
//   /tasks/put?as=&id=&v=       insert a row of `tasks` with variant `v`'s
//                               values; answers `row`, select() of it after
//   /tasks/set?as=&id=&v=       update it to variant `v`; answers `row`
//   /tasks/bump?as=&id=&n=      `n` updates of its title, one statement each
//   /tasks/rekey?as=&id=&to=    update its primary key; answers `row`
//   /tasks/drop?as=&id=         delete it; answers `row` (null)
//   /lines/put|set|drop?as=&order=&line=&item=
//   /events/put?as=&what=&n=    a row of the table without a primary key
//   /events/set?as=&what=&n=    update `n` of the rows with this `what`
//   /archive/put|set?as=&id=&body=
//   /reshaped/set?as=&id=&added=&after=
//   /history?as=&table=&key=&undefinedKey=&options=
//                               the one host operation of this route:
//                               history(table, key, options), `key` and
//                               `options` given as JSON (`undefinedKey=1`
//                               passes `undefined` explicitly). Answers
//                               `changes`, each described, or `caught`
//   /shared/history?resource=   `history` forced past the types on a shared
//                               database's handle
//   /forged?as=&op=insert|update|delete
//                               a write through ./forged.ts (vm.history
//                               declared by the Application)
//   /exports                    the names @vm/sdk exports, those whose value
//                               is a Drizzle table, and the properties of
//                               each own handle that mention history
import * as vmSdk from "@vm/sdk";
import { defineApp } from "@vm/sdk";
import { Table, and, eq, is } from "drizzle-orm";
import manifest from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";
import { notes as sharedNotes } from "../resources/ops/schema.ts";
import { archivedNotes, events, orderLines, recast, reshaped, tasks } from "./db/schema.ts";
import { describe, describeError } from "./describe.ts";
import { forgedHistory } from "./forged.ts";

const json = (value: unknown, status = 200) => Response.json(value, { status });

/** The values a row of `tasks` is given, by variant. */
const variants = {
  a: {
    title: "first title",
    dueAt: new Date("2026-01-02T03:04:05.678Z"),
    amount: "12345678901234567890.012345678901234567890",
    meta: { a: [1, "x", null], b: "123" },
    tags: ["a,b", 'q"uote', "back\\slash", "{brace}", ""] as string[] | null,
    big: 9007199254740993n,
  },
  b: {
    title: "second title",
    dueAt: new Date("2027-06-30T23:59:59.001Z"),
    amount: "-0.000000000000000000001",
    meta: "123",
    tags: [] as string[] | null,
    big: -9007199254740995n,
  },
  c: {
    title: "second title",
    dueAt: new Date("2028-02-29T12:00:00.000Z"),
    amount: "-0.000000000000000000001",
    meta: { k: null },
    tags: [] as string[] | null,
    big: -9007199254740995n,
  },
  n: { title: "nulls", dueAt: null, amount: null, meta: null, tags: null as string[] | null, big: null },
};

type Variant = keyof typeof variants;

function variant(name: string | null): (typeof variants)[Variant] {
  if (name === "a" || name === "b" || name === "c" || name === "n") return variants[name];
  throw new Error(`no variant ${String(name)}`);
}

/** The tables `history` is asked about, by the name a test gives. */
const tables = { tasks, lines: orderLines, events, archive: archivedNotes, reshaped, recast } as const;

/** A change, each property described; `atMs` is `at`'s milliseconds since the epoch. */
function describeChange(change: Record<string, unknown>): Record<string, unknown> {
  const at = change.at;
  return {
    id: describe(change.id),
    at: describe(at),
    atMs: at instanceof Date ? at.getTime() : null,
    kind: describe(change.kind),
    old: describe(change.old),
    new: describe(change.new),
    changed: describe(change.changed),
    userId: describe(change.userId),
    application: describe(change.application),
    actor: describe(change.actor),
    request: describe(change.request),
    migration: describe(change.migration),
  };
}

/** The property names of `value` and its prototypes that mention history. */
function historyNames(value: object): string[] {
  const names = new Set<string>();
  for (let o: object | null = value; o !== null; o = Object.getPrototypeOf(o)) {
    for (const name of Object.getOwnPropertyNames(o)) if (/history/i.test(name)) names.add(name);
  }
  return [...names].sort();
}

function parsed(text: string | null): unknown {
  return text === null ? undefined : JSON.parse(text);
}

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const q = (name: string) => url.searchParams.get(name);
    const num = (name: string) => Number(q(name));
    const actor = q("as") === "application" ? "application" : "user";
    const own = () => (actor === "application" ? ctx.app().ownDb() : ctx.user().ownDb());
    const taskRow = async (id: number) => {
      const [row] = await own().select().from(tasks).where(eq(tasks.id, id));
      return { row: describe(row ?? null) };
    };

    switch (url.pathname) {
      case "/tasks/put": {
        await own().insert(tasks).values({ id: num("id"), ...variant(q("v")) });
        return json(await taskRow(num("id")));
      }
      case "/tasks/set": {
        await own().update(tasks).set(variant(q("v"))).where(eq(tasks.id, num("id")));
        return json(await taskRow(num("id")));
      }
      case "/tasks/bump": {
        for (let i = 0; i < num("n"); i += 1) {
          await own().update(tasks).set({ title: `bump ${i}` }).where(eq(tasks.id, num("id")));
        }
        return json(await taskRow(num("id")));
      }
      case "/tasks/rekey": {
        await own().update(tasks).set({ id: num("to") }).where(eq(tasks.id, num("id")));
        return json(await taskRow(num("to")));
      }
      case "/tasks/drop": {
        await own().delete(tasks).where(eq(tasks.id, num("id")));
        return json(await taskRow(num("id")));
      }

      case "/lines/put":
      case "/lines/set":
      case "/lines/drop": {
        const key = and(eq(orderLines.orderId, num("order")), eq(orderLines.line, num("line")));
        if (url.pathname === "/lines/put") {
          await own().insert(orderLines).values({ orderId: num("order"), line: num("line"), item: q("item") ?? "" });
        } else if (url.pathname === "/lines/set") {
          await own().update(orderLines).set({ item: q("item") ?? "" }).where(key);
        } else {
          await own().delete(orderLines).where(key);
        }
        const [row] = await own().select().from(orderLines).where(key);
        return json({ row: describe(row ?? null) });
      }

      case "/events/put": {
        await own().insert(events).values({ what: q("what") ?? "", n: num("n") });
        return json({ rows: describe(await own().select().from(events).where(eq(events.what, q("what") ?? ""))) });
      }
      case "/events/set": {
        await own().update(events).set({ n: num("n") }).where(eq(events.what, q("what") ?? ""));
        return json({ rows: describe(await own().select().from(events).where(eq(events.what, q("what") ?? ""))) });
      }

      case "/archive/put":
      case "/archive/set": {
        if (url.pathname === "/archive/put") {
          await own().insert(archivedNotes).values({ id: num("id"), body: q("body") ?? "" });
        } else {
          await own().update(archivedNotes).set({ body: q("body") ?? "" }).where(eq(archivedNotes.id, num("id")));
        }
        const [row] = await own().select().from(archivedNotes).where(eq(archivedNotes.id, num("id")));
        return json({ row: describe(row ?? null) });
      }

      case "/reshaped/set": {
        await own().update(reshaped).set({ added: q("added"), afterName: q("after") }).where(eq(reshaped.id, num("id")));
        const [row] = await own().select().from(reshaped).where(eq(reshaped.id, num("id")));
        return json({ row: describe(row ?? null) });
      }

      case "/history": {
        const name = q("table") ?? "tasks";
        const table = tables[name as keyof typeof tables];
        if (table === undefined) return json({ error: `no table ${name}` }, 404);
        const key = q("undefinedKey") === "1" ? undefined : parsed(q("key"));
        const options = parsed(q("options"));
        // The key and the options come from the test, so they are passed
        // past the types: the run-time checks are what is asserted.
        const handle = own() as unknown as {
          history(table: unknown, key?: unknown, options?: unknown): Promise<Record<string, unknown>[]>;
        };
        try {
          let changes: Record<string, unknown>[];
          if (options !== undefined) changes = await handle.history(table, key, options);
          else if (q("undefinedKey") === "1" || key !== undefined) changes = await handle.history(table, key);
          else changes = await handle.history(table);
          return json({ changes: changes.map(describeChange), isArray: Array.isArray(changes) });
        } catch (error) {
          return json({ caught: describeError(error) });
        }
      }

      case "/shared/history": {
        const handle = q("resource") === "devops" ? ctx.user().db("devops") : ctx.user().db("ops");
        const table = q("resource") === "devops" ? incidents : sharedNotes;
        try {
          // Forced past the types: a shared database's handle has no `history`.
          const forced = handle as unknown as { history(table: unknown, key?: unknown): Promise<unknown> };
          const answer = await forced.history(table, { id: 1 });
          return json({ resolved: describe(answer) });
        } catch (error) {
          return json({ caught: describeError(error) });
        }
      }

      case "/forged": {
        const op = q("op");
        try {
          if (op === "insert") {
            await own().insert(forgedHistory).values({
              at: new Date(),
              schemaName: "public",
              tableName: "tasks",
              kind: "insert",
              key: { id: 1 },
              new: { id: 1, title: "forged" },
              userId: "mallory",
              application: "sdk-chronicle",
              actor: "user",
              request: "forged-request",
            });
          } else if (op === "update") {
            await own().update(forgedHistory).set({ userId: "mallory" }).where(eq(forgedHistory.tableName, "tasks"));
          } else {
            await own().delete(forgedHistory).where(eq(forgedHistory.tableName, "tasks"));
          }
          return json({ resolved: true });
        } catch (error) {
          return json({ caught: describeError(error) });
        }
      }

      case "/exports": {
        const entries = Object.entries(vmSdk as Record<string, unknown>);
        return json({
          names: entries.map(([name]) => name).sort(),
          tables: entries.filter(([, value]) => is(value, Table)).map(([name]) => name),
          asUser: historyNames(ctx.user().ownDb()),
          asApplication: historyNames(ctx.app().ownDb()),
        });
      }

      default:
        return json({ error: `no route ${url.pathname}` }, 404);
    }
  },
});
