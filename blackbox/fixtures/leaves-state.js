// A hostile Application that leaves a value everywhere it can in memory, so
// that a later request might find it. CONTRACT.md: nothing in memory
// survives from one request to another.
//
// Routes:
//   /leave?value=<v>  puts <v> in every place below
//   /look             reports what every place holds now

const json = (value) =>
  new Response(JSON.stringify(value), { headers: { "content-type": "application/json" } });

// Captured before any request can replace them, so /look reports honestly.
const realMathMax = Math.max;

// Module-level variables and a closure.
let moduleLet = "fresh";
var moduleVar = "fresh";
let handledBefore = 0;
const closure = (() => {
  let captured = "fresh";
  return { get: () => captured, set: (v) => (captured = v) };
})();

// Counts how many times this top level ran in this context.
globalThis.loadCount = (globalThis.loadCount || 0) + 1;

function leave(value) {
  const tried = {};
  const attempt = (name, f) => {
    try {
      f();
      tried[name] = "done";
    } catch (e) {
      tried[name] = "refused: " + String((e && e.message) || e);
    }
  };
  attempt("globalThis", () => (globalThis.leftOnGlobal = value));
  attempt("symbol on globalThis", () => (globalThis[Symbol.for("left")] = value));
  attempt("Object.prototype", () => (Object.prototype.leftOnObjectPrototype = value));
  attempt("Array.prototype", () => (Array.prototype.leftOnArrayPrototype = value));
  attempt("String.prototype", () => (String.prototype.leftOnStringPrototype = value));
  attempt("Promise.prototype", () => (Promise.prototype.leftOnPromisePrototype = value));
  attempt("property of JSON", () => (JSON.leftOnJSON = value));
  attempt("property of Response", () => (Response.leftOnResponse = value));
  attempt("built-in function replaced", () => (Math.max = () => value));
  attempt("property of env", () => (env.leftOnEnv = value));
  attempt("property of env.VM", () => (env.VM.leftOnVM = value));
  attempt("module let", () => (moduleLet = value));
  attempt("module var", () => (moduleVar = value));
  attempt("closure", () => closure.set(value));
  return tried;
}

function look() {
  const read = (f) => {
    try {
      const v = f();
      return v === undefined ? null : v;
    } catch (e) {
      return "threw: " + String((e && e.message) || e);
    }
  };
  const mathMax = Math.max === realMathMax ? "native" : read(() => Math.max(1, 2));
  return {
    globalThis: read(() => globalThis.leftOnGlobal),
    "symbol on globalThis": read(() => globalThis[Symbol.for("left")]),
    "Object.prototype": read(() => ({}).leftOnObjectPrototype),
    "Array.prototype": read(() => [].leftOnArrayPrototype),
    "String.prototype": read(() => "".leftOnStringPrototype),
    "Promise.prototype": read(() => Promise.resolve().leftOnPromisePrototype),
    "property of JSON": read(() => JSON.leftOnJSON),
    "property of Response": read(() => Response.leftOnResponse),
    "built-in function replaced": mathMax === "native" ? null : mathMax,
    "property of env": read(() => env.leftOnEnv),
    "property of env.VM": read(() => env.VM.leftOnVM),
    "module let": moduleLet === "fresh" ? null : moduleLet,
    "module var": moduleVar === "fresh" ? null : moduleVar,
    closure: closure.get() === "fresh" ? null : closure.get(),
  };
}

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const before = handledBefore;
    handledBefore += 1;
    const user = ctx.user();
    if (url.pathname === "/leave") {
      const tried = leave(url.searchParams.get("value"));
      return json({ user, tried, handledBefore: before, loadCount: globalThis.loadCount });
    }
    return json({ user, found: look(), handledBefore: before, loadCount: globalThis.loadCount });
  },
};
