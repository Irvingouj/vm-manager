// The guest: one classic script, as runtime-v8 compiles it. The host gives it
// one invoke binding, HOST.
//
//   GET /?name=Ada  calls the host function `greet` and returns its answer
//   GET /spin       never yields; the CPU limit must stop it
globalThis.default = {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (url.pathname === "/spin") {
      while (true) {}
    }

    const name = url.searchParams.get("name") ?? "world";
    const host = await env.HOST.invoke("greet", { name });

    return Response.json({ guest: "hello from the guest", host });
  },
};
