// An honest Application. It tells the caller who the User is, as ctx.user()
// and as the host's `whoami`, and what it was handed. It prints one line
// per request, so that a request that ran its code leaves a `printed`
// record.
//
// Routes:
//   /hello              text naming the User and their Groups
//   /                   (and any other path) JSON: user, whoami, the request
//   /unknown-operation  what the host answers to an operation it does not know
//   /fetch?to=<url>     whether an outbound fetch to <url> rejects
//   /print              one console line per level

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const settle = (promise) =>
  Promise.resolve(promise).then(
    (value) => ({ resolved: value }),
    (e) => ({ rejected: String((e && e.message) || e) }),
  );

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const user = ctx.user();
    console.log(`honest: handling ${url.pathname}`);

    if (url.pathname === "/hello") {
      return new Response(`Hello, ${user.id}! Your Groups: ${user.groups.join(",")}.`, {
        headers: { "content-type": "text/plain" },
      });
    }

    if (url.pathname === "/unknown-operation") {
      return json({ answer: await settle(env.VM.invoke("no-such-operation", {})) });
    }

    if (url.pathname === "/fetch") {
      return json({ fetch: await settle(fetch(url.searchParams.get("to")).then((r) => r.status)) });
    }

    if (url.pathname === "/print") {
      console.log(`log line from ${user.id}`);
      console.info(`info line from ${user.id}`);
      console.warn(`warn line from ${user.id}`);
      console.error(`error line from ${user.id}`);
      console.debug(`debug line from ${user.id}`);
      return json({ printed: true });
    }

    const whoami = await env.VM.invoke("whoami", {});
    const headers = {};
    for (const [name, value] of request.headers) headers[name] = value;
    return json({
      user,
      whoami,
      ctxFrozen: Object.isFrozen(ctx),
      userFrozen: Object.isFrozen(user),
      method: request.method,
      url: request.url,
      headers,
      body: await request.text(),
    });
  },
};
