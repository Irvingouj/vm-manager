// A hostile Application that opens every stream it may in one request and
// writes the largest chunks it can (CONTRACT.md, "Storage: Bounds";
// blackbox/tests/storage_stream_bounds.rs). Through the raw interface the
// code may hold 16 bodies open and 16 streams of its own at once, and may
// write chunks of 64 KiB; more is what this Application asks for.
//
// Routes (POST, JSON):
//   /flood { resource, key, keyPrefix, gets, puts, bytesEach }
//       as much as one request may hold open at once: `gets` gets of `key`
//       whose bodies are never read (the bodies stay open), and `puts` puts
//       of streams of the code's own, started together, each writing
//       `bytesEach` bytes in chunks of 64 KiB. Every put's stream gives its
//       first chunk (the file is open and being written) and then waits
//       until all of them have, so that all the places are held at the same
//       time. With gets + puts = 16 files (the bound for one request), one
//       more get and one more put are tried meanwhile: each finds no place,
//       waits and fails. Then the puts are let go:
//       { gets: [<ok or answer>], puts: [<answer>], together: <bool>,
//         extraGet: { answer, ms }, extraPut: { answer, ms } }
//   /put-oversized { resource, key, chunk }
//       a put of a stream of the code's own whose one chunk is `chunk`
//       bytes: { answer }; the platform's glue cuts it, the put lands
//   /put-sizes { resource, key, sizes }
//       the same, chunks of the lengths in `sizes`
//   /globals
//       the runtime's own globals called directly, past the SDK:
//       __responseStreamCreate() 17 times and __responseStreamWrite(id,
//       chunk) with chunks of 65,537 and of 65,536 bytes:
//       { defined: { create, write }, creates: [{ ok } | { threw }],
//         write: { over, exact } }

const CHUNK = 64 * 1024;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

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

// A stream of `total` bytes in chunks of `chunk`, each chunk a slice of a
// pattern so that a misplaced byte is seen.
function sized(sizes, seed) {
  let at = 0;
  let n = seed >>> 0;
  return new ReadableStream({
    pull(controller) {
      if (at >= sizes.length) {
        controller.close();
        return;
      }
      const bytes = new Uint8Array(Number(sizes[at]));
      for (let i = 0; i < bytes.length; i++) {
        n = (Math.imul(n, 1664525) + 1013904223) >>> 0;
        bytes[i] = n >>> 24;
      }
      at += 1;
      controller.enqueue(bytes);
    },
  });
}

function chunked(total, chunk, seed) {
  let sent = 0;
  let n = seed >>> 0;
  return new ReadableStream({
    pull(controller) {
      if (sent >= total) {
        controller.close();
        return;
      }
      const length = Math.min(chunk, total - sent);
      const bytes = new Uint8Array(length);
      for (let i = 0; i < length; i++) {
        n = (Math.imul(n, 1664525) + 1013904223) >>> 0;
        bytes[i] = n >>> 24;
      }
      sent += length;
      controller.enqueue(bytes);
    },
  });
}

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    const b = await request.json().catch(() => ({}));
    switch (url.pathname) {
      case "/flood": {
        const nGets = Number(b.gets || 8);
        const nPuts = Number(b.puts || 8);
        const bytesEach = Number(b.bytesEach || 1024 * 1024);
        const gets = [];
        for (let i = 0; i < nGets; i++) {
          const answer = await call("storage.get", { resource: b.resource, key: b.key });
          // Nothing reads it: the body stays open until this request ends.
          gets.push(answer && answer.ok ? { ok: true } : answer);
        }
        let started = 0;
        let release;
        const gate = new Promise((resolve) => {
          release = resolve;
        });
        const allStarted = new Promise((resolve) => {
          const watch = setInterval(() => {
            if (started >= nPuts) {
              clearInterval(watch);
              resolve(true);
            }
          }, 20);
          setTimeout(() => {
            clearInterval(watch);
            resolve(false);
          }, 15000);
        });
        const gated = (total, seed) => {
          let sent = 0;
          let n = seed >>> 0;
          let first = true;
          return new ReadableStream({
            async pull(controller) {
              if (sent >= total) {
                controller.close();
                return;
              }
              if (first) {
                first = false;
                started += 1;
              } else {
                await gate;
              }
              const length = Math.min(CHUNK, total - sent);
              const bytes = new Uint8Array(length);
              for (let i = 0; i < length; i++) {
                n = (Math.imul(n, 1664525) + 1013904223) >>> 0;
                bytes[i] = n >>> 24;
              }
              sent += length;
              controller.enqueue(bytes);
            },
          });
        };
        const running = [];
        for (let i = 0; i < nPuts; i++) {
          running.push(
            call("storage.put", {
              resource: b.resource,
              key: `${b.keyPrefix || "flood"}/${i}.bin`,
              body: gated(bytesEach, i + 1),
            }),
          );
        }
        const together = await allStarted;
        const timed = async (fn) => {
          const began = Date.now();
          const answer = await fn();
          return { answer: answer && answer.ok ? { ok: true } : answer, ms: Date.now() - began };
        };
        const [extraGet, extraPut] = await Promise.all([
          timed(() => call("storage.get", { resource: b.resource, key: b.key })),
          timed(() =>
            call("storage.put", {
              resource: b.resource,
              key: `${b.keyPrefix || "flood"}/extra.bin`,
              body: chunked(CHUNK, CHUNK, 99),
            }),
          ),
        ]);
        release();
        const puts = await Promise.all(running);
        return json({ gets, puts, together, extraGet, extraPut });
      }
      case "/put-sizes":
        return json({
          answer: await call("storage.put", {
            resource: b.resource,
            key: b.key,
            body: sized(b.sizes || [], 7),
          }),
        });
      case "/globals": {
        const out = {
          defined: {
            create: typeof globalThis.__responseStreamCreate,
            write: typeof globalThis.__responseStreamWrite,
          },
          creates: [],
          write: {},
        };
        let first;
        for (let i = 0; i < 17; i++) {
          try {
            const id = globalThis.__responseStreamCreate();
            if (i === 0) first = id;
            out.creates.push({ ok: true });
          } catch (e) {
            out.creates.push({ threw: message(e) });
          }
        }
        for (const [label, size] of [
          ["over", 65537],
          ["exact", 65536],
        ]) {
          try {
            await globalThis.__responseStreamWrite(first, new Uint8Array(size));
            out.write[label] = { ok: true };
          } catch (e) {
            out.write[label] = { threw: message(e) };
          }
        }
        return json(out);
      }
      case "/put-oversized": {
        const chunk = Number(b.chunk || 1024 * 1024);
        return json({
          answer: await call("storage.put", {
            resource: b.resource,
            key: b.key,
            body: chunked(chunk, chunk, 7),
          }),
        });
      }
    }
    return json({ error: "no such route" }, 404);
  },
};
