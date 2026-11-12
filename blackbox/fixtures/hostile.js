// A hostile Application that tries, inside its handler, to make the host
// see another User or another Application: by claiming them in arguments,
// by forging or replacing ctx or its user, by changing the User object it
// was given, and by overwriting globals and built-ins. Ported from the
// prototype's attacks (spike/identity-pass-through, guest/attacks.js) where
// they apply without database operations, plus more.
//
// The User it would like to be is "mallory", in Group "admins", in the
// Application "payroll".
//
// Every route answers JSON, built with built-ins captured before any
// tampering, after undoing what it replaced, so that the answer reports
// honestly what the Application saw. Readings of whoami are
// { answer: <what invoke resolved to> } or { threw: <message> }.
//
// Routes:
//   /plain              ctx.user() and whoami, no attack
//   /claim-user         another User claimed through globals and arguments
//   /claim-application  another Application claimed through arguments,
//                       names on env and the runtime's native functions
//   /replace-ctx        ctx and ctx.user replaced, redefined, called detached
//   /forge-ctx          the handler called by the Application itself with a
//                       forged ctx
//   /change-user        the returned User object changed
//   /tamper             globals and built-ins overwritten, then ctx.user()
//                       and whoami
//   /replace-default    globalThis.default replaced during the handler

// --- Captured before anything is tampered with ---------------------------
const real = {
  apply: Reflect.apply,
  defineProperty: Object.defineProperty,
  getOwnPropertyDescriptor: Object.getOwnPropertyDescriptor,
  getOwnPropertyNames: Object.getOwnPropertyNames,
  getPrototypeOf: Object.getPrototypeOf,
  hasOwn: Object.hasOwn,
  ObjectPrototype: Object.prototype,
  deleteProperty: Reflect.deleteProperty,
  isFrozen: Object.isFrozen,
  stringify: JSON.stringify,
  parse: JSON.parse,
  Response,
  Promise,
  vm: env.VM,
  invoke: env.VM.invoke,
  setTimeout,
};

const FORGED_USER = { id: "mallory", groups: ["admins"] };
const FORGED_CTX = { user: () => FORGED_USER };

const message = (e) => {
  try {
    return "" + ((e && e.message) || e);
  } catch {
    return "an error that cannot be printed";
  }
};

// Reads whoami through the invoke function captured at load.
async function whoami(args) {
  try {
    return { answer: await real.apply(real.invoke, real.vm, ["whoami", args === undefined ? {} : args]) };
  } catch (e) {
    return { threw: message(e) };
  }
}

// Awaits any thenable or value, reporting { answer } or { threw }, and gives
// up after 500 ms so that a call that never settles cannot hang the request.
async function settle(make) {
  try {
    const pending = make();
    const timeout = new real.Promise((resolve) => real.setTimeout(() => resolve("no answer within 500 ms"), 500));
    const answer = await real.Promise.race([real.Promise.resolve(pending), timeout]);
    return answer === "no answer within 500 ms" ? { threw: answer } : { answer };
  } catch (e) {
    return { threw: message(e) };
  }
}

const json = (value) =>
  new real.Response(real.stringify(value), { headers: { "content-type": "application/json" } });

// --- Replacing things, and undoing it before answering ------------------
const saved = [];
function remember(target, key) {
  saved[saved.length] = [target, key, real.getOwnPropertyDescriptor(target, key)];
}
// Replaces by assignment, as Application code would; reports whether it took.
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

// Makes a User-shaped value, or one holding a user, name mallory instead.
// Only plain objects are forged, so that a Response passing through a
// tampered `then` (the host awaiting the handler) is left alone.
function forge(value) {
  if (value && typeof value === "object" && real.getPrototypeOf(value) === real.ObjectPrototype) {
    if (real.hasOwn(value, "id") && real.hasOwn(value, "groups")) return FORGED_USER;
    if (real.hasOwn(value, "user")) return { ...value, user: FORGED_USER, application: "payroll" };
    if (real.hasOwn(value, "ok")) return { ok: forge(value.ok) };
  }
  return value;
}

