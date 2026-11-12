// Fake runtime for candidate B, so its sample handlers execute under Node.
// The SDK objects hold no identity: every operation asks the fake host who is
// in flight *now*, as the real host does (docs/research/identity-pass-through.md).
import { host } from "./fake-host.mjs";

export class Refused extends Error {
  name = "Refused";
  constructor(refusal, fix) {
    super(`Refused (${refusal.reason}): ${fix}`);
    this.refusal = refusal;
    this.fix = fix;
  }
}

export const defineManifest = (manifest) => manifest;
export const defineApp = (manifest, handlers) => ({ ...handlers, manifest });

const LEVEL = { structure: 0, read: 1, write: 2 };

function database(name, principal) {
  const op = (needed, statement) => host.operate({ principal, resource: name, needed, statement });
  return {
    name,
    select: (table) => op("read", `select * from ${table}`),
    query: (sql) => op(/^\s*(insert|update|delete)/i.test(sql.text) ? "write" : "read", sql.text),
    insert: (table, row) => op("write", `insert into ${table} ${JSON.stringify(row)}`),
    update: (table) => op("write", `update ${table}`),
  };
}

function actingAsUser(user) {
  return {
    id: user.id,
    email: user.email,
    displayName: user.displayName,
    db: (name) => database(name, "user"),
    ownDb: () => database("(own)", "user"),
  };
}

/** Created by the host for each request. It holds no identity: user() asks the host at call time. */
export function createContext(requestId) {
  return {
    requestId,
    user: () => {
      const user = host.currentUser();
      return user ? actingAsUser(user) : null;
    },
    requireUser: () => {
      const user = host.currentUser();
      if (!user) throw new Refused({ reason: "no-user" }, "This request has no signed-in User. Use ctx.user() and handle null, or let the host redirect to sign-in.");
      return actingAsUser(user);
    },
    app: () => ({ ownDb: () => database("(own)", "application") }),
  };
}

export { LEVEL };
