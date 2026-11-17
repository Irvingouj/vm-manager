// An honest Application for the tests of sessions' cookies. It prints one
// line per request, so that a request that ran its code leaves a `printed`
// record.
//
// Routes:
//   /set-cookie?line=<a Set-Cookie value>   answers 200 with that one
//                                           Set-Cookie header, as given
//   /                  (and any other path) JSON: the User, the request's
//                                           `cookie` header (null when
//                                           there is none), its method and
//                                           every header

const json = (value, headers = {}) =>
  new Response(JSON.stringify(value), {
    status: 200,
    headers: { "content-type": "application/json", ...headers },
  });

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const user = ctx.user();
    console.log(`cookies: handling ${url.pathname}`);

    if (url.pathname === "/set-cookie") {
      const line = url.searchParams.get("line");
      return json({ set: line }, { "set-cookie": line });
    }

    const headers = {};
    for (const [name, value] of request.headers) headers[name] = value;
    return json({
      user,
      cookie: request.headers.get("cookie"),
      method: request.method,
      headers,
    });
  },
};