// Replaces ctx-like arguments with the forged ctx, and adds claims to plain
// object arguments.
function forgeArguments(args) {
  const out = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a && typeof a === "object" && typeof a.user === "function") out[i] = FORGED_CTX;
    else if (a && typeof a === "object" && real.getPrototypeOf(a) === real.ObjectPrototype)
      out[i] = { ...a, user: "mallory", application: "payroll", request: "forged" };
    else out[i] = a;
  }
  return out;
}

// Overwrites globals and built-ins that host glue might use, each in a way
// that would hand the host or the Application the forged identity.
function tamperEverything() {
  const tried = {};
  tried["globals naming another User"] = [
    replace(globalThis, "user", FORGED_USER),
    replace(globalThis, "USER", "mallory"),
    replace(globalThis, "currentUser", FORGED_USER),
    replace(globalThis, "application", "payroll"),
    replace(globalThis, "requestId", "forged"),
  ].join(", ");
  tried["Object.prototype names another User"] = [
    replace(Object.prototype, "user", FORGED_USER),
    replace(Object.prototype, "id", "mallory"),
    replace(Object.prototype, "groups", ["admins"]),
    replace(Object.prototype, "application", "payroll"),
  ].join(", ");
  tried["Object.freeze does nothing"] = replace(Object, "freeze", (o) => o);
  tried["Object.isFrozen lies"] = replace(Object, "isFrozen", () => true);
  tried["Object.defineProperty does nothing"] = replace(Object, "defineProperty", (o) => o);
  const realParse = JSON.parse;
  tried["JSON.parse forges"] = replace(JSON, "parse", (text, reviver) => forge(real.apply(realParse, JSON, [text, reviver])));
  const realStringify = real.stringify;
  tried["JSON.stringify forges"] = replace(JSON, "stringify", (v, r, s) => real.apply(realStringify, JSON, [forge(v), r, s]));
  const RealPromise = real.Promise;
  class ForgingPromise extends RealPromise {
    constructor(executor) {
      super((resolve, reject) => executor((v) => resolve(forge(v)), reject));
    }
  }
  tried["Promise replaced"] = replace(globalThis, "Promise", ForgingPromise);
  const realThen = RealPromise.prototype.then;
  tried["Promise.prototype.then forges"] = replace(RealPromise.prototype, "then", function (onFulfilled, onRejected) {
    const wrapped = typeof onFulfilled === "function" ? (v) => onFulfilled(forge(v)) : onFulfilled;
    return real.apply(realThen, this, [wrapped, onRejected]);
  });
  const arrayMethods = ["map", "filter", "slice", "concat", "push", "join", "includes", "indexOf", "some", "every"];
  tried["Array.prototype methods forge Groups"] = arrayMethods
    .map((name) => {
      const original = Array.prototype[name];
      return replace(Array.prototype, name, function (...args) {
        const result = real.apply(original, this, args);
        if (name === "includes" || name === "some" || name === "every") return true;
        if (name === "indexOf") return 0;
        if (name === "join") return "admins";
        return result;
      });
    })
    .join(", ");
  tried["Array.from forges"] = replace(Array, "from", () => ["admins"]);
  tried["Array.isArray lies"] = replace(Array, "isArray", () => true);
  const stringMethods = ["split", "trim", "toLowerCase", "toUpperCase", "startsWith", "endsWith", "slice", "substring"];
  tried["String.prototype methods forge"] = stringMethods
    .map((name) =>
      replace(String.prototype, name, function () {
        if (name === "split") return ["mallory", "admins"];
        if (name === "startsWith" || name === "endsWith") return true;
        return "mallory";
      }),
    )
    .join(", ");
  const realCall = Function.prototype.call;
  const realFnApply = Function.prototype.apply;
  const realBind = Function.prototype.bind;
  tried["Function.prototype.call forges arguments"] = replace(Function.prototype, "call", function (thisArg, ...args) {
    return real.apply(this, thisArg, forgeArguments(args));
  });
  tried["Function.prototype.apply forges arguments"] = replace(Function.prototype, "apply", function (thisArg, args) {
    return real.apply(this, thisArg, forgeArguments(args || []));
  });
  tried["Function.prototype.bind forges"] = replace(Function.prototype, "bind", function (thisArg, ...bound) {
    const target = this;
    return (...args) => real.apply(target, FORGED_CTX, forgeArguments([...bound, ...args]));
  });
  tried["Reflect.apply forges arguments"] = replace(Reflect, "apply", (f, thisArg, args) =>
    real.apply(f, thisArg, forgeArguments(args || [])),
  );
  tried["Reflect.construct forges arguments"] = replace(Reflect, "construct", (C, args) => new C(...forgeArguments(args || [])));
  tried["Map and Set answer forged values"] = [
    replace(Map.prototype, "get", () => FORGED_USER),
    replace(Map.prototype, "has", () => true),
    replace(Map.prototype, "clear", () => {
      throw new Error("tampered Map.prototype.clear");
    }),
    replace(Set.prototype, "has", () => true),
    replace(Set.prototype, "clear", () => {
      throw new Error("tampered Set.prototype.clear");
    }),
    replace(WeakMap.prototype, "get", () => FORGED_USER),
  ].join(", ");
  tried["Symbol.for answers a fixed symbol"] = replace(Symbol, "for", () => Symbol.iterator);
  // Every global function of the runtime's glue that looks like a way into
  // the host gets the forged identity added to its arguments.
  for (const name of real.getOwnPropertyNames(globalThis)) {
    let value;
    try {
      value = globalThis[name];
    } catch {
      continue;
    }
    if (typeof value === "function" && /invoke|binding|native|host|__/i.test(name) && name !== "fetch") {
      tried[`global ${name} forges arguments`] = replace(globalThis, name, function (...args) {
        return real.apply(value, this, forgeArguments(args));
      });
    }
  }
  // Any global value that might hold who is asking is replaced with the
  // forged User.
  for (const name of real.getOwnPropertyNames(globalThis)) {
    if (!/user|identity|current|request|session|context|__/i.test(name)) continue;
    let value;
    try {
      value = globalThis[name];
    } catch {
      continue;
    }
    if (value && typeof value === "object") {
      tried[`global ${name} replaced by a forged User`] = replace(globalThis, name, FORGED_USER);
    } else if (typeof value === "string") {
      tried[`global ${name} replaced by a forged id`] = replace(globalThis, name, "mallory");
    }
  }
  // The host interface itself: invoke adds claims to its arguments.
  tried["env.VM.invoke forges arguments"] = replace(real.vm, "invoke", function (operation, args) {
    return real.apply(real.invoke, this, [operation, { ...(args || {}), user: "mallory", application: "payroll" }]);
  });
  return tried;
}

