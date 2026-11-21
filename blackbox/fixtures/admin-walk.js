// An Application for the browser walk of the pages of Platform
// administrators (browser_admin.rs). No script runs in the pages it
// serves; a person (the harness's browser) clicks.
//
// Routes:
//   /hello                 text naming the User
//   /query?resource=<name> runs `select 1 as one` on that Resource with the
//                          host's `query` and answers JSON of what the host
//                          said (an Operation log entry of this Application)
//   /form?action=<url>     an HTML page with one form, method POST, to
//                          <url>, and one button "Send": a page of an
//                          Application posting to another origin
//   /                      (and any other path) JSON naming the User

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const escape = (text) =>
  String(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const user = ctx.user();

    if (url.pathname === "/hello") {
      return new Response(`Hello, ${user.id}!`, {
        headers: { "content-type": "text/plain" },
      });
    }

    if (url.pathname === "/query") {
      const resource = url.searchParams.get("resource");
      let answer;
      try {
        answer = await env.VM.invoke("query", { resource, sql: "select 1 as one" });
      } catch (e) {
        answer = { threw: String((e && e.message) || e) };
      }
      return json({ answer, user });
    }

    if (url.pathname === "/form") {
      const action = url.searchParams.get("action") || "";
      const html =
        '<!doctype html><html lang="en"><head><meta charset="utf-8">' +
        "<title>A page of an Application</title></head><body>" +
        `<form method="post" action="${escape(action)}">` +
        '<input type="hidden" name="x" value="1">' +
        '<button type="submit" id="send">Send</button></form></body></html>';
      return new Response(html, {
        headers: { "content-type": "text/html; charset=utf-8" },
      });
    }

    return json({ user });
  },
};
