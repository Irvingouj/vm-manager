// An Application that calls the outside with plain `fetch` (CONTRACT.md,
// "Outbound calls"; blackbox/tests/outbound_*.rs). It reports what each
// call resolved to or how it rejected, so that a test can see what Application
// code sees.
//
// A report is JSON: { resolved: { status, statusText, url, redirected, ok,
// headers: [[name, value], ...], bodyIsStream, length, fnv, text } } or
// { rejected: { name, message, isError, refused, failure } }. A body that
// failed while it was read: resolved.readFailed (a message), with
// resolved.length the bytes read before.
//
// Routes:
//   POST /call  { url, method, headers, redirect, as, read, body }
//       one fetch. `headers` an object, or an array of [name, value].
//       `as` is "request" to make a `Request` of (url, init) first.
//       `read` is "bytes" (default: read the answer to its end and report
//       its length and fnv and, up to 64 KiB, its text), or "none".
//       `body` is null, or { kind, ... }:
//         string { text }, bytes { length }, arraybuffer { length },
//         view { length } (a view over a larger buffer), dataview { length },
//         stream { chunks: [<text>...], size, count, failAt, pauseMs }
//         (a stream of the code's own), params { pairs }, form { fields },
//         blob { text, type }
//   GET /pass?url=&method=&redirect=
//       `return fetch(url)`: the answer given back as the response.
//   GET /pass-new?url=
//       `new Response(answer.body, { status, headers })`.
//   POST /send-request-body?url=&method=&type=
//       fetch(url, { method, body: request.body }); reports the answer's
//       status and text.
//   POST /send-file { url, method, resource, key }
//       a stored file's `body` as the call's body (the raw host interface).
//   POST /put-answer { url, resource, key }
//       the answer's body put into Storage as it is.
//   GET /text?url=         the answer read with text(); { status, text }.
//   GET /at-load           what a call made while the script loaded got.
//   GET /after-request?url=  a call made 300 ms after the request ended.
//   GET /many?url=&n=      n calls at once, each read to its end:
//       { results: [{ i, ok | failure | refused, ms }] } in the order made.
//   GET /whoami            ctx.user().

const enc = new TextEncoder();
const message = (e) => String((e && e.message) || e);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

function rejection(e) {
  return {
    name: e && e.name,
    message: message(e),
    isError: e instanceof Error,
    refused: e && e.refused,
    failure: e && e.failure,
    kind: e && e.kind,
    refusal: e && e.refusal,
    fix: e && e.fix,
    detail: e && e.detail,
    ctor: e && e.constructor && e.constructor.name,
  };
}

function fnvStart() {
  return 0x811c9dc5 >>> 0;
}
function fnvBytes(hash, bytes) {
  let h = hash;
  for (let i = 0; i < bytes.length; i++) h = Math.imul((h ^ bytes[i]) >>> 0, 0x01000193) >>> 0;
  return h;
}

// What `fetch` was handed at load, when no request is in flight.
let atLoad;
try {
  atLoad = fetch("https://example.com/at-load-secret-path?q=at-load-secret-query").then(
    (a) => ({ resolved: a.status }),
    (e) => ({ rejected: rejection(e) }),
  );
} catch (e) {
  atLoad = Promise.resolve({ threw: rejection(e) });
}

function pattern(length, seed) {
  const bytes = new Uint8Array(length);
  for (let i = 0; i < length; i++) bytes[i] = (i + (seed || 0)) % 251;
  return bytes;
}

function streamOf(spec) {
  const chunks = Array.isArray(spec.chunks) ? spec.chunks.map((t) => enc.encode(t)) : [];
  const count = spec.count || 0;
  const size = spec.size || 0;
  let i = 0;
  const total = chunks.length + count;
  return new ReadableStream({
    async pull(controller) {
      if (spec.failAt !== undefined && spec.failAt !== null && i === spec.failAt) {
        controller.error(new Error("the code's own stream failed"));
        return;
      }
      if (i >= total) {
        controller.close();
        return;
      }
      if (spec.pauseMs) await sleep(spec.pauseMs);
      controller.enqueue(i < chunks.length ? chunks[i] : pattern(size, 0));
      i += 1;
    },
  });
}

function bodyOf(spec, request) {
  if (!spec) return undefined;
  switch (spec.kind) {
    case "string":
      return spec.text;
    case "bytes":
      return pattern(spec.length, 1);
    case "arraybuffer":
      return pattern(spec.length, 1).buffer;
    case "view": {
      const padded = pattern(spec.length + 9, 1);
      return new Uint8Array(padded.buffer, 4, spec.length);
    }
    case "dataview": {
      const padded = pattern(spec.length + 9, 1);
      return new DataView(padded.buffer, 4, spec.length);
    }
    case "stream":
      return streamOf(spec);
    case "params":
      return new URLSearchParams(spec.pairs);
    case "form": {
      const form = new FormData();
      for (const [k, v] of Object.entries(spec.fields)) form.append(k, v);
      return form;
    }
    case "blob":
      return new Blob([spec.text], { type: spec.type || "" });
    case "request-body":
      return request.body;
    default:
      throw new Error("the fixture does not know the body kind " + spec.kind);
  }
}

