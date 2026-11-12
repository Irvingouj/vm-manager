// An Application that calls the host synchronously at its top level, while
// it loads, when no request is in flight. The handler then reports what
// that call got.

let atLoad;
try {
  atLoad = env.VM.invoke("whoami", {}).then(
    (answer) => ({ answered: answer }),
    (e) => ({ rejected: String((e && e.message) || e) }),
  );
} catch (e) {
  atLoad = Promise.resolve({ threw: String((e && e.message) || e) });
}

globalThis.default = {
  async fetch(request, ctx) {
    return new Response(
      JSON.stringify({
        atLoad: await atLoad,
        user: ctx.user(),
      }),
      { headers: { "content-type": "application/json" } },
    );
  },
};
