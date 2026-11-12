// A hostile Application that overwrites globals and built-ins at its top
// level, before the host hands it the request: whatever the host's glue
// does with them while calling the handler, it would hand over the forged
// identity "mallory" (Group "admins", Application "payroll"). It also
// registers its handler through a getter.
//
// The replacements forge only identity-shaped values (a User, something
// holding a user, a ctx), so that the rest of the runtime keeps working.
//
// The one route answers JSON with what the handler saw, built with built-ins
// captured before the tampering, after undoing it.

const real = {
  apply: Reflect.apply,
  defineProperty: Object.defineProperty,
  getOwnPropertyDescriptor: Object.getOwnPropertyDescriptor,
  getOwnPropertyNames: Object.getOwnPropertyNames,
  deleteProperty: Reflect.deleteProperty,
  isFrozen: Object.isFrozen,
  stringify: JSON.stringify,
  parse: JSON.parse,
  Response,
  Promise,
  vm: env.VM,
  invoke: env.VM.invoke,
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

const saved = [];
function replace(target, key, value) {
  try {
    saved[saved.length] = [target, key, real.getOwnPropertyDescriptor(target, key)];
    target[key] = value;
    return target[key] === value ? "replaced" : "unchanged";
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
      // Built with captured built-ins anyway.
    }
  }
  saved.length = 0;
}

function forge(value) {
  if (value && typeof value === "object") {
    if (typeof value.id === "string" && Array.isArray(value.groups)) return FORGED_USER;
    if (typeof value.user === "function") return FORGED_CTX;
    if (Object.hasOwn(value, "user")) return { ...value, user: FORGED_USER, application: "payroll" };
  }
  return value;
}

function forgeArguments(args) {
  const out = [];
  for (let i = 0; i < args.length; i++) out[i] = forge(args[i]);
  return out;
}

const tried = {};
tried["globals naming another User"] = [
  replace(globalThis, "user", FORGED_USER),
  replace(globalThis, "currentUser", FORGED_USER),
  replace(globalThis, "ctx", FORGED_CTX),
  replace(globalThis, "application", "payroll"),
].join(", ");
tried["Object.freeze does nothing"] = replace(Object, "freeze", (o) => o);
tried["Object.isFrozen lies"] = replace(Object, "isFrozen", () => true);
const realParse = JSON.parse;
tried["JSON.parse forges"] = replace(JSON, "parse", (text, reviver) => forge(real.apply(realParse, JSON, [text, reviver])));
const realStringify = JSON.stringify;
tried["JSON.stringify forges"] = replace(JSON, "stringify", (v, r, s) => real.apply(realStringify, JSON, [forge(v), r, s]));
class ForgingPromise extends real.Promise {
  constructor(executor) {
    super((resolve, reject) => executor((v) => resolve(forge(v)), reject));
  }
}
tried["Promise replaced"] = replace(globalThis, "Promise", ForgingPromise);
const realThen = real.Promise.prototype.then;
tried["Promise.prototype.then forges"] = replace(real.Promise.prototype, "then", function (onFulfilled, onRejected) {
  const wrapped = typeof onFulfilled === "function" ? (v) => onFulfilled(forge(v)) : onFulfilled;
  return real.apply(realThen, this, [wrapped, onRejected]);
});
tried["Function.prototype.call forges a ctx"] = replace(Function.prototype, "call", function (thisArg, ...args) {
  return real.apply(this, thisArg, forgeArguments(args));
});
tried["Function.prototype.apply forges a ctx"] = replace(Function.prototype, "apply", function (thisArg, args) {
  return real.apply(this, thisArg, forgeArguments(args || []));
});
tried["Reflect.apply forges a ctx"] = replace(Reflect, "apply", (f, thisArg, args) => real.apply(f, thisArg, forgeArguments(args || [])));
for (const name of real.getOwnPropertyNames(globalThis)) {
  let value;
  try {
    value = globalThis[name];
  } catch {
    continue;
  }
  if (typeof value === "function" && /invoke|binding/i.test(name)) {
    tried[`global ${name} forges`] = replace(globalThis, name, function (...args) {
      return real.apply(value, this, forgeArguments(args));
    });
  }
}
tried["env.VM.invoke adds claims"] = replace(real.vm, "invoke", function (operation, args) {
  return real.apply(real.invoke, this, [operation, { ...(args || {}), user: "mallory", application: "payroll" }]);
});

// While loading, no request is in flight: whatever this call gets, it must
// not be an identity.
let atLoad;
try {
  atLoad = real.apply(real.invoke, real.vm, ["whoami", { user: "mallory" }]);
} catch (e) {
  atLoad = real.Promise.resolve({ threw: message(e) });
}

const handler = {
  async fetch(request, ctx) {
    let user = null;
    let userError = null;
    try {
      user = ctx.user();
    } catch (e) {
      userError = message(e);
    }
    const ctxFrozen = real.isFrozen(ctx);
    const userFrozen = user ? real.isFrozen(user) : null;
    let whoami;
    try {
      whoami = { answer: await env.VM.invoke("whoami", {}) };
    } catch (e) {
      whoami = { threw: message(e) };
    }
    let loaded;
    try {
      loaded = { answer: await atLoad };
    } catch (e) {
      loaded = { threw: message(e) };
    }
    undoAll();
    const report = real.parse(real.stringify({ tried, user, userError, ctxFrozen, userFrozen, whoami, atLoad: loaded }));
    return new real.Response(real.stringify(report), { headers: { "content-type": "application/json" } });
  },
};

// Registered through a getter rather than by assignment, and an assignment
// to it is ignored.
real.defineProperty(globalThis, "default", {
  configurable: true,
  get() {
    return handler;
  },
  set() {},
});
