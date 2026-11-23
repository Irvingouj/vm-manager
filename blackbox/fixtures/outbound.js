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
//   GET /flood?url=&n=&pad=&settle=
//       n calls made in a loop, none awaited, each with a header of `pad`
//       bytes; then every one awaited (or, with `settle` ms, only that long):
//       { resolved, refused: { reason: n }, failed: { kind: n }, pending }.
//   GET /hold-then-read?url=&ms=
//       the answer's headers, then nothing for `ms`, then the body read:
//       { status, readFailed?, length }.

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
    case "stalling": {
      // 1 KiB, then nothing for ever.
      let sent = false;
      return new ReadableStream({
        pull(controller) {
          if (sent) return new Promise(() => {});
          sent = true;
          controller.enqueue(pattern(1024, 0));
          return undefined;
        },
      });
    }
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
  if (spec.gen) {
    // `count` headers of `size` bytes each, made here.
    const made = {};
    const value = "h".repeat(spec.gen.size);
    for (let i = 0; i < spec.gen.count; i++) made["x-gen-" + i] = value;
    init.headers = made;
  }
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
      case "/flood": {
        const n = Number(q("n") || "1");
        const pad = "a".repeat(Number(q("pad") || "0"));
        const target = q("url");
        const counts = { resolved: 0, refused: {}, failed: {}, other: 0, readFailed: 0, pending: 0 };
        const bump = (m, k) => {
          m[k] = (m[k] || 0) + 1;
        };
        let settled = 0;
        const calls = [];
        for (let i = 0; i < n; i++) {
          const init = pad ? { headers: { "x-pad": pad } } : {};
          calls.push(
            fetch(target + "?i=" + i, init).then(
              async (a) => {
                try {
                  await a.text();
                  counts.resolved++;
                } catch (e) {
                  counts.readFailed++;
                }
                settled++;
              },
              (e) => {
                if (e && e.refused) bump(counts.refused, e.refused.refusal.reason);
                else if (e && e.failure) bump(counts.failed, e.failure.kind);
                else counts.other++;
                settled++;
              },
            ),
          );
        }
        if (q("settle")) await Promise.race([Promise.all(calls), sleep(Number(q("settle")))]);
        else await Promise.all(calls);
        counts.pending = n - settled;
        return json(counts);
      }
      case "/hold-then-read": {
        try {
          const answer = await fetch(q("url"));
          await sleep(Number(q("ms") || "0"));
          const out = { status: answer.status, length: 0 };
          try {
            const reader = answer.body.getReader();
            for (;;) {
              const { done, value } = await reader.read();
              if (done) break;
              out.length += value.length;
            }
          } catch (e) {
            out.readFailed = message(e);
          }
          return json(out);
        } catch (e) {
          return json({ rejected: rejection(e) });
        }
      }
      case "/native-hand": {
        // Steps in `?steps=<json>`; the request's own body is left unread so
        // that its native stream can be named. Values are named: a string
        // "@name" (or "@name.member") in an argument is replaced by it.
        const steps = JSON.parse(q("steps"));
        const env = {};
        const outs = {};
        const started = {};
        const lookup = (ref) => ref.slice(1).split(".").reduce((v, k) => (v == null ? v : v[k]), env);
        const resolveDeep = (v) => {
          if (typeof v === "string" && v.startsWith("@")) return lookup(v);
          if (Array.isArray(v)) return v.map(resolveDeep);
          if (v && typeof v === "object") {
            const o = {};
            for (const k of Object.keys(v)) o[k] = resolveDeep(v[k]);
            return o;
          }
          return v;
        };
        const call = (args) =>
          new Promise((resolve) => {
            const timer = setTimeout(() => resolve({ timeout: true }), 12000);
            try {
              globalThis.__nativeBindingInvoke(
                "__vm_fetch",
                "fetch",
                args,
                (v) => {
                  clearTimeout(timer);
                  resolve({ resolved: v });
                },
                (e) => {
                  clearTimeout(timer);
                  resolve({ rejected: message(e) });
                },
              );
            } catch (e) {
              clearTimeout(timer);
              resolve({ threw: message(e) });
            }
          });
        const note = (name, out) => {
          outs[name] = out;
          env[name] = out;
          const body = out.resolved && out.resolved.ok && out.resolved.ok.body;
          if (body && body.$stream !== undefined) env[name + "Body"] = body.$stream;
        };
        for (const step of steps) {
          try {
            if (step.create) env[step.create] = globalThis.__responseStreamCreate();
            else if (step.reqbody) env[step.reqbody] = request.body._nativeStreamId;
            else if (step.call) note(step.call, await call(resolveDeep(step.args)));
            else if (step.start) started[step.start] = call(resolveDeep(step.args));
            else if (step.await) note(step.await, await started[step.await]);
            else if (step.write !== undefined) {
              const bytes = step.text !== undefined ? enc.encode(step.text) : pattern(step.bytes || 0, 3);
              const p = globalThis.__responseStreamWrite(resolveDeep(step.write), bytes);
              if (!step.nowait) await p;
            } else if (step.end !== undefined) globalThis.__responseStreamEnd(resolveDeep(step.end));
            else if (step.sleep) await sleep(step.sleep);
          } catch (e) {
            outs["step-threw-" + Object.keys(step)[0]] = message(e);
          }
        }
        return json({ outs });
      }
      case "/globals":
        return json(Object.getOwnPropertyNames(globalThis).filter((n) => n.startsWith("__")));
      case "/native-seq": {
        // Several hand-made calls in one request; an argument object may
        // hold the string "$prev-body" as its `body`, which is replaced by
        // the body marker of the previous answer; "$request-body" by the
        // marker of this request's body when it can be had.
        const b = await request.json();
        const call = (args) =>
          new Promise((resolve) => {
            const timer = setTimeout(() => resolve({ timeout: true }), 5000);
            try {
              globalThis.__nativeBindingInvoke(
                "__vm_fetch",
                "fetch",
                args,
                (v) => {
                  clearTimeout(timer);
                  resolve({ resolved: v });
                },
                (e) => {
                  clearTimeout(timer);
                  resolve({ rejected: message(e) });
                },
              );
            } catch (e) {
              clearTimeout(timer);
              resolve({ threw: message(e) });
            }
          });
        const outs = [];
        let prev;
        for (const args of b.calls) {
          const made = { ...args };
          if (made.body === "$prev-body") made.body = prev;
          if (made.body === "$bytes") made.body = pattern(16, 1);
          if (made.body === "$arraybuffer") made.body = pattern(16, 1).buffer;
          if (made.body === "$stream") made.body = streamOf({ chunks: ["ab", "cd"] });
          if (made.body === "$request-body") made.body = request.body;
          if (made.body === "$params") made.body = new URLSearchParams([["a", "b"]]);
          const out = await call(made);
          outs.push(out);
          prev = out.resolved && out.resolved.ok ? out.resolved.ok.body : undefined;
        }
        return json({ outs });
      }
      case "/native-flood": {
        // `n` hand-made calls in a loop, none awaited, each with a header of
        // `pad` bytes; the answers counted by their shape.
        const n = Number(q("n") || "1");
        const pad = "a".repeat(Number(q("pad") || "0"));
        const target = q("url");
        const counts = {};
        let settled = 0;
        const calls = [];
        for (let i = 0; i < n; i++) {
          calls.push(
            new Promise((resolve) => {
              const done = (key) => {
                counts[key] = (counts[key] || 0) + 1;
                settled++;
                resolve();
              };
              try {
                globalThis.__nativeBindingInvoke(
                  "__vm_fetch",
                  "fetch",
                  { url: target + "?i=" + i, headers: pad ? [["x-pad", pad]] : [] },
                  (v) => {
                    if (v && v.ok) done("ok");
                    else if (v && v.error) done("error:" + v.error.kind);
                    else if (v && v.refused) done("refused:" + v.refused.refusal.reason);
                    else done("other:" + Object.keys(v || {}).join(","));
                  },
                  (e) => done("rejected"),
                );
              } catch (e) {
                done("threw");
              }
            }),
          );
        }
        if (q("settle")) await Promise.race([Promise.all(calls), sleep(Number(q("settle")))]);
        else await Promise.all(calls);
        return json({ counts, pending: n - settled });
      }
      case "/native": {
        // Calls the native binding by hand: { binding, command, args } from
        // the request's JSON; the answer or the rejection, as it comes.
        const b = await request.json();
        const out = await new Promise((resolve) => {
          const timer = setTimeout(() => resolve({ timeout: true }), b.waitMs || 5000);
          try {
            globalThis.__nativeBindingInvoke(
              b.binding === undefined ? "__vm_fetch" : b.binding,
              b.command === undefined ? "fetch" : b.command,
              b.args,
              (v) => {
                clearTimeout(timer);
                resolve({ resolved: v });
              },
              (e) => {
                clearTimeout(timer);
                resolve({ rejected: typeof e === "string" ? e : message(e), ctype: typeof e, detail: e && e.failure ? e.failure : undefined });
              },
            );
          } catch (e) {
            clearTimeout(timer);
            resolve({ threw: message(e) });
          }
        });
        return json(out);
      }
      case "/whoami":
        return json(ctx.user());
      default:
        return json({ route: url.pathname }, 404);
    }
  },
};
