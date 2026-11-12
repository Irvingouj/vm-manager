import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { listIncidents } from "./list-incidents.ts";
import { acknowledge } from "./acknowledge.ts";
import { incidentsOrExplain } from "./incidents-or-explain.ts";

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const { pathname } = new URL(request.url);
    if (pathname === "/health") return new Response("ok"); // public route: no User needed
    if (pathname === "/incidents") return listIncidents(request, ctx);
    if (pathname === "/incidents/explain") return incidentsOrExplain(request, ctx);
    if (pathname === "/ack" && request.method === "POST") return acknowledge(request, ctx);
    return new Response("not found", { status: 404 });
  },
});
