// The SDK's main black-box fixture: an Application written with the SDK and
// Drizzle, as CONTRACT.md ("The SDK") lays one out. Every route answers JSON
// that blackbox/tests/sdk_*.rs assert on, including `statements`: the SQL and
// parameters Drizzle built for each statement, in the order they were sent,
// so that a test can match them with the Operation log.
//
// Routes (GET unless said; `?as=user|application` picks the actor on the own
// database, `user` by default):
//
//   /shared/select          a typed select on `devops` (declared at read)
//   /shared/join            a join on `devops`
//   /shared/relational      a relational query (db.query) on `devops`
//   /shared/with            a `with ... select` on `devops`
//   /shared/write?marker=   insert, update and delete on `ops` (declared at write)
//   /own?as=&marker=        insert, update, select, relational query, update
//                           of a counter, delete, on the own database
//   /values/own?as=         every value type written, read back, used as a
//                           parameter, updated and deleted on the own database
//   /values/shared          every value type read from `devops`, and used as
//                           a parameter there
//   /refuse/no-grant        a select on `devops` (the test sends a User with no Grant)
//   /refuse/not-declared    `payroll`, forced past the types
//   /refuse/declared-lower  an insert through `devops`, forced past the types
//   /refuse/not-own-database ctx.app().db("devops"), forced past the types
//   /refuse/escape          a refused select on `devops`, not caught
//   /refuse/construct       new Refused(...) at run time
//   /fail/unique?as=&tag=   the same tag inserted twice
//   /fail/statement?as=     an update whose `sql` fragment the statement check refuses
//   /fail/limit?as=         a select of more rows than the limit
//   /fail/escape?tag=       the unique violation, not caught
//   /throw                  an ordinary Error, not caught
import { defineApp, Refused } from "@vm/sdk";
import { asc, eq, inArray, sql } from "drizzle-orm";
import manifest from "../manifest.ts";
import { incidents, services, teams, vals as sharedVals } from "../resources/devops/schema.ts";
import { notes as sharedNotes } from "../resources/ops/schema.ts";
import { counters, notes, tags, vals } from "./db/schema.ts";
import { type Described, describe, describeError, describeRows } from "./describe.ts";

/** A query Drizzle built, not yet sent. */
interface Built<T> extends PromiseLike<T> {
  toSQL(): { sql: string; params: unknown[] };
}

/** Sends Drizzle queries one after another, keeping the SQL of each. */
class Recorder {
  readonly statements: { sql: string; params: Described[] }[] = [];

  async run<T>(query: Built<T>): Promise<T> {
    const { sql: text, params } = query.toSQL();
    this.statements.push({ sql: text, params: params.map(describe) });
    return await query;
  }
}

// Drizzle types an array column's elements as not null (`string[]`); the
// contract keeps a null element intact, so the arrays below hold nulls and
// are cast to the element type Drizzle declares.

/** The values the Application writes, one of each type. */
const WRITTEN = {
  label: "written",
  i4: 2147483647,
  i2: -32768,
  big: 9007199254740993n,
  bigNum: 9007199254740991,
  num: "12345678901234567890.012345678901234567890",
  r4: 1.5,
  f8: 3.141592653589793,
  flag: true,
  t: 'héllo ☃ "quoted" \\ back',
  vc: "varchar value",
  u: "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
  ts: new Date("2026-01-02T03:04:05.678Z"),
  tstz: new Date("2026-01-02T03:04:05.678Z"),
  tsStr: "2026-01-02 03:04:05.678412",
  // As the own database prints it: the tests set its TimeZone to America/St_Johns.
  tstzStr: "2026-01-01 23:34:05.678412-03:30",
  d: "2026-01-02",
  j: { a: [1, "x", null], b: "123" },
  jb: "123",
  texts: ["a,b", 'q"uote', "back\\slash", "{brace}", "", null, "NULL", " spaced "] as string[],
  ints: [1, null, -3] as number[],
};

