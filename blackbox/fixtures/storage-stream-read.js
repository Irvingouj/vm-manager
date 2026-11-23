// An Application that reads a Storage file through the raw interface as
// the stream the host feeds (CONTRACT.md, "Storage: Through the raw
// interface" and "Bounds"; blackbox/tests/storage_stream_get.rs,
// storage_stream_bounds.rs). A get answers the file's metadata and `body`:
// a ReadableStream of the file as it was when it was opened, fed by the
// host in chunks of at most 64 KiB, ending after exactly `size` bytes.
// What the code can do with the body here: read it (and report what it
// read), hand it back unread as the response (then the host sends it), or
// cancel it.
//
// Each reading is what invoke resolved to, or { threw: <message> }.
//
// Routes (POST, JSON):
//   /read { resource, key }
//       get; the body read to its end:
//       { answer: { ok: <metadata>, isStream,
//                   read: { length, digest, chunks, maxChunk } } }
//       (read.failed when the stream failed while it was read, with
//       read.length the bytes before it did)
//   /read-slow { resource, key, pauseMs }
//       as /read, a pause before each chunk
//   /get-unread { resource, key }
//       get; the body not read: { answer: { ok: <metadata>, isStream } }
//   /serve { resource, key, contentLength, headers }
//       get; the body given back unread as the response, with the
//       content-length the code names when one is given and the other
//       headers of `headers` (the platform sets its own framing for a
//       file); 404 when no file
//   /read-stall { resource, key, pauseMs }
//       as /read, but one pause only, of pauseMs, after the first chunk:
//       a body that is not nearly all in a buffer of the host's yet when
//       the file is changed meanwhile
//   /read-until { resource, key, until }
//       get; the first chunk read; then wait (polling `storage.head` of
//       the key, at most 20 s) until `until` holds — "deleted": no file at
//       the key; "changed": the key's size is not the one got — then read
//       the body on to its end, as /read answers it, with `waited` the
//       milliseconds waited. What the file is made while the body is open
//       is the test's.
//   /hold-bodies { resource, keys, holdMs, cancel, read, after }
//       get every key in turn, the bodies unread; the first get that is
//       not a file ends it: { held: <n>, failed: { answer, ms } }; else
//       cancel the body at index `cancel` or read the one at index `read`
//       to its end; then one more get of `after` when given: `after`:
//       { ok, error, ms }; hold the request open for holdMs; then
//       { held: <n>, after } — the bodies are open while the request is in
//       flight
//   /get-one { resource, key }
//       one get, answered at once: { answer: { ok: <metadata> | null } }

const CHUNK = 64 * 1024;

function fnvStart() {
  return 0x811c9dc5 >>> 0;
}

function fnvByte(hash, byte) {
  return Math.imul((hash ^ byte) >>> 0, 0x01000193) >>> 0;
}

