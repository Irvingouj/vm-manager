// An Application that puts every kind of body the raw interface takes, and
// every kind it refuses (CONTRACT.md, "Storage: Through the raw
// interface"; blackbox/tests/storage_stream_put.rs). A body is a
// ReadableStream, a string (written as UTF-8), an ArrayBuffer or an
// ArrayBufferView; nothing else is taken. The routes make each of them in
// JavaScript, because a test's JSON can only carry hex or text.
//
// Every route answers { answer: <what invoke resolved to, or
// { threw: <message> }> }.
//
// Routes (POST, JSON):
//   /put-string { resource, key, body, content_type }
//   /put-buffer { resource, key, hex, content_type }    an ArrayBuffer
//   /put-view   { resource, key, hex, content_type }    an ArrayBufferView
//   /put-own-stream { resource, key, hexChunks, failAt, cancelAt, stallMs,
//                     content_type }
//       a ReadableStream the code makes, its chunks from `hexChunks`:
//       `failAt`: the stream errors once that many chunks are out;
//       `cancelAt`: the code tries to cancel the stream right after asking
//       the put (the stream is the put's from the call on: locked, the
//       cancel rejects with a TypeError, the put goes on): the answer then
//       holds `locked` and `cancel` ({ cancelThrew, isTypeError });
//       `stallMs`: a pause before each chunk;
//       `sizes`: instead of `hexChunks`, chunks of those lengths in bytes
//       (one byte of `(index + 1) & 0xff` repeated: a chunk may be any
//       size, the platform's glue cuts the larger ones)
//   /put-request-body { resource, key, content_type }
//       a put of the request's own body: the request is POSTed with the
//       bytes to write as its body
//   /put-locked   { resource, key }   a stream locked by a reader
//   /put-read     { resource, key }   a stream already read to its end
//   /put-none     { resource, key, kind }   number, undefined (no `body`),
//                                     boolean, object, array, blob: none
//                                     of the kinds a put takes; and `null`,
//                                     which is an empty file
//   /own-stream-puts { resource, keyPrefix, count }
//       `count` puts of streams that never yield a byte, none of them
//       awaited, then one more awaited: { seventeenth: <answer> }
//   /copy-after-replace { resource, from, to, replaceWith }
//       get `from`, put `replaceWith` at `from` (which leaves the body got
//       before untouched: the file as it was opened), then put that body at
//       `to`: { first, replaced, second }