/** What the update sets on the written row. */
const UPDATED = {
  big: -9223372036854775808n,
  bigNum: -9007199254740991,
  num: "-0.000000000000000000001000",
  tstz: new Date("2030-06-07T08:09:10.123Z"),
  jb: ["x", "123", null, { k: null }],
  texts: [",", '"', "\\", "{}", "", null] as string[],
};

const json = (value: unknown, status = 200) => Response.json(value, { status });

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const actor = url.searchParams.get("as") === "application" ? "application" : "user";
    const own = () => (actor === "application" ? ctx.app().ownDb() : ctx.user().ownDb());
    const rec = new Recorder();
    // A handle with every Drizzle method, for the casts that force a call
    // past the types.
    type OwnHandle = ReturnType<ReturnType<typeof ctx.user>["ownDb"]>;

    switch (url.pathname) {
      case "/shared/select": {
        const devops = ctx.user().db("devops");
        const rows = await rec.run(
          devops
            .select({ id: incidents.id, title: incidents.title, severity: incidents.severity })
            .from(incidents)
            .where(eq(incidents.open, true))
            .orderBy(asc(incidents.id)),
        );
        return json({ rows: describeRows(rows), statements: rec.statements });
      }

      case "/shared/join": {
        const devops = ctx.user().db("devops");
        const rows = await rec.run(
          devops
            .select({ service: services.name, team: teams.name, tier: services.tier })
            .from(services)
            .innerJoin(teams, eq(teams.id, services.teamId))
            .where(eq(teams.name, "Payments"))
            .orderBy(asc(services.id)),
        );
        return json({ rows: describeRows(rows), statements: rec.statements });
      }

      case "/shared/relational": {
        const devops = ctx.user().db("devops");
        const rows = await rec.run(
          devops.query.services.findMany({
            columns: { id: true, name: true },
            with: { team: { columns: { name: true } } },
            orderBy: [asc(services.id)],
          }),
        );
        return json({ rows: describeRows(rows), statements: rec.statements });
      }

      case "/shared/with": {
        const devops = ctx.user().db("devops");
        const open = devops
          .$with("open_ones")
          .as(devops.select({ id: incidents.id, severity: incidents.severity }).from(incidents).where(eq(incidents.open, true)));
        const rows = await rec.run(devops.with(open).select().from(open).orderBy(asc(open.id)));
        return json({ rows: describeRows(rows), statements: rec.statements });
      }

      case "/shared/write": {
        const ops = ctx.user().db("ops");
        const marker = url.searchParams.get("marker") ?? "shared-write";
        const [inserted] = await rec.run(
          ops.insert(sharedNotes).values({ body: `${marker}-inserted` }).returning({ id: sharedNotes.id, body: sharedNotes.body, writtenBy: sharedNotes.writtenBy }),
        );
        if (inserted === undefined) return json({ error: "nothing inserted" }, 500);
        const updated = await rec.run(
          ops.update(sharedNotes).set({ body: `${marker}-updated` }).where(eq(sharedNotes.id, inserted.id)).returning({ id: sharedNotes.id, body: sharedNotes.body }),
        );
        const deleted = await rec.run(
          ops.delete(sharedNotes).where(eq(sharedNotes.id, inserted.id)).returning({ id: sharedNotes.id }),
        );
        return json({
          inserted: describe(inserted),
          updated: describeRows(updated),
          deleted: describeRows(deleted),
          statements: rec.statements,
        });
      }

      case "/own": {
        const db = own();
        const marker = url.searchParams.get("marker") ?? "own";
        const [inserted] = await rec.run(db.insert(notes).values({ body: `${marker}-inserted`, author: actor }).returning());
        if (inserted === undefined) return json({ error: "nothing inserted" }, 500);
        const updated = await rec.run(
          db.update(notes).set({ body: `${marker}-updated` }).where(eq(notes.id, inserted.id)).returning(),
        );
        const selected = await rec.run(db.select().from(notes).where(eq(notes.id, inserted.id)));
        const relational = await rec.run(db.query.notes.findMany({ where: eq(notes.id, inserted.id) }));
        const counted = await rec.run(
          db.update(counters).set({ n: sql`${counters.n} + 1` }).where(eq(counters.name, "hits")).returning(),
        );
        const deleted = await rec.run(db.delete(notes).where(eq(notes.id, inserted.id)).returning({ id: notes.id }));
        return json({
          inserted: describe(inserted),
          updated: describeRows(updated),
          selected: describeRows(selected),
          relational: describeRows(relational),
          counted: describeRows(counted),
          deleted: describeRows(deleted),
          statements: rec.statements,
        });
      }

      case "/values/own": {
        const db = own();
        const seeded = await rec.run(
          db.select().from(vals).where(inArray(vals.label, ["every", "nulls", "json-number", "json-bool", "json-null", "json-nested"])).orderBy(asc(vals.id)),
        );
        const inserted = await rec.run(db.insert(vals).values(WRITTEN).returning());
        const emptyArrays = await rec.run(
          db.insert(vals).values({ label: "written-empty", texts: [], ints: [], jb: [], j: {} }).returning(),
        );
        const writtenNulls = await rec.run(db.insert(vals).values({ label: "written-nulls" }).returning());
        const readBack = await rec.run(
          db.select().from(vals).where(inArray(vals.label, ["written", "written-empty", "written-nulls"])).orderBy(asc(vals.id)),
        );
        // Each value of WRITTEN as a parameter in `where`: which rows match.
        const matching = async (column: string, condition: ReturnType<typeof eq>) => {
          const rows = await rec.run(db.select({ label: vals.label }).from(vals).where(condition).orderBy(asc(vals.id)));
          return [column, rows.map((r) => r.label)] as const;
        };
        const where = Object.fromEntries(
          await Promise.all([
            matching("i4", eq(vals.i4, WRITTEN.i4)),
            matching("i2", eq(vals.i2, WRITTEN.i2)),
            matching("big", eq(vals.big, WRITTEN.big)),
            matching("bigNum", eq(vals.bigNum, WRITTEN.bigNum)),
            matching("num", eq(vals.num, WRITTEN.num)),
            matching("r4", eq(vals.r4, WRITTEN.r4)),
            matching("f8", eq(vals.f8, WRITTEN.f8)),
            matching("flag", eq(vals.flag, WRITTEN.flag)),
            matching("t", eq(vals.t, WRITTEN.t)),
            matching("vc", eq(vals.vc, WRITTEN.vc)),
            matching("u", eq(vals.u, WRITTEN.u)),
            matching("ts", eq(vals.ts, WRITTEN.ts)),
            matching("tstz", eq(vals.tstz, WRITTEN.tstz)),
            matching("tsStr", eq(vals.tsStr, WRITTEN.tsStr)),
            matching("tstzStr", eq(vals.tstzStr, WRITTEN.tstzStr)),
            matching("d", eq(vals.d, WRITTEN.d)),
            matching("jb", eq(vals.jb, WRITTEN.jb)),
            matching("texts", eq(vals.texts, WRITTEN.texts)),
            matching("ints", eq(vals.ints, WRITTEN.ints)),
          ]),
        );
        const updated = await rec.run(
          db
            .update(vals)
            .set({ ...UPDATED, ints: [] })
            .where(eq(vals.label, "written"))
            .returning(),
        );
        const deleted = await rec.run(db.delete(vals).where(eq(vals.label, "written")).returning());
        return json({
          seeded: describeRows(seeded),
          inserted: describeRows(inserted),
          emptyArrays: describeRows(emptyArrays),
          writtenNulls: describeRows(writtenNulls),
          readBack: describeRows(readBack),
          where,
          updated: describeRows(updated),
          deleted: describeRows(deleted),
          statements: rec.statements,
        });
      }

      case "/values/shared": {
        const devops = ctx.user().db("devops");
        const rows = await rec.run(devops.select().from(sharedVals).orderBy(asc(sharedVals.id)));
        const matching = async (column: string, condition: ReturnType<typeof eq>) => {
          const found = await rec.run(
            devops.select({ label: sharedVals.label }).from(sharedVals).where(condition).orderBy(asc(sharedVals.id)),
          );
          return [column, found.map((r) => r.label)] as const;
        };
        const where = Object.fromEntries(
          await Promise.all([
            matching("big", eq(sharedVals.big, WRITTEN.big)),
            matching("bigNum", eq(sharedVals.bigNum, WRITTEN.bigNum)),
            matching("num", eq(sharedVals.num, WRITTEN.num)),
            matching("tsStr", eq(sharedVals.tsStr, WRITTEN.tsStr)),
            matching("tstzStr", eq(sharedVals.tstzStr, "2026-01-02 08:34:05.678412+05:30")),
            matching("d", eq(sharedVals.d, WRITTEN.d)),
            matching("jb", eq(sharedVals.jb, WRITTEN.jb)),
            matching("texts", eq(sharedVals.texts, WRITTEN.texts)),
            matching("ints", eq(sharedVals.ints, WRITTEN.ints)),
            matching("u", eq(sharedVals.u, WRITTEN.u)),
            matching("t", eq(sharedVals.t, WRITTEN.t)),
          ]),
        );
        return json({ rows: describeRows(rows), where, statements: rec.statements });
      }

      case "/refuse/no-grant": {
        try {
          await rec.run(ctx.user().db("devops").select().from(incidents));
          return json({ ran: true, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/refuse/not-declared": {
        try {
          // Forced past the types: `payroll` is not in manifest.ts.
          const forced = ctx.user() as unknown as { db(name: string): OwnHandle };
          const payroll = forced.db("payroll");
          await rec.run(payroll.select().from(incidents));
          return json({ ran: true, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/refuse/declared-lower": {
        try {
          // Forced past the types: `devops` is declared at read.
          const forced = ctx.user().db("devops") as unknown as OwnHandle;
          await rec.run(forced.insert(sharedNotes).values({ body: "forced through a read declaration" }));
          return json({ ran: true, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/refuse/not-own-database": {
        try {
          // Forced past the types: the Application acts as itself only on its own database.
          const forced = ctx.app() as unknown as { db(name: string): OwnHandle };
          await rec.run(forced.db("devops").select().from(incidents));
          return json({ ran: true, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/refuse/escape": {
        const rows = await ctx.user().db("devops").select().from(incidents);
        return json({ ran: true, rows: rows.length });
      }

      case "/refuse/construct": {
        try {
          const Forced = Refused as unknown as new (refusal: unknown, fix: string) => Refused;
          const made = new Forced({ reason: "no-grant", resource: "devops" }, "Made up by Application code.");
          return json({ constructed: true, made: describeError(made) });
        } catch (error) {
          return json({ constructed: false, caught: describeError(error) });
        }
      }

      case "/fail/unique": {
        const tag = url.searchParams.get("tag") ?? "duplicate";
        const db = own();
        await rec.run(db.insert(tags).values({ name: tag }));
        try {
          await rec.run(db.insert(tags).values({ name: tag }));
          return json({ ran: true, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/fail/statement": {
        try {
          await rec.run(
            own()
              .update(counters)
              .set({ n: sql`length(set_config('vm.sneaky', 'x', true))` })
              .where(eq(counters.name, "hits")),
          );
          return json({ ran: true, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/fail/limit": {
        try {
          const rows = await rec.run(own().select({ n: sql<number>`g` }).from(sql`generate_series(1, 20000) as g`));
          return json({ ran: true, rows: rows.length, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/fail/escape": {
        const tag = url.searchParams.get("tag") ?? "escaping";
        const db = own();
        await db.insert(tags).values({ name: tag });
        await db.insert(tags).values({ name: tag });
        return json({ ran: true });
      }

      case "/throw":
        throw new Error("an ordinary error, marker-uncaught-7f3a");

      default:
        return json({ error: `no route ${url.pathname}` }, 404);
    }
  },
});
