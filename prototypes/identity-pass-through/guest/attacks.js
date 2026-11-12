// The guest for the identity spike: an Application that tries to make the
// host attach the wrong User or Application to an operation. The host gives
// it one invoke binding, HOST, with `whoami` (answers what the host
// attached) and `sleep` (pending for a while, then answers what the host
// attached at call time). Every route answers JSON.

const settle = (promise) =>
  Promise.resolve(promise).then(
    (ok) => ({ ok }),
    (e) => ({ err: String((e && e.message) || e) }),
  );
const whoami = (args) => settle(env.HOST.invoke("whoami", args ?? {}));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const settleNative = (binding, command, args) =>
  settle(new Promise((resolve, reject) => __nativeBindingInvoke(binding, command, args, resolve, reject)));

// Top level, at load: no request is in flight.
globalThis.loadResult = "not settled";
settle(env.HOST.invoke("whoami", { tag: "load" })).then((r) => {
  globalThis.loadResult = r;
});

globalThis.queue = [];

const routes = {
  async "/whoami"(url) {
    return { whoami: await whoami({ tag: url.searchParams.get("tag") }) };
  },

  // What the guest sees of the request: the identity header is gone.
  async "/headers"(url, request) {
    return { headers: Object.fromEntries(request.headers) };
  },

  // Claim another User through everything the guest can put in a call.
  async "/claim-user"() {
    globalThis.user = "mallory";
    globalThis.USER = "mallory";
    return {
      args: await whoami({
        user: "mallory",
        application: "payroll",
        request_id: 1,
        headers: { "x-vm-user": "mallory" },
      }),
      // Arguments of any shape, straight to the native function
      native: await settleNative("HOST", "whoami", ["mallory", { user: "mallory" }]),
      // A Request the guest builds with the header, passed as an argument
      request: await whoami({
        request: Object.fromEntries(new Request("http://x/", { headers: { "x-vm-user": "mallory" } }).headers),
      }),
      // An outbound fetch with the header (no fetch is wired; must fail)
      fetch: await settle(fetch("http://127.0.0.1/whoami", { headers: { "x-vm-user": "mallory" } }).then((r) => r.text())),
    };
  },

  // Claim another Application.
  async "/claim-app"() {
    return {
      envKeys: Object.keys(env),
      otherBinding: await settleNative("PAYROLL", "whoami", {}),
      otherBindingCase: await settleNative("host", "whoami", {}),
      emptyBinding: await settleNative("", "whoami", {}),
      argClaim: await whoami({ application: "payroll" }),
      redefineEnv: await settle(
        new Promise((resolve) => {
          try {
            Object.defineProperty(globalThis, "env", { value: { HOST: env.HOST, PAYROLL: env.HOST } });
            resolve("redefined");
          } catch (e) {
            resolve("refused: " + e.message);
          }
        }),
      ),
    };
  },

  // Keep handles for later and leave work behind after the answer.
  async "/stash"(url, request, ctx) {
    const tag = url.searchParams.get("tag") ?? "stash";
    const HOST = env.HOST;
    globalThis.stash = {
      env,
      HOST,
      invoke: HOST.invoke,
      later: () => env.HOST.invoke("whoami", { tag: tag + ":closure" }),
    };
    globalThis.timerResult = "not run";
    globalThis.waitUntilResult = "not run";
    globalThis.pendingResult = "not run";
    globalThis.lateTimerResult = "not run";

    // A timer that fires after the answer, while waitUntil keeps the event open
    setTimeout(async () => {
      globalThis.timerResult = await whoami({ tag: tag + ":timer" });
    }, 20);
    // A timer that fires after the event is over
    setTimeout(async () => {
      globalThis.lateTimerResult = await whoami({ tag: tag + ":late-timer" });
    }, 300);
    // waitUntil: runs after the answer, before the event ends
    ctx.waitUntil(
      sleep(50).then(async () => {
        globalThis.waitUntilResult = await whoami({ tag: tag + ":waitUntil" });
      }),
    );
    // A pending host op nobody waits for; its continuation calls whoami
    globalThis.pending = env.HOST.invoke("sleep", { ms: 200, tag: tag + ":pending-sleep" }).then(async (r) => {
      globalThis.pendingResult = { sleep: r, whoami: await whoami({ tag: tag + ":pending" }) };
    });
    return { stashed: tag, whoami: await whoami({ tag }) };
  },

  async "/use-stash"(url) {
    const tag = url.searchParams.get("tag") ?? "use";
    return {
      closure: await settle(globalThis.stash.later()),
      detachedInvoke: await settle(globalThis.stash.invoke("whoami", { tag: tag + ":detached" })),
      oldEnv: await settle(globalThis.stash.env.HOST.invoke("whoami", { tag: tag + ":old-env" })),
      timerResult: globalThis.timerResult,
      waitUntilResult: globalThis.waitUntilResult,
      pendingResult: globalThis.pendingResult,
      lateTimerResult: globalThis.lateTimerResult,
      loadResult: globalThis.loadResult,
    };
  },

  async "/results"() {
    return {
      timerResult: globalThis.timerResult,
      waitUntilResult: globalThis.waitUntilResult,
      pendingResult: globalThis.pendingResult,
      lateTimerResult: globalThis.lateTimerResult,
      betweenResult: globalThis.betweenResult,
      loadResult: globalThis.loadResult,
    };
  },

  // A timer that fires when no request is in flight.
  async "/timer-between"() {
    globalThis.betweenResult = "not run";
    setTimeout(async () => {
      globalThis.betweenResult = await whoami({ tag: "between" });
    }, 10);
    return { armed: true };
  },

  // Many calls in one turn, past the runtime's inline budget (16), none
  // awaited: the answer leaves before they are settled.
  async "/flood"(url) {
    const tag = url.searchParams.get("tag");
    const n = Number(url.searchParams.get("n") ?? 40);
    for (let i = 0; i < n; i++) env.HOST.invoke("whoami", { tag, i }).catch(() => {});
    return { flooded: n };
  },

  async "/flood-await"(url) {
    const tag = url.searchParams.get("tag");
    const n = Number(url.searchParams.get("n") ?? 40);
    const all = [];
    for (let i = 0; i < n; i++) all.push(whoami({ tag, i }));
    return { results: await Promise.all(all) };
  },

  // Outbound fetch: the host answers http://host.test/ with what it attached.
  async "/fetch-whoami"(url) {
    const tag = url.searchParams.get("tag");
    return { fetch: await settle(fetch("http://host.test/", { headers: { "x-tag": tag } }).then((r) => r.json())) };
  },
  async "/fetch-flood"(url) {
    const tag = url.searchParams.get("tag");
    const n = Number(url.searchParams.get("n") ?? 40);
    for (let i = 0; i < n; i++) fetch("http://host.test/", { headers: { "x-tag": tag } }).catch(() => {});
    return { flooded: n };
  },

  // Global state between requests of different Users.
  async "/global-write"(url) {
    globalThis.leak = url.searchParams.get("v");
    return { wrote: globalThis.leak };
  },
  async "/global-read"() {
    return { read: globalThis.leak ?? null };
  },

  // Work queued by one request, run by the next one.
  async "/queue-push"(url) {
    const tag = url.searchParams.get("tag");
    globalThis.queue.push(() => whoami({ tag: tag + ":queued" }));
    return { queued: globalThis.queue.length };
  },
  async "/queue-drain"() {
    const results = [];
    while (globalThis.queue.length) results.push(await globalThis.queue.shift()());
    return { results };
  },

  // Two requests at once: each awaits a pending host op and a timer, then
  // asks again.
  async "/interleave"(url) {
    const tag = url.searchParams.get("tag");
    const before = await whoami({ tag: tag + ":before" });
    const slept = await settle(env.HOST.invoke("sleep", { ms: 150, tag }));
    const afterHostOp = await whoami({ tag: tag + ":after-host-op" });
    await sleep(100);
    const afterTimer = await whoami({ tag: tag + ":after-timer" });
    return { before, slept, afterHostOp, afterTimer };
  },

  // Tamper with the globals and prototypes the runtime's JS glue uses.
  async "/tamper"() {
    const tried = {};
    const attempt = (name, f) => {
      try {
        f();
        tried[name] = "done";
      } catch (e) {
        tried[name] = "refused: " + e.message;
      }
    };
    const original = globalThis.__nativeBindingInvoke;
    attempt("wrap __nativeBindingInvoke", () => {
      globalThis.__nativeBindingInvoke = (binding, command, args, resolve, reject) =>
        original(binding, command,{ ...(args || {}), user: "mallory" }, resolve, reject);
    });
    attempt("redefine __timerCallbacks", () => {
      Object.defineProperty(globalThis, "__timerCallbacks", { value: new Map() });
    });
    attempt("Map.prototype.clear throws", () => {
      Map.prototype.clear = function () {
        throw new Error("tampered Map.clear");
      };
    });
    attempt("Set.prototype.clear throws", () => {
      Set.prototype.clear = function () {
        throw new Error("tampered Set.clear");
      };
    });
    attempt("Reflect.apply throws", () => {
      Reflect.apply = function () {
        throw new Error("tampered Reflect.apply");
      };
    });
    attempt("__ow.asyncContextSet replaced", () => {
      globalThis.__ow.asyncContextSet = () => {};
    });
    attempt("Promise.prototype.then hooked", () => {
      const then = Promise.prototype.then;
      Promise.prototype.then = function (a, b) {
        globalThis.thenCalls = (globalThis.thenCalls || 0) + 1;
        return then.call(this, a, b);
      };
    });
    attempt("Object.freeze is identity", () => {
      Object.freeze = (o) => o;
    });
    // A timer armed after the tampering, to see whether the next request
    // still clears it
    globalThis.tamperTimerResult = "not run";
    setTimeout(async () => {
      globalThis.tamperTimerResult = await whoami({ tag: "tamper-timer" });
    }, 30);
    return { tried, whoami: await whoami({ tag: "tamper" }), keys: Object.getOwnPropertyNames(globalThis).filter((k) => k.startsWith("__")) };
  },

  async "/tamper-check"() {
    return {
      whoami: await whoami({ tag: "after-tamper" }),
      tamperTimerResult: globalThis.tamperTimerResult,
      thenCalls: globalThis.thenCalls ?? 0,
    };
  },
};

globalThis.default = {
  async fetch(request, _env, ctx) {
    const url = new URL(request.url);
    const route = routes[url.pathname];
    if (!route) return Response.json({ error: "no route " + url.pathname }, { status: 404 });
    return Response.json(await route(url, request, ctx));
  },
};
