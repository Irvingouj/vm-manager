// An Application for what a Storage body holds and when it gives it back,
// for the chunks a put's stream may hold, and for a wait for a place
// (CONTRACT.md, "Storage: Through the SDK", "Storage: Bounds";
// blackbox/tests/storage_stream_ends.rs). Raw interface, through
// `env.VM.invoke`.
//
// Routes (POST, JSON):
//   /get-loop { resource, key, count, budgetMs }
//       up to `count` times (and for at most `budgetMs`): get `key` and read
//       its body to the end: { done, bytes, ms } or { done, failed }
//   /own-chunks { resource, key, kind }
//       a put of a stream of the code's own whose chunks are the bytes `AB`,
//       one odd chunk of `kind`, then the bytes `CD`: kind `number`,
//       `object`, `null`, `undefined`, `boolean` (not bytes), `string`
//       (`é`), `empty` (an empty Uint8Array), `arraybuffer` (`xy`), `view`
//       (a Uint16Array holding the bytes `xy`): { answer }
//   /wait-17 { resource, key, mode }
//       16 gets of `key`, bodies unread; then a 17th, which waits for a
//       place: mode `await` answers { held, answer, ms } once the 17th is
//       decided, mode `no-await` starts it and answers { held } at once
//   /puts-then-op { resource, keyPrefix, key, count }
//       `count` puts of streams of the code's own, started and none awaited,
//       each stream waiting for a gate before its first chunk, then at once,
//       with no pause, one get of `key`, timed: { answer, ms, puts }; the
//       gate is opened after, with `AB` for each put

const message = (e) => String((e && e.message) || e);

async function call(operation, args) {
  try {
    return await env.VM.invoke(operation, args);
  } catch (e) {
    return { threw: message(e) };
  }
}

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const bytes = (text) => new TextEncoder().encode(text);

const odd = {
  number: () => 5,
  object: () => ({ a: 1 }),
  null: () => null,
  undefined: () => undefined,
  boolean: () => true,
  string: () => "é",
  empty: () => new Uint8Array(0),
  arraybuffer: () => bytes("xy").buffer,
  view: () => new Uint16Array([0x7978]),
};

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    const b = await request.json().catch(() => ({}));
    switch (url.pathname) {
      case "/get-loop": {
        const count = Number(b.count || 1);
        const budget = Number(b.budgetMs || 20000);
        const began = Date.now();
        let done = 0;
        let total = 0;
        while (done < count && Date.now() - began < budget) {
          const answer = await call("storage.get", { resource: b.resource, key: b.key });
          if (!answer || !answer.ok) return json({ done, failed: answer });
          const reader = answer.ok.body.getReader();
          for (;;) {
            const { done: end, value } = await reader.read();
            if (end) break;
            total += value.length;
          }
          done += 1;
        }
        return json({ done, bytes: total, ms: Date.now() - began });
      }
      case "/own-chunks": {
        const make = odd[b.kind];
        const stream = new ReadableStream({
          start(controller) {
            controller.enqueue(bytes("AB"));
            controller.enqueue(make());
            controller.enqueue(bytes("CD"));
            controller.close();
          },
        });
        return json({
          answer: await call("storage.put", { resource: b.resource, key: b.key, body: stream }),
        });
      }
      case "/wait-17": {
        let held = 0;
        const bodies = [];
        for (let i = 0; i < 16; i++) {
          const answer = await call("storage.get", { resource: b.resource, key: b.key });
          if (!answer || !answer.ok) return json({ held, failed: answer });
          bodies.push(answer.ok.body);
          held += 1;
        }
        const began = Date.now();
        const seventeenth = call("storage.get", { resource: b.resource, key: b.key });
        if (b.mode === "no-await") return json({ held });
        const answer = await seventeenth;
        return json({ held, answer: answer && answer.ok ? { ok: true } : answer, ms: Date.now() - began });
      }
      case "/puts-then-op": {
        const count = Number(b.count || 16);
        let open;
        const gate = new Promise((resolve) => {
          open = resolve;
        });
        const running = [];
        for (let i = 0; i < count; i++) {
          let sent = false;
          const stream = new ReadableStream({
            async pull(controller) {
              await gate;
              if (sent) {
                controller.close();
                return;
              }
              sent = true;
              controller.enqueue(bytes("AB"));
            },
          });
          running.push(
            call("storage.put", {
              resource: b.resource,
              key: `${b.keyPrefix || "gated"}/${i}.bin`,
              body: stream,
            }),
          );
        }
        const began = Date.now();
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        const ms = Date.now() - began;
        open();
        const puts = await Promise.all(running);
        return json({ answer: answer && answer.ok ? { ok: true } : answer, ms, puts });
      }
    }
    return json({ error: "no such route" }, 404);
  },
};