function unhex(text) {
  const out = new Uint8Array(text.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(text.slice(i * 2, i * 2 + 2), 16);
  return out;
}

const message = (e) => String((e && e.message) || e);

async function call(operation, args) {
  try {
    return await env.VM.invoke(operation, args);
  } catch (e) {
    return { threw: message(e) };
  }
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

// The metadata of an answer's `ok`, without its body.
function metadataOf(ok) {
  const c = { ...ok };
  delete c.body;
  return c;
}

function argsFor(b, body) {
  const args = { resource: b.resource, key: b.key, body };
  if (b.content_type !== undefined) args.content_type = b.content_type;
  return args;
}

// A stream the code makes, fed from `hexChunks`, that may fail, stall, or
// be cancelled by the code itself.
function madeStream(b) {
  const chunks = Array.isArray(b.sizes)
    ? b.sizes.map((size, i) => new Uint8Array(Number(size)).fill((i + 1) & 0xff))
    : (b.hexChunks || []).map(unhex);
  const failAt = b.failAt === undefined || b.failAt === null ? -1 : Number(b.failAt);
  const stallMs = Number(b.stallMs || 0);
  let at = 0;
  const stream = new ReadableStream({
    async pull(controller) {
      if (stallMs) await sleep(stallMs);
      if (at === failAt) {
        controller.error(new Error("the stream failed on purpose"));
        return;
      }
      if (at >= chunks.length) {
        controller.close();
        return;
      }
      controller.enqueue(chunks[at]);
      at += 1;
    },
  });
  return stream;
}

// A stream that never yields a byte and never ends: a put of it waits for
// its body until the request ends.
function stalledStream() {
  return new ReadableStream({
    start() {
      return new Promise(() => {});
    },
  });
}

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/put-request-body") {
      const target = new URL(request.url);
      const b = {
        resource: target.searchParams.get("resource") || "marketing",
        key: target.searchParams.get("key") || "upload.bin",
        content_type: target.searchParams.get("content_type") || undefined,
      };
      // The request's body itself, unread: the host writes it to the file
      // as it arrives from the socket.
      return json({ answer: await call("storage.put", argsFor(b, request.body)) });
    }
    const b = await request.json().catch(() => ({}));
    switch (url.pathname) {
      case "/put-string":
        return json({ answer: await call("storage.put", argsFor(b, b.body)) });
      case "/put-buffer": {
        const bytes = unhex(b.hex || "");
        const buffer = new ArrayBuffer(bytes.length);
        new Uint8Array(buffer).set(bytes);
        return json({ answer: await call("storage.put", argsFor(b, buffer)) });
      }
      case "/put-view": {
        const bytes = unhex(b.hex || "");
        // A view over a buffer of its own, not starting at 0: an
        // ArrayBufferView and nothing else.
        const padded = new Uint8Array(bytes.length + 7);
        padded.set(bytes, 3);
        const view = new Uint8Array(padded.buffer, 3, bytes.length);
        return json({ answer: await call("storage.put", argsFor(b, view)) });
      }
      case "/put-own-stream": {
        const stream = madeStream(b);
        const pending = call("storage.put", argsFor(b, stream));
        if (b.cancelAt !== undefined && b.cancelAt !== null) {
          const locked = stream.locked;
          const cancel = await stream.cancel("cancelled by the code").then(
            () => ({ cancelled: true }),
            (e) => ({ cancelThrew: message(e), isTypeError: e instanceof TypeError }),
          );
          return json({ answer: await pending, locked, cancel });
        }
        return json({ answer: await pending });
      }
      case "/put-locked": {
        const stream = madeStream({ hexChunks: ["00"] });
        stream.getReader();
        return json({ answer: await call("storage.put", argsFor(b, stream)) });
      }
      case "/put-read": {
        const stream = madeStream({ hexChunks: ["00"] });
        const reader = stream.getReader();
        for (;;) {
          const { done } = await reader.read();
          if (done) break;
        }
        reader.releaseLock();
        return json({ answer: await call("storage.put", argsFor(b, stream)) });
      }
      case "/put-none": {
        const values = {
          number: 5,
          null: null,
          boolean: true,
          object: { bytes: [1] },
          array: [1, 2],
        };
        if (b.kind === "blob") {
          if (typeof Blob === "undefined") return json({ answer: { skipped: "no Blob here" } });
          return json({ answer: await call("storage.put", argsFor(b, new Blob(["x"]))) });
        }
        if (b.kind === "undefined") {
          // No `body` at all.
          const args = { resource: b.resource, key: b.key };
          return json({ answer: await call("storage.put", args) });
        }
        return json({ answer: await call("storage.put", argsFor(b, values[b.kind])) });
      }
      case "/copy-after-replace": {
        const first = await call("storage.get", { resource: b.resource, key: b.from });
        if (!first || !first.ok) return json({ first, error: "no file to copy" });
        const replaced = await call("storage.put", {
          resource: b.resource,
          key: b.from,
          body: b.replaceWith,
        });
        // The stream was opened before the put above: it is the file as it
        // was, whatever the key holds now.
        const second = await call("storage.put", { resource: b.resource, key: b.to, body: first.ok.body });
        return json({ first: { ok: metadataOf(first.ok) }, replaced, second });
      }
      case "/own-stream-puts": {
        const count = Number(b.count || 16);
        const pending = [];
        for (let i = 0; i < count; i++) {
          pending.push(
            call("storage.put", {
              resource: b.resource,
              key: `${b.keyPrefix || "held"}/${i}.bin`,
              body: stalledStream(),
            }),
          );
        }
        const seventeenth = await call("storage.put", {
          resource: b.resource,
          key: `${b.keyPrefix || "held"}/seventeenth.bin`,
          body: stalledStream(),
        });
        return json({ seventeenth });
      }
    }
    return json({ error: "no such route" }, 404);
  },
};
