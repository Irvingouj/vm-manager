// A hostile Application that tries to get rows from a shared database it,
// or its User, should not: by querying with no request in flight, by
// claiming another identity in the arguments, and by overwriting globals
// and built-ins before it asks.
//
// The identity it would like to have: the User "it-admin" in the Groups
// "platform-admins", "readers" and "writers", in the Application "board",
// Version 1, at Access level "write".
//
// Every answer is built with built-ins captured at load, after undoing
// what was replaced, so that it reports honestly what the host said.
// A reading is what invoke resolved to, or { threw: <message> }.
//
// At load (no request in flight) it queries `devops`; GET /at-load answers
// what that got.
//
// Routes:
//   GET  /at-load          { atLoad }
//   POST /claim   {args}   the query with an identity claimed in the
//                          arguments, in a third argument, and on the
//                          arguments' prototype: { answers: [3 readings] }
//   POST /tamper  {args}   globals and built-ins overwritten, then the
//                          query: { answer, tampered: { what: outcome } }
//   POST /later   {args, delayMs}
//                          answers at once; the query is made from a timer
//                          after the request has ended

const real = {
  apply: Reflect.apply,
  defineProperty: Object.defineProperty,
  getOwnPropertyDescriptor: Object.getOwnPropertyDescriptor,
  deleteProperty: Reflect.deleteProperty,
  create: Object.create,
  assign: Object.assign,
  stringify: JSON.stringify,
  Response,
  Promise,
  vm: env.VM,
  invoke: env.VM.invoke,
  setTimeout,
  ArrayPrototype: Array.prototype,
  ObjectPrototype: Object.prototype,
  StringPrototype: String.prototype,
};

const CLAIMED_USER = { id: "it-admin", groups: ["platform-admins", "readers", "writers"] };
const CLAIMS = {
  user: CLAIMED_USER,
  userId: CLAIMED_USER.id,
  groups: CLAIMED_USER.groups,
  application: "board",
  version: 1,
  access: "write",
  level: "write",
  needed: "structure",
  declared: "write",
  request: "req-of-someone-else",
  as: CLAIMED_USER,
};

const message = (e) => {
  try {
    return "" + ((e && e.message) || e);
  } catch {
    return "an error that cannot be printed";
  }
};

async function query(...args) {
  try {
    return await real.apply(real.invoke, real.vm, ["query", ...args]);
  } catch (e) {
    return { threw: message(e) };
  }
}

const json = (value) =>
  new real.Response(real.stringify(value), { headers: { "content-type": "application/json" } });

// --- At load: no request in flight ----------------------------------------
const atLoad = query({ resource: "devops", sql: "select 'at-load-probe' as probe, title from incidents" });

// --- Replacing things, and undoing it before answering ------------------
const saved = [];
function remember(target, key) {
  saved[saved.length] = [target, key, real.getOwnPropertyDescriptor(target, key)];
}
function replace(target, key, value) {
  try {
    remember(target, key);
    target[key] = value;
    return target[key] === value ? "replaced" : "unchanged";
  } catch (e) {
    return "refused: " + message(e);
  }
}
function redefine(target, key, descriptor) {
  try {
    remember(target, key);
    real.defineProperty(target, key, descriptor);
    return "redefined";
  } catch (e) {
    return "refused: " + message(e);
  }
}
function undoAll() {
  for (let i = saved.length - 1; i >= 0; i--) {
    const [target, key, descriptor] = saved[i];
    try {
      if (descriptor) real.defineProperty(target, key, descriptor);
      else real.deleteProperty(target, key);
    } catch {
      // Some replacements cannot be undone; the answer is built with
      // captured built-ins anyway.
    }
  }
  saved.length = 0;
}

function tamper(ctx) {
  const t = {};
  // Globals a host might read the identity from.
  t.globalUser = replace(globalThis, "user", CLAIMED_USER);
  t.globalGroups = replace(globalThis, "groups", CLAIMED_USER.groups);
  t.globalApplication = replace(globalThis, "application", "board");
  t.globalVersion = replace(globalThis, "version", 1);
  t.globalRequest = replace(globalThis, "request", { user: CLAIMED_USER });
  t.globalCtx = replace(globalThis, "ctx", { user: () => CLAIMED_USER });
  // The context it was handed, and the host's object.
  t.ctxUser = redefine(ctx, "user", { value: () => CLAIMED_USER, configurable: true });
  t.vmUser = replace(real.vm, "user", CLAIMED_USER);
  t.vmGroups = replace(real.vm, "groups", CLAIMED_USER.groups);
  // Every object inherits the claims.
  for (const key of ["user", "groups", "application", "version", "access", "level"]) {
    t["objectPrototype." + key] = redefine(real.ObjectPrototype, key, {
      value: CLAIMS[key],
      writable: true,
      configurable: true,
    });
  }
  // Group checks done with built-ins say yes to everything.
  t.includes = replace(real.ArrayPrototype, "includes", () => true);
  t.indexOf = replace(real.ArrayPrototype, "indexOf", () => 0);
  t.some = replace(real.ArrayPrototype, "some", () => true);
  t.find = replace(real.ArrayPrototype, "find", function () {
    return this[0];
  });
  t.stringIncludes = replace(real.StringPrototype, "includes", () => true);
  // Anything serialized carries the claims.
  t.stringify = replace(JSON, "stringify", (value, ...rest) =>
    real.stringify(value && typeof value === "object" ? real.assign({}, value, CLAIMS) : value, ...rest),
  );
  return t;
}

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/at-load") {
      return json({ atLoad: await atLoad });
    }
    const body = await request.json();
    const args = body.args || {};

    if (url.pathname === "/claim") {
      const inArgs = await query(real.assign({}, args, CLAIMS));
      const inThird = await query(args, CLAIMS);
      const onPrototype = await query(real.assign(real.create(CLAIMS), args));
      return json({ answers: [inArgs, inThird, onPrototype] });
    }

    if (url.pathname === "/tamper") {
      let tampered;
      let answer;
      try {
        tampered = tamper(ctx);
        answer = await query(args);
      } finally {
        undoAll();
      }
      return json({ answer, tampered });
    }

    if (url.pathname === "/later") {
      const delay = Number(body.delayMs || 100);
      real.setTimeout(() => {
        query(args);
      }, delay);
      return json({ scheduled: true });
    }

    return new real.Response("no such route", { status: 404 });
  },
};
