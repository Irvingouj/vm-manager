// The SDK fixture of transactions (CONTRACT.md, "Transactions: Through the
// SDK"): `db.transaction(...)` on the own database as the User and as the
// Application, on a shared database declared at write (`ops`) and at read
// (`devops`). Every route answers JSON that blackbox/tests/sdk_transactions.rs
// asserts on, with `statements`: the SQL Drizzle built for each statement, in
// the order they were sent.
//
// Routes (GET; `?as=user|application` picks the actor on the own database,
// `user` by default; `marker` marks the rows written):
//
//   /commit?as=&marker=        two notes and a counter in one transaction,
//                              read back inside it and outside it, and
//                              vm.identity() at its start and its end; the
//                              callback's value is answered as `result`
//   /throw?as=&marker=         a note, then the callback throws its own Error
//   /rollback?as=&marker=      a note, then tx.rollback()
//   /hold?as=&marker=&ms=      a note, then the callback waits `ms` before
//                              it returns (the test reads meanwhile)
//   /count?as=&marker=         how many notes hold the marker, outside any
//                              transaction
//   /nested?as=&marker=        tx.transaction(...) forced past the types,
//                              inside a transaction that goes on
//   /aborted?as=&marker=       a note, then a duplicate key caught in the
//                              callback, which then returns
//   /shared/commit?marker=     two notes on `ops`, read back inside and outside
//   /shared/throw?marker=      a note on `ops`, then the callback throws
//   /shared/forced-write?marker=
//                              an insert through a transaction on `devops`
//                              (declared at read), forced past the types
//   /cascade/tx?as=&id=        db.transaction deleting `pa` row `id`, whose
//                              cascade runs code a migration left
//                              (migrations/0001_cascade.sql); answers
//                              { done } or { caught }
//   /cascade/write?as=&id=     the same delete outside a transaction
//   /cascade/honest?as=&id=    clears the action of `ca` row `id`
//   /settings?target=own|ops|devops&as=&iso=&mode=
//                              current_setting('transaction_isolation') and
//                              ('transaction_read_only') and current_user from
//                              inside a transaction begun with these options
import { defineApp, OperationFailed, Refused } from "@vm/sdk";
import { TransactionRollbackError, asc, eq, like, sql } from "drizzle-orm";
import manifest from "../manifest.ts";
import { incidents } from "../resources/devops/schema.ts";
import { notes as sharedNotes } from "../resources/ops/schema.ts";
import { ca, counters, notes, pa } from "./db/schema.ts";

/** A query Drizzle built, not yet sent. */
interface Built<T> extends PromiseLike<T> {
  toSQL(): { sql: string; params: unknown[] };
}

/** Sends Drizzle queries one after another, keeping the SQL of each. */
class Recorder {
  readonly statements: { sql: string }[] = [];

  async run<T>(query: Built<T>): Promise<T> {
    this.statements.push({ sql: query.toSQL().sql });
    return await query;
  }
}

/** What a caught error was. */
function describeError(error: unknown): Record<string, unknown> {
  const out: Record<string, unknown> = {
    isRefused: error instanceof Refused,
    isOperationFailed: error instanceof OperationFailed,
    isRollbackError: error instanceof TransactionRollbackError,
    isError: error instanceof Error,
  };
  if (error instanceof Error) {
    out.name = error.name;
    out.message = error.message;
  } else {
    out.thrown = String(error);
  }
  if (error instanceof Refused) {
    out.refusal = error.refusal;
    out.fix = error.fix;
  }
  if (error instanceof OperationFailed) {
    out.kind = error.kind;
    out.code = error.code ?? null;
    out.detail = error.detail;
  }
  return out;
}

const json = (value: unknown, status = 200) => Response.json(value, { status });

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