// --- Routes -----------------------------------------------------------------
const routes = {
  async "/plain"(request, ctx) {
    return { user: ctx.user(), whoami: await whoami({}) };
  },

  async "/claim-user"(request, ctx) {
    globalThis.user = "mallory";
    globalThis.USER = "mallory";
    globalThis.currentUser = FORGED_USER;
    let flip = false;
    const claims = {
      "claims in an object": () =>
        real.invoke.call(real.vm, "whoami", {
          user: "mallory",
          userId: "mallory",
          id: "mallory",
          groups: ["admins"],
          application: "payroll",
          request: "forged",
          request_id: "forged",
          headers: { "x-vm-stand-in-user": "mallory" },
        }),
      "a User object as the argument": () => real.invoke.call(real.vm, "whoami", FORGED_USER),
      "a forged ctx as the argument": () => real.invoke.call(real.vm, "whoami", FORGED_CTX),
      "a string": () => real.invoke.call(real.vm, "whoami", "mallory"),
      "an array": () => real.invoke.call(real.vm, "whoami", ["mallory", { user: "mallory" }]),
      "a number": () => real.invoke.call(real.vm, "whoami", 42),
      null: () => real.invoke.call(real.vm, "whoami", null),
      "no argument": () => real.invoke.call(real.vm, "whoami"),
      "a third argument": () => real.invoke.call(real.vm, "whoami", {}, { user: "mallory" }),
      "a Request with the stand-in header": () =>
        real.invoke.call(real.vm, "whoami", {
          request: new Request("http://payroll.vm.test/", { headers: { "x-vm-stand-in-user": "mallory" } }),
        }),
      "the stand-in header as an argument": () =>
        real.invoke.call(real.vm, "whoami", { "x-vm-stand-in-user": "mallory", "x-vm-stand-in-groups": "admins" }),
      "a Proxy claiming everything": () =>
        real.invoke.call(
          real.vm,
          "whoami",
          new Proxy(
            {},
            {
              get: () => "mallory",
              has: () => true,
              ownKeys: () => ["user", "application", "request"],
              getOwnPropertyDescriptor: () => ({ value: "mallory", enumerable: true, configurable: true }),
            },
          ),
        ),
      "a getter that answers differently each time": () =>
        real.invoke.call(real.vm, "whoami", {
          get user() {
            flip = !flip;
            return flip ? "mallory" : undefined;
          },
        }),
      "an argument whose getter throws": () =>
        real.invoke.call(real.vm, "whoami", {
          get user() {
            throw new Error("getter");
          },
        }),
      "the User in the operation name": () => real.invoke.call(real.vm, "whoami?user=mallory", {}),
      "the operation name as an object": () =>
        real.invoke.call(real.vm, { toString: () => "whoami", user: "mallory" }, {}),
    };
    const answers = {};
    for (const [name, call] of Object.entries(claims)) answers[name] = await settle(call);
    return { user: ctx.user(), answers, whoami: await whoami({}) };
  },

  async "/claim-application"(request, ctx) {
    const answers = {};
    answers["application in the arguments"] = await settle(() =>
      real.invoke.call(real.vm, "whoami", { application: "payroll", app: "payroll" }),
    );
    answers["the operation name names the Application"] = await settle(() =>
      real.invoke.call(real.vm, "payroll.whoami", {}),
    );
    for (const key of real.getOwnPropertyNames(env)) {
      if (key === "VM") continue;
      answers[`env.${key}`] = await settle(() => env[key].invoke("whoami", {}));
    }
    for (const key of ["PAYROLL", "payroll", "vm", "Vm"]) {
      answers[`env.${key} made up`] = await settle(() => env[key].invoke("whoami", {}));
    }
    // A copy of the host interface, as if the Application built its own.
    answers["invoke on a made-up object"] = await settle(() =>
      real.invoke.call({ application: "payroll", name: "PAYROLL" }, "whoami", {}),
    );
    // The runtime's native functions, called directly with other names.
    for (const name of real.getOwnPropertyNames(globalThis)) {
      if (!/invoke|binding/i.test(name)) continue;
      const native = globalThis[name];
      if (typeof native !== "function") continue;
      for (const binding of ["PAYROLL", "payroll", "VM", ""]) {
        answers[`${name}(${binding})`] = await settle(
          () =>
            new real.Promise((resolve, reject) => {
              native(binding, "whoami", { application: "payroll" }, resolve, reject);
            }),
        );
      }
    }
    const redefineEnv = redefine(globalThis, "env", { value: { VM: real.vm, PAYROLL: real.vm } });
    const assignEnv = replace(globalThis, "env", { VM: real.vm, PAYROLL: real.vm });
    answers["env redefined, then env.PAYROLL"] = await settle(() => env.PAYROLL.invoke("whoami", {}));
    undoAll();
    return { user: ctx.user(), envKeys: real.getOwnPropertyNames(env), answers, redefineEnv, assignEnv, whoami: await whoami({}) };
  },

  async "/replace-ctx"(request, ctx) {
    const tried = {};
    tried["assign ctx.user"] = replace(ctx, "user", () => FORGED_USER);
    tried["redefine ctx.user"] = redefine(ctx, "user", { value: () => FORGED_USER });
    try {
      tried["delete ctx.user"] = delete ctx.user ? "deleted or absent" : "not deleted";
    } catch (e) {
      tried["delete ctx.user"] = "refused: " + message(e);
    }
    try {
      Object.setPrototypeOf(ctx, { user: () => FORGED_USER });
      tried["change the prototype of ctx"] = "changed";
    } catch (e) {
      tried["change the prototype of ctx"] = "refused: " + message(e);
    }
    try {
      Object.assign(ctx, FORGED_CTX);
      tried["Object.assign onto ctx"] = "assigned";
    } catch (e) {
      tried["Object.assign onto ctx"] = "refused: " + message(e);
    }
    tried["user on Object.prototype"] = replace(Object.prototype, "user", () => FORGED_USER);
    const realUser = ctx.user;
    const calls = {
      "ctx.user()": () => ctx.user(),
      "ctx.user() with a claimed User": () => ctx.user("mallory", FORGED_USER),
      "detached": () => {
        const f = realUser;
        return f();
      },
      "with a forged this": () => real.apply(realUser, FORGED_CTX, []),
      "with a forged this through call": () => realUser.call({ user: realUser, id: "mallory" }),
      "bound to a forged ctx": () => realUser.bind(FORGED_CTX)(),
      "on an object made from ctx": () => Object.create(ctx).user(),
    };
    const readings = {};
    for (const [name, call] of Object.entries(calls)) {
      try {
        readings[name] = { answer: call() };
      } catch (e) {
        readings[name] = { threw: message(e) };
      }
    }
    undoAll();
    return { tried, readings, ctxFrozen: real.isFrozen(ctx), user: ctx.user(), whoami: await whoami({}) };
  },

  async "/forge-ctx"(request, ctx) {
    const inner = await globalThis.default.fetch(new Request("http://hostile.vm.test/inner"), FORGED_CTX);
    const created = (() => {
      try {
        return { answer: Object.create(ctx).user() };
      } catch (e) {
        return { threw: message(e) };
      }
    })();
    return { inner: await inner.json(), created, user: ctx.user(), whoami: await whoami({}) };
  },

  // Called only by /forge-ctx, with the forged ctx.
  async "/inner"(request, ctx) {
    return { ctxUser: ctx.user(), whoami: await whoami({}) };
  },

  async "/change-user"(request, ctx) {
    const user = ctx.user();
    const tried = {};
    const attempt = (name, f) => {
      try {
        f();
        tried[name] = "done";
      } catch (e) {
        tried[name] = "refused: " + message(e);
      }
    };
    attempt("assign id", () => (user.id = "mallory"));
    attempt("push a Group", () => user.groups.push("admins"));
    attempt("overwrite a Group", () => (user.groups[0] = "admins"));
    attempt("empty the Groups", () => (user.groups.length = 0));
    attempt("assign groups", () => (user.groups = ["admins"]));
    attempt("redefine id", () => real.defineProperty(user, "id", { value: "mallory" }));
    attempt("delete id", () => delete user.id);
    attempt("change the prototype", () => Object.setPrototypeOf(user, { id: "mallory" }));
    attempt("add a property", () => (user.admin = true));
    return {
      tried,
      userFrozen: real.isFrozen(user),
      changedUserId: user.id,
      again: ctx.user(),
      whoami: await whoami({}),
    };
  },

  async "/tamper"(request, ctx) {
    const tried = tamperEverything();
    let user;
    let userError = null;
    try {
      user = ctx.user();
    } catch (e) {
      userError = message(e);
    }
    const ctxFrozen = real.isFrozen(ctx);
    const userFrozen = user ? real.isFrozen(user) : null;
    const tampered = await whoami({});
    const throughTamperedInvoke = await settle(() => env.VM.invoke("whoami", {}));
    undoAll();
    // Read again after undoing, to show the answer is built honestly.
    const userText = real.stringify(user);
    return { tried, user: user ? real.parse(userText) : null, userError, ctxFrozen, userFrozen, whoamiWhileTampered: tampered, throughTamperedInvoke };
  },

  async "/replace-default"(request, ctx) {
    const forgedHandler = { fetch: () => json({ user: FORGED_USER, forged: true }) };
    const assigned = replace(globalThis, "default", forgedHandler);
    const redefined = redefine(globalThis, "default", { get: () => forgedHandler, configurable: true });
    // Not undone: the next request must not see it.
    saved.length = 0;
    return { assigned, redefined, user: ctx.user(), whoami: await whoami({}) };
  },
};

globalThis.default = {
  async fetch(request, ctx) {
    const route = routes[new URL(request.url).pathname];
    if (!route) return new real.Response("no such route", { status: 404 });
    const answer = await route(request, ctx);
    if (answer instanceof real.Response) return answer;
    return json(answer);
  },
};
