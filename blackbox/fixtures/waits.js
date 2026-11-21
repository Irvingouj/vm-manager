// An honest Application whose code holds its request as long as it is
// asked, so that a test can do something (disable the Application, say)
// while the request is in flight, its code given it and not yet answered.
//
// Any method, any path, with ?ms=<n> (0 when left out):
//   prints "waits: began <path>", waits <n> ms, prints "waits: done <path>",
//   calls the host's `whoami`, prints "waits: after whoami <path>", and
//   answers 200 { waited: <n>, whoami: <what the host answered> }.

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    const ms = Number(url.searchParams.get("ms") || 0);
    console.log(`waits: began ${url.pathname}`);
    await sleep(ms);
    console.log(`waits: done ${url.pathname}`);
    let whoami;
    try {
      whoami = await env.VM.invoke("whoami", {});
    } catch (e) {
      whoami = { threw: String((e && e.message) || e) };
    }
    console.log(`waits: after whoami ${url.pathname}`);
    return new Response(JSON.stringify({ waited: ms, whoami }), {
      headers: { "content-type": "application/json" },
    });
  },
};