type Isolation = "read committed" | "repeatable read" | "serializable";
type AccessMode = "read only" | "read write";

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const actor = url.searchParams.get("as") === "application" ? "application" : "user";
    const own = () => (actor === "application" ? ctx.app().ownDb() : ctx.user().ownDb());
    const marker = url.searchParams.get("marker") ?? "ledger";
    const rec = new Recorder();

    switch (url.pathname) {
      case "/commit": {
        const result = await own().transaction(async (tx) => {
          const before = await rec.run(
            tx
              .select({
                userId: sql<string>`user_id`,
                application: sql<string>`application`,
                actor: sql<string>`actor`,
                request: sql<string>`request`,
              })
              .from(sql`vm.identity()`),
          );
          const [a] = await rec.run(tx.insert(notes).values({ body: `${marker}-a`, author: actor }).returning());
          const [b] = await rec.run(tx.insert(notes).values({ body: `${marker}-b`, author: actor }).returning());
          await rec.run(tx.update(counters).set({ n: sql`${counters.n} + 1` }).where(eq(counters.name, "hits")));
          const inside = await rec.run(tx.select().from(notes).where(like(notes.body, `${marker}-%`)).orderBy(asc(notes.id)));
          const outside = await rec.run(own().select().from(notes).where(like(notes.body, `${marker}-%`)));
          const after = await rec.run(
            tx
              .select({
                userId: sql<string>`user_id`,
                application: sql<string>`application`,
                actor: sql<string>`actor`,
                request: sql<string>`request`,
              })
              .from(sql`vm.identity()`),
          );
          return {
            ids: [a?.id ?? null, b?.id ?? null],
            inside: inside.map((n) => n.body),
            outside: outside.length,
            identities: [before, after],
          };
        });
        return json({ result, statements: rec.statements });
      }

      case "/throw": {
        const thrown = new Error(`thrown by the callback, ${marker}`);
        try {
          await own().transaction(async (tx) => {
            await rec.run(tx.insert(notes).values({ body: `${marker}-thrown`, author: actor }));
            throw thrown;
          });
          return json({ resolved: true, statements: rec.statements });
        } catch (error) {
          return json({ same: error === thrown, caught: describeError(error), statements: rec.statements });
        }
      }

      case "/rollback": {
        try {
          await own().transaction(async (tx) => {
            await rec.run(tx.insert(notes).values({ body: `${marker}-rolled`, author: actor }));
            tx.rollback();
          });
          return json({ resolved: true, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/hold": {
        const ms = Number(url.searchParams.get("ms") ?? "1500");
        const result = await own().transaction(async (tx) => {
          await rec.run(tx.insert(notes).values({ body: `${marker}-held`, author: actor }));
          await sleep(ms);
          return "held";
        });
        return json({ result, statements: rec.statements });
      }

      case "/count": {
        const rows = await own().select().from(notes).where(like(notes.body, `${marker}-%`));
        return json({ count: rows.length });
      }

      case "/nested": {
        const result = await own().transaction(async (tx) => {
          await rec.run(tx.insert(notes).values({ body: `${marker}-outer-1`, author: actor }));
          let nested: unknown;
          try {
            // Forced past the types: `tx` has no `transaction`.
            const forced = tx as unknown as {
              transaction(callback: (inner: unknown) => Promise<unknown>): Promise<unknown>;
            };
            await forced.transaction(async (inner) => {
              await (inner as typeof tx).insert(notes).values({ body: `${marker}-inner`, author: actor });
              return "inner";
            });
            nested = { resolved: true };
          } catch (error) {
            nested = describeError(error);
          }
          await rec.run(tx.insert(notes).values({ body: `${marker}-outer-2`, author: actor }));
          return nested;
        });
        return json({ result, statements: rec.statements });
      }

      case "/aborted": {
        let inner: unknown = null;
        try {
          const value = await own().transaction(async (tx) => {
            await rec.run(tx.insert(notes).values({ body: `${marker}-before`, author: actor }));
            try {
              await rec.run(tx.insert(counters).values({ name: "hits", n: 0 }));
            } catch (error) {
              inner = describeError(error);
            }
            return "returned after a failed statement";
          });
          return json({ resolved: true, value, inner, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), inner, statements: rec.statements });
        }
      }

      case "/shared/commit": {
        const ops = ctx.user().db("ops");
        const result = await ops.transaction(async (tx) => {
          await rec.run(tx.insert(sharedNotes).values({ body: `${marker}-a` }));
          await rec.run(tx.insert(sharedNotes).values({ body: `${marker}-b` }));
          const inside = await rec.run(
            tx.select({ body: sharedNotes.body, writtenBy: sharedNotes.writtenBy, labelUser: sharedNotes.labelUser, labelRequest: sharedNotes.labelRequest })
              .from(sharedNotes)
              .where(like(sharedNotes.body, `${marker}-%`))
              .orderBy(asc(sharedNotes.id)),
          );
          const outside = await rec.run(ops.select().from(sharedNotes).where(like(sharedNotes.body, `${marker}-%`)));
          return { inside, outside: outside.length };
        });
        return json({ result, statements: rec.statements });
      }

      case "/shared/throw": {
        const thrown = new Error(`thrown on ops, ${marker}`);
        try {
          await ctx.user().db("ops").transaction(async (tx) => {
            await rec.run(tx.insert(sharedNotes).values({ body: `${marker}-thrown` }));
            throw thrown;
          });
          return json({ resolved: true, statements: rec.statements });
        } catch (error) {
          return json({ same: error === thrown, caught: describeError(error), statements: rec.statements });
        }
      }

      case "/shared/forced-write": {
        try {
          await ctx.user().db("devops").transaction(async (tx) => {
            await rec.run(tx.select({ id: incidents.id }).from(incidents).limit(1));
            // Forced past the types: `devops` is declared at read.
            const forced = tx as unknown as {
              insert(table: typeof sharedNotes): { values(row: { body: string }): Built<unknown> };
            };
            await rec.run(forced.insert(sharedNotes).values({ body: `${marker}-forced` }));
          });
          return json({ resolved: true, statements: rec.statements });
        } catch (error) {
          return json({ caught: describeError(error), statements: rec.statements });
        }
      }

      case "/cascade/tx": {
        const id = Number(url.searchParams.get("id") ?? "1");
        try {
          await own().transaction(async (tx) => {
            await tx.delete(pa).where(eq(pa.id, id));
          });
          return json({ done: true });
        } catch (error) {
          return json({ caught: describeError(error) });
        }
      }

      case "/cascade/write": {
        const id = Number(url.searchParams.get("id") ?? "2");
        try {
          await own().delete(pa).where(eq(pa.id, id));
          return json({ done: true });
        } catch (error) {
          return json({ caught: describeError(error) });
        }
      }

      case "/cascade/honest": {
        const id = Number(url.searchParams.get("id") ?? "3");
        await own().update(ca).set({ action: null }).where(eq(ca.id, id));
        return json({ done: true });
      }

      case "/settings": {
        const iso = url.searchParams.get("iso");
        const mode = url.searchParams.get("mode");
        const config: { isolationLevel?: Isolation; accessMode?: AccessMode } = {};
        if (iso === "read committed" || iso === "repeatable read" || iso === "serializable") config.isolationLevel = iso;
        if (mode === "read only" || mode === "read write") config.accessMode = mode;
        const shape = {
          iso: sql<string>`current_setting('transaction_isolation')`,
          ro: sql<string>`current_setting('transaction_read_only')`,
          login: sql<string>`current_user::text`,
        };
        // One row, whatever the tables hold.
        const one = sql`(values (1)) as one (x)`;
        const target = url.searchParams.get("target") ?? "own";
        let rows: unknown;
        if (target === "ops") {
          rows = await ctx.user().db("ops").transaction(async (tx) => await rec.run(tx.select(shape).from(one)), config);
        } else if (target === "devops") {
          rows = await ctx.user().db("devops").transaction(async (tx) => await rec.run(tx.select(shape).from(one)), config);
        } else {
          rows = await own().transaction(async (tx) => await rec.run(tx.select(shape).from(one)), config);
        }
        return json({ rows, statements: rec.statements });
      }

      default:
        return json({ error: `no route ${url.pathname}` }, 404);
    }
  },
});
