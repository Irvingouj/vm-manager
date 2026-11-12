// An honest Application that says which bundle answered. The tests replace
// %MARKER% before sending the bundle as a Version (blackbox/src/api.rs,
// `marked_bundle`), so that each Version they send answers differently.
//
// Routes:
//   /                          JSON: the marker, and what whoami answers
//                              (which names the Version)
//   /slow?ms=<ms>&token=<t>    prints "which-version <marker>: started <t>",
//                              waits <ms>, then answers like /
//
// whoami is called after the wait, so that it reports the Version the
// request is running on when it ends.

const MARKER = "%MARKER%";

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/slow") {
      const token = url.searchParams.get("token");
      console.log(`which-version ${MARKER}: started ${token}`);
      const ms = Number(url.searchParams.get("ms") || 0);
      await new Promise((resolve) => setTimeout(resolve, ms));
    }
    const whoami = await env.VM.invoke("whoami", {});
    return new Response(JSON.stringify({ marker: MARKER, user: ctx.user(), whoami }), {
      headers: { "content-type": "application/json" },
    });
  },
};
