// A fake host: holds identity, Declared access (frozen at deploy) and Grants,
// and decides every operation. Sequential only: one request in flight at a time.
import { Refused, createContext, LEVEL } from "./sdk.mjs";

const grants = [
  { group: "sre", resource: "devops", level: "read" },
  { group: "facilities-team", resource: "facilities", level: "write" },
];
const groupsOf = { alice: ["sre"], bob: [] };

let declared = null; // fixed at deploy, not read from the running code
let inFlight = null; // { requestId, user } while a request runs
let nextId = 1;

function refuse(refusal, fix) {
  host.operationLog.push({ request: inFlight?.requestId ?? null, user: inFlight?.user?.id ?? null, outcome: "refused", ...refusal });
  throw new Refused(refusal, fix);
}

export const host = {
  operationLog: [],
  ownRows: [],

  deploy(manifest) {
    declared = structuredClone(manifest);
  },

  currentUser() {
    return inFlight?.user ?? null;
  },

  async serve(app, request, user) {
    if (declared.signIn === "required" && !user) return new Response(null, { status: 302, headers: { location: "/.vm/sign-in" } });
    inFlight = { requestId: `r${nextId++}`, user };
    try {
      return await app.fetch(request, createContext(inFlight.requestId));
    } catch (e) {
      if (!(e instanceof Refused)) return new Response(String(e), { status: 500 });
      if (e.refusal.reason === "no-user") return new Response(null, { status: 302, headers: { location: "/.vm/sign-in" } });
      return Response.json({ refusal: e.refusal, fix: e.fix }, { status: 403 });
    } finally {
      inFlight = null;
    }
  },

  async operate({ principal, resource, needed, statement }) {
    if (!inFlight) refuse({ reason: "no-request" }, "Resource operations must run while a request is being handled, not at module top level or after the response.");
    const { user } = inFlight;
    if (resource !== "(own)") {
      if (principal === "application") refuse({ reason: "not-own-database", resource }, "An Application acts as itself only in its own database. Use ctx.user().db(name).");
      if (!user) refuse({ reason: "no-user" }, "This request has no signed-in User.");
      const decl = declared.resources[resource];
      if (!decl) refuse({ reason: "not-declared", resource }, `Declare '${resource}' in manifest.ts.`);
      if (LEVEL[decl.access] < LEVEL[needed]) refuse({ reason: "declared-lower", resource, declared: decl.access, needed }, `Declare '${resource}' at '${needed}' in manifest.ts.`);
      const held = grants
        .filter((g) => g.resource === resource && groupsOf[user.id]?.includes(g.group))
        .map((g) => g.level)
        .sort((a, b) => LEVEL[b] - LEVEL[a])[0] ?? null;
      if (held === null || LEVEL[held] < LEVEL[needed])
        refuse({ reason: "no-grant", resource, needed, held }, `${user.email} has no Grant on '${resource}' at '${needed}'. A Platform administrator must grant it to one of their Groups.`);
    } else if (principal === "user" && !user) {
      refuse({ reason: "no-user" }, "This request has no signed-in User.");
    }
    host.operationLog.push({ request: inFlight.requestId, user: user?.id ?? null, as: principal, resource, statement, outcome: "ok" });
    if (resource === "(own)" && needed === "write") {
      host.ownRows.push({ statement, changedBy: { application: "incident-board", user: user?.id ?? null } });
      return { rowCount: 1 };
    }
    return [{ id: 1, title: "disk full on db-3", severity: "high", status: "open" }];
  },
};
