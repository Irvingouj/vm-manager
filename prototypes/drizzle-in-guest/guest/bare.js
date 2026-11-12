// The baseline for the startup and per-request measurements: the same two
// routes as the Drizzle bundle's /noop and /select-raw, without Drizzle.
globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/noop") return Response.json({ ok: true });
    if (url.pathname === "/select-raw") {
      const { rows } = await env.HOST.invoke("sql", {
        db: "app",
        sql: 'select "id", "title" from "tasks" where "tasks"."priority" = $1',
        params: [1],
        method: "all",
      });
      return Response.json(rows);
    }
    return Response.json({ error: "no route" }, { status: 404 });
  },
};
