// A fixture that uses its database handles after its request is over
// (CONTRACT.md, "What Application code is given": a handle kept and used
// after its request is over is refused by the host with `no-request`, or its
// code simply never runs).
//
// GET /after keeps both handles and answers at once; timers use them later
// and print what they got, each line starting with "LATE " and holding JSON
// (printing is the only way such code can tell anyone). GET /now uses the
// same handles while the request is in flight.
import { defineApp, Refused } from "@vm/sdk";
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
