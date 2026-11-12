// Runs candidate B's sample handlers (the real .ts files, types stripped by Node)
// against the fake host. Usage: node b-define-app/runtime/run.mjs
import { registerHooks } from "node:module";

const sdk = new URL("./sdk.mjs", import.meta.url).href;
registerHooks({
  resolve: (specifier, context, next) => (specifier === "@vm/sdk" ? { url: sdk, shortCircuit: true } : next(specifier, context)),
});

const { host } = await import("./fake-host.mjs");
const { default: app } = await import("../src/index.ts");

host.deploy(app.manifest);
const alice = { id: "alice", email: "alice@corp.example", displayName: "Alice" };
const bob = { id: "bob", email: "bob@corp.example", displayName: "Bob" };

async function show(label, request, user) {
  const res = await host.serve(app, request, user);
  const body = await res.text();
  console.log(`${label}\n  -> ${res.status} ${res.headers.get("location") ?? ""}${body}\n`);
}

const url = (p) => `https://incident-board.corp.example${p}`;
await show("(a) GET /incidents as alice (sre holds read on devops)", new Request(url("/incidents")), alice);
await show("(a) GET /incidents as bob (no Grant), refusal not caught", new Request(url("/incidents")), bob);
await show("(a) GET /incidents with no User", new Request(url("/incidents")), null);
await show("(c) GET /incidents/explain as bob, refusal handled", new Request(url("/incidents/explain")), bob);
const ack = { method: "POST", body: JSON.stringify({ incidentId: 1, note: "on it" }) };
await show("(b) POST /ack as alice, written as the Application", new Request(url("/ack"), ack), alice);
await show("(b) POST /ack with no User", new Request(url("/ack"), ack), null);

// Misuse the types would stop, done here in plain JS to show the host stops it too.
const misuse = {
  manifest: app.manifest,
  async fetch(_request, ctx) {
    globalThis.kept = ctx; // keep the context for later
    await ctx.user().db("devops").query({ text: "delete from incidents", params: [] }); // raw write through a read declaration
    return new Response("unreachable");
  },
};
await host.serve(misuse, new Request(url("/x")), alice).then(async (r) => console.log(`raw write via read handle -> ${r.status} ${await r.text()}\n`));
await host.serve({ ...misuse, fetch: async (_r, c) => Response.json(await c.app().ownDb().select("acks")) }, new Request(url("/x")), bob)
  .then(async (r) => console.log(`as the Application, own database, bob in flight -> ${r.status} ${await r.text()}\n`));
try {
  await globalThis.kept.app().ownDb().insert("acks", { note: "after the response" });
} catch (e) {
  console.log(`context kept and used after its request -> ${e.name}: ${JSON.stringify(e.refusal)}\n`);
}

console.log("Operation log:");
for (const entry of host.operationLog) console.log(" ", JSON.stringify(entry));
console.log("Own-database rows with platform-recorded identity:");
for (const row of host.ownRows) console.log(" ", JSON.stringify(row));