function fnvBytes(hash, bytes) {
  let h = hash;
  for (let i = 0; i < bytes.length; i++) h = fnvByte(h, bytes[i]);
  return h;
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

async function readAll(body, pauseMs, pauseOnce) {
  const isStream = typeof ReadableStream !== "undefined" && body instanceof ReadableStream;
  const read = { isStream, length: 0, digest: fnvStart(), chunks: 0, maxChunk: 0 };
  if (!isStream) {
    read.notStream = typeof body;
    return read;
  }
  const reader = body.getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      const bytes = value instanceof Uint8Array ? value : new Uint8Array(value);
      read.chunks += 1;
      read.length += bytes.length;
      read.maxChunk = Math.max(read.maxChunk, bytes.length);
      read.digest = fnvBytes(read.digest, bytes);
      if (pauseMs) await sleep(pauseMs);
      if (pauseOnce && read.chunks === 1) await sleep(pauseOnce);
    }
  } catch (e) {
    read.failed = message(e);
  }
  return read;
}

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    const b = request.method === "POST" ? await request.json().catch(() => ({})) : {};
    switch (url.pathname) {
      case "/read": {
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        if (!answer || !answer.ok) return json({ answer });
        const read = await readAll(answer.ok.body, 0);
        return json({ answer: { ok: metadataOf(answer.ok), isStream: read.isStream, read } });
      }
      case "/read-slow": {
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        if (!answer || !answer.ok) return json({ answer });
        const read = await readAll(answer.ok.body, Number(b.pauseMs || 0));
        return json({ answer: { ok: metadataOf(answer.ok), isStream: read.isStream, read } });
      }
      case "/read-stall": {
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        if (!answer || !answer.ok) return json({ answer });
        const read = await readAll(answer.ok.body, 0, Number(b.pauseMs || 0));
        return json({ answer: { ok: metadataOf(answer.ok), isStream: read.isStream, read } });
      }
      case "/read-until": {
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        if (!answer || !answer.ok) return json({ answer });
        const size = answer.ok.size;
        const reader = answer.ok.body.getReader();
        const read = { isStream: true, length: 0, digest: fnvStart(), chunks: 0, maxChunk: 0 };
        let waited = 0;
        try {
          const first = await reader.read();
          if (!first.done) {
            read.chunks += 1;
            read.length += first.value.length;
            read.maxChunk = Math.max(read.maxChunk, first.value.length);
            read.digest = fnvBytes(read.digest, first.value);
          }
          const began = Date.now();
          while (Date.now() - began < 20000) {
            const head = await call("storage.head", { resource: b.resource, key: b.key });
            const held =
              b.until === "deleted"
                ? head && head.ok === null
                : head && head.ok && head.ok.size !== size;
            if (held) break;
            await sleep(100);
          }
          waited = Date.now() - began;
          for (;;) {
            const { done, value } = await reader.read();
            if (done) break;
            read.chunks += 1;
            read.length += value.length;
            read.maxChunk = Math.max(read.maxChunk, value.length);
            read.digest = fnvBytes(read.digest, value);
          }
        } catch (e) {
          read.failed = message(e);
        }
        return json({ answer: { ok: metadataOf(answer.ok), isStream: true, read, waited } });
      }
      case "/get-unread": {
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        if (!answer || !answer.ok) return json({ answer });
        const isStream = answer.ok.body instanceof ReadableStream;
        return json({ answer: { ok: metadataOf(answer.ok), isStream } });
      }
      case "/serve": {
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        if (!answer || !answer.ok) return json({ answer }, 404);
        const headers = { "content-type": answer.ok.content_type, ...(b.headers || {}) };
        if (b.contentLength !== undefined) headers["content-length"] = String(b.contentLength);
        return new Response(answer.ok.body, { headers });
      }
      case "/hold-bodies": {
        const keys = b.keys || [];
        const bodies = [];
        for (const key of keys) {
          const began = Date.now();
          const answer = await call("storage.get", { resource: b.resource, key });
          if (!answer || !answer.ok) {
            return json({ held: bodies.length, failed: { answer, ms: Date.now() - began } });
          }
          // Open, and held: nothing reads or cancels it but what is asked.
          bodies.push(answer.ok.body);
        }
        if (b.cancel !== undefined && b.cancel !== null) {
          await bodies[Number(b.cancel)].cancel("cancelled by the test");
        }
        if (b.read !== undefined && b.read !== null) {
          const reader = bodies[Number(b.read)].getReader();
          for (;;) {
            const { done } = await reader.read();
            if (done) break;
          }
        }
        let after = null;
        if (b.after !== undefined && b.after !== null) {
          const began = Date.now();
          const answer = await call("storage.get", { resource: b.resource, key: b.after });
          const ok = !!(answer && answer.ok);
          after = { ok, error: answer && answer.error, ms: Date.now() - began };
          if (ok) bodies.push(answer.ok.body);
        }
        await sleep(Number(b.holdMs || 0));
        return json({ held: bodies.length, after });
      }
      case "/get-one": {
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        // The body (a ReadableStream) cannot be written as JSON: its metadata
        // is. The body stays open, unread, until the request ends, as the
        // places it holds are what the tests probe.
        if (answer && answer.ok) return json({ answer: { ok: metadataOf(answer.ok) } });
        return json({ answer });
      }
    }
    return json({ error: "no such route" }, 404);
  },
};
