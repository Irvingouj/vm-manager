// A fixture that uses its database handles when no request is in flight
// (CONTRACT.md, "What Application code is given": a handle used at the top
// level, or after the response, is refused by the host with `no-request`).
//
// At the top level. Application code has no context while its script
// loads, so this one takes the only road there is: it watches for the
// bundle's own assignment of `globalThis.default` and, at that moment, still
// inside the script's top level, calls the handler it was given with a
// made-up platform context. The handler (route /at-load) then uses
// ctx.user().ownDb() and ctx.app().ownDb(). What those calls got is kept and
// reported by GET /top.
//
// After the response. GET /after keeps both handles and answers at once;
// timers use them later and print what they got, each line starting with
// "LATE " and holding JSON (printing is the only way such code can tell
// anyone).
import { defineApp, Refused } from "@vm/sdk";
import { asc } from "drizzle-orm";
import manifest from "../manifest.ts";
import { notes } from "./db/schema.ts";

type Outcome = { ran: true; rows: number } | { caught: Record<string, unknown> };

async function attempt(use: () => PromiseLike<unknown[]>): Promise<Outcome> {
  try {
    const rows = await use();
    return { ran: true, rows: rows.length };
  } catch (error) {
    return { caught: describeError(error) };
  }
}

function describeError(error: unknown): Record<string, unknown> {
  const out: Record<string, unknown> = {
    isRefused: error instanceof Refused,
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
  return out;
}

const app = defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    switch (url.pathname) {
      case "/at-load": {
        const asUser = await attempt(() => ctx.user().ownDb().select().from(notes).orderBy(asc(notes.id)));
        const asApplication = await attempt(() => ctx.app().ownDb().select().from(notes).orderBy(asc(notes.id)));
        return Response.json({ asUser, asApplication });
      }

      case "/top":
        return Response.json({ atLoad: await atLoad });

      case "/after": {
        const asUser = ctx.user().ownDb();
        const asApplication = ctx.app().ownDb();
        for (const ms of [0, 50, 300]) {
          setTimeout(async () => {
            const user = await attempt(() => asUser.select().from(notes));
            const application = await attempt(() => asApplication.insert(notes).values({ body: `late ${ms}` }).returning());
            console.log(`LATE ${JSON.stringify({ ms, asUser: user, asApplication: application })}`);
          }, ms);
        }
        return Response.json({ left: true });
      }

      case "/now": {
        // The same handles while the request is in flight: allowed.
        const asUser = await attempt(() => ctx.user().ownDb().select().from(notes));
        const asApplication = await attempt(() => ctx.app().ownDb().select().from(notes));
        return Response.json({ asUser, asApplication });
      }

      default:
        return Response.json({ error: `no route ${url.pathname}` }, { status: 404 });
    }
  },
});

export default app;

// What the top-level attempt got, or a note that it never happened.
let atLoad: Promise<unknown> = Promise.resolve({ note: "globalThis.default was never assigned" });

type Platform = { fetch(request: Request, ctx: unknown): Promise<Response> | Response };
let installed: unknown;
Object.defineProperty(globalThis, "default", {
  configurable: true,
  enumerable: true,
  get() {
    return installed;
  },
  set(value: unknown) {
    installed = value;
    const platform = value as Platform;
    // Still the script's top level: no request is in flight.
    const madeUp = Object.freeze({
      user: () => Object.freeze({ id: "made-up-at-load", groups: Object.freeze([]) }),
    });
    try {
      atLoad = Promise.resolve(platform.fetch(new Request("http://sdk-leftover.vm.test/at-load"), madeUp)).then(
        (response) => response.json(),
        (error: unknown) => ({ rejected: describeError(error) }),
      );
    } catch (error) {
      atLoad = Promise.resolve({ threw: describeError(error) });
    }
  },
});