async function describe(answer, read) {
  const out = {
    status: answer.status,
    statusText: answer.statusText,
    url: answer.url,
    redirected: answer.redirected,
    ok: answer.ok,
    type: answer.type,
    headers: [...answer.headers],
    bodyIsStream: typeof ReadableStream !== "undefined" && answer.body instanceof ReadableStream,
  };
  if (read === "none") return out;
  let length = 0;
  let hash = fnvStart();
  let chunks = 0;
  let maxChunk = 0;
  const kept = [];
  try {
    const reader = answer.body ? answer.body.getReader() : null;
    for (;;) {
      if (!reader) break;
      const { done, value } = await reader.read();
      if (done) break;
      const bytes = value instanceof Uint8Array ? value : new Uint8Array(value);
      length += bytes.length;
      chunks += 1;
      maxChunk = Math.max(maxChunk, bytes.length);
      hash = fnvBytes(hash, bytes);
      if (length <= 65536) kept.push(bytes);
    }
  } catch (e) {
    out.readFailed = message(e);
  }
  out.length = length;
  out.chunks = chunks;
  out.maxChunk = maxChunk;
  out.fnv = hash;
  if (length <= 65536) out.text = new TextDecoder().decode(concat(kept));
  return out;
}

function concat(parts) {
  let n = 0;
  for (const p of parts) n += p.length;
  const all = new Uint8Array(n);
  let at = 0;
  for (const p of parts) {
    all.set(p, at);
    at += p.length;
  }
  return all;
}

async function call(spec, request) {
  const init = {};
  if (spec.method !== undefined) init.method = spec.method;
  if (spec.headers !== undefined) init.headers = spec.headers;
  if (spec.redirect !== undefined) init.redirect = spec.redirect;
  const body = bodyOf(spec.body, request);
  if (body !== undefined) init.body = body;
  try {
    const answer =
      spec.as === "request"
        ? await fetch(new Request(spec.url, init))
        : await fetch(spec.url, init);
    return { resolved: await describe(answer, spec.read) };
  } catch (e) {
    return { rejected: rejection(e) };
  }
}

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const q = (name) => url.searchParams.get(name);
    switch (url.pathname) {
      case "/call": {
        const spec = await request.json();
        return json(await call(spec, request));
      }
      case "/pass": {
        const init = {};
        if (q("method")) init.method = q("method");
        if (q("redirect")) init.redirect = q("redirect");
        return fetch(q("url"), init);
      }
      case "/pass-new": {
        const answer = await fetch(q("url"));
        return new Response(answer.body, { status: answer.status, headers: answer.headers });
      }
      case "/send-request-body": {
        const init = { method: q("method") || "POST", body: request.body };
        if (q("type")) init.headers = { "content-type": q("type") };
        try {
          const answer = await fetch(q("url"), init);
          return json({ status: answer.status, text: await answer.text() });
        } catch (e) {
          return json({ rejected: rejection(e) });
        }
      }
      case "/send-file": {
        const b = await request.json();
        const got = await env.VM.invoke("storage.get", { resource: b.resource, key: b.key });
        if (!got || !got.ok) return json({ storage: got }, 500);
        try {
          const answer = await fetch(b.url, { method: b.method || "PUT", body: got.ok.body });
          return json({ status: answer.status, text: await answer.text() });
        } catch (e) {
          return json({ rejected: rejection(e) });
        }
      }
      case "/put-answer": {
        const b = await request.json();
        try {
          const answer = await fetch(b.url);
          const put = await env.VM.invoke("storage.put", {
            resource: b.resource,
            key: b.key,
            body: answer.body,
          });
          return json({ status: answer.status, put });
        } catch (e) {
          return json({ rejected: rejection(e) });
        }
      }
      case "/text": {
        try {
          const answer = await fetch(q("url"));
          return json({ status: answer.status, text: await answer.text() });
        } catch (e) {
          return json({ rejected: rejection(e) });
        }
      }
      case "/at-load":
        return json({ atLoad: await atLoad });
      case "/after-request": {
        const target = q("url");
        setTimeout(async () => {
          try {
            const answer = await fetch(target);
            console.log("AFTER-RESOLVED " + answer.status);
          } catch (e) {
            console.log("AFTER-REJECTED " + JSON.stringify(rejection(e)));
          }
        }, 300);
        return json({ started: true });
      }
      case "/many": {
        const n = Number(q("n") || "1");
        const target = q("url");
        const began = Date.now();
        const results = await Promise.all(
          Array.from({ length: n }, async (_, i) => {
            const at = Date.now();
            try {
              const answer = await fetch(target + (target.includes("?") ? "&" : "?") + "i=" + i);
              await answer.text();
              return { i, ok: answer.status, ms: Date.now() - at };
            } catch (e) {
              return { i, rejected: rejection(e), ms: Date.now() - at };
            }
          }),
        );
        return json({ results, totalMs: Date.now() - began });
      }
      case "/whoami":
        return json(ctx.user());
      default:
        return json({ route: url.pathname }, 404);
    }
  },
};
