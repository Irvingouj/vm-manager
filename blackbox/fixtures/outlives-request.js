// A hostile Application that leaves code running after its request: timers,
// a promise that settles late, and kept references to ctx, to env.VM and to
// its invoke function. That code tries to use them once the request is over,
// hoping to act as some User. CONTRACT.md: such code never gets an allowed
// host call; ctx.user() then throws "no request in flight...".
//
// What the leftover code finds, it prints with the prefix "LEFTOVER", since
// printing is the only way it could tell anyone.
//
// Routes:
//   /leave-behind      arms the leftover code and answers at once
//   /flood?n=<n>       makes <n> host calls without awaiting them and answers
//   /victim?ms=<ms>    reads its User, waits <ms>, reads it again, then makes
//                      exactly one `whoami` call. It prints nothing.

const json = (value) =>
  new Response(JSON.stringify(value), { headers: { "content-type": "application/json" } });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function leftover(tag, kept) {
  try {
    const user = kept.ctx.user();
    console.log(`LEFTOVER ${tag}: ctx.user() returned ${JSON.stringify(user)}`);
  } catch (e) {
    console.log(`LEFTOVER ${tag}: ctx.user() threw ${String((e && e.message) || e)}`);
  }
  const calls = {
    "kept env.VM": () => kept.vm.invoke("whoami", {}),
    "kept invoke": () => kept.invoke.call(kept.vm, "whoami", {}),
    "global env.VM": () => env.VM.invoke("whoami", {}),
  };
  for (const [name, call] of Object.entries(calls)) {
    try {
      const answer = await call();
      console.log(`LEFTOVER ${tag}: whoami through ${name} answered ${JSON.stringify(answer)}`);
    } catch (e) {
      console.log(`LEFTOVER ${tag}: whoami through ${name} threw ${String((e && e.message) || e)}`);
    }
  }
}

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);

    if (url.pathname === "/leave-behind") {
      const kept = { ctx, vm: env.VM, invoke: env.VM.invoke };
      globalThis.kept = kept;
      for (const ms of [0, 20, 100, 300, 700]) setTimeout(() => leftover(`timer ${ms}ms`, kept), ms);
      sleep(150).then(() => leftover("late promise", kept));
      // A host call made now whose continuation runs after the answer.
      kept.vm.invoke("whoami", {}).then(
        () => sleep(50).then(() => leftover("continuation of a host call", kept)),
        () => leftover("continuation of a refused host call", kept),
      );
      return json({ left: true, user: ctx.user() });
    }

    if (url.pathname === "/flood") {
      const n = Number(url.searchParams.get("n") || 200);
      for (let i = 0; i < n; i++) env.VM.invoke("whoami", {}).catch(() => {});
      return json({ flooded: n, user: ctx.user() });
    }

    if (url.pathname === "/victim") {
      const ms = Number(url.searchParams.get("ms") || 0);
      const before = ctx.user();
      await sleep(ms);
      const after = ctx.user();
      const whoami = await env.VM.invoke("whoami", {});
      return json({ before, after, whoami });
    }

    return new Response("no such route", { status: 404 });
  },
};
