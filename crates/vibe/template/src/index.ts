import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";

/** `text` made safe to put in a page: HTML's special characters escaped. */
function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    // Every request is signed in: ctx.user() is the person asking.
    const user = ctx.user();
    const page = `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Hello</title></head>
<body><p>Hello, ${escapeHtml(user.id)}.</p></body>
</html>
`;
    return new Response(page, { headers: { "content-type": "text/html; charset=utf-8" } });
  },
});
