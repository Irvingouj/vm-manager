import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);

    if (request.method === "GET" && url.pathname === "/") {
      const user = ctx.user();
      return new Response(`<!doctype html><title>${manifest.name}</title><h1>Hello, ${escapeHtml(user.displayName)}</h1>`, {
        headers: { "content-type": "text/html; charset=utf-8" },
      });
    }

    return new Response("Not found", { status: 404 });
  },
});

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}
