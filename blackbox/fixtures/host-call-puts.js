// An Application that makes Storage puts with the bodies a test describes,
// through the raw interface, and answers what the host said (CONTRACT.md,
// "A host call's arguments are bounded": "A put's `body` is not arguments").
//
// POST /run { streams, steps }
//   streams  how many streams of its own the code opens first
//            (`__responseStreamCreate()`), and keeps open to the end of the
//            request: with 16, the request has none left
//   steps    one after another, each
//            { op: "storage.put", resource, key, keyLen?, body, extra? }
//              keyLen   when given, the key is `a` repeated this many times
//              body     { kind: "bytes", n }    a Uint8Array of n bytes
//                       { kind: "string", parts: [[text, repeat], ...] }
//                                               the parts joined
//                       { kind: "none" }        no body at all
//              extra    { resource: <name> } overrides, nothing more
//            Answers { answer } or { threw }.
// Answers { streams: [ok | threw], results: [ ... ] }.

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const message = (e) => String((e && e.message) || e);

function bodyOf(spec) {
  if (spec.kind === "bytes") {
    const bytes = new Uint8Array(spec.n);
    for (let i = 0; i < bytes.length; i++) bytes[i] = 65 + (i % 26);
    return bytes;
  }
  if (spec.kind === "string") return spec.parts.map(([text, n]) => text.repeat(n)).join("");
  return null;
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function put(resource, key, body) {
  try {
    return { answer: await env.VM.invoke("storage.put", { resource, key, body }) };
  } catch (e) {
    return { threw: message(e) };
  }
}

// /forty { resource }: forty puts one after another, each awaited, bytes of
// 100 KiB and strings of 70,000 characters alternating; then sixteen puts of
// bytes started together; then the sizes of the last files by `storage.head`.
async function forty(b) {
  const results = [];
  for (let i = 0; i < 40; i++) {
    const spec = i % 2 === 0 ? { kind: "bytes", n: 100 * 1024 } : { kind: "string", parts: [["a", 70000]] };
    results.push(await put(b.resource, "forty/" + i, bodyOf(spec)));
  }
  const together = await Promise.all(
    Array.from({ length: 16 }, (_, i) => put(b.resource, "together/" + i, bodyOf({ kind: "bytes", n: 100 * 1024 }))),
  );
  const heads = [];
  for (const key of ["forty/38", "forty/39", "together/15"]) {
    heads.push(await env.VM.invoke("storage.head", { resource: b.resource, key }));
  }
  return { results, together, heads };
}

// /cap { n, how, afterCreate, steps }: makes `n` streams, each ended
// (`how` "end": __responseStreamEnd, still open until read) or cancelled
// (otherwise: __nativeStreamCancel); of the code's
// own directly; reports where the first create threw (and what), then, when
// `afterCreate`, one more create; then the puts of `steps`.
async function cap(b) {
  const out = { made: 0, threw: null, after: null, results: [] };
  for (let i = 0; i < b.n; i++) {
    try {
      const id = globalThis.__responseStreamCreate();
      if (b.how === "end") globalThis.__responseStreamEnd(id);
      else globalThis.__nativeStreamCancel(id);
      out.made++;
    } catch (e) {
      out.threw = { at: i, message: message(e) };
      break;
    }
  }
  if (b.afterCreate) {
    try {
      globalThis.__nativeStreamCancel(globalThis.__responseStreamCreate());
      out.after = "ok";
    } catch (e) {
      out.after = { threw: message(e) };
    }
  }
  for (const step of b.steps || []) out.results.push(await put(step.resource, step.key, bodyOf(step.body)));
  return out;
}

// /cancel { resource, key, keyLen, mode }: a put whose body is a stream of
// the code's own; `mode` "pending" makes `pull` return a promise that never
// settles. Reports the put's answer and how often pull and cancel were
// called when it was answered and 200 ms later.
async function cancelled(b) {
  let pulls = 0;
  let cancels = 0;
  const stream = new ReadableStream({
    pull(c) {
      pulls++;
      if (b.mode === "pending") return new Promise(() => {});
      c.enqueue(new Uint8Array(1024));
    },
    cancel() {
      cancels++;
    },
  });
  const began = Date.now();
  const result = await Promise.race([
    put(b.resource, b.keyLen ? "a".repeat(b.keyLen) : b.key, stream),
    sleep(5000).then(() => ({ timedOut: true })),
  ]);
  const out = { result, ms: Date.now() - began, pullsAtAnswer: pulls, cancelsAtAnswer: cancels };
  await sleep(200);
  out.pullsLater = pulls;
  out.cancelsLater = cancels;
  return out;
}

globalThis.default = {
  async fetch(request) {
    const body = await request.json();
    const route = new URL(request.url).pathname;
    if (route === "/forty") return json(await forty(body));
    if (route === "/cap") return json(await cap(body));
    if (route === "/cancel") return json(await cancelled(body));
    const streams = [];
    for (let i = 0; i < (body.streams || 0); i++) {
      try {
        globalThis.__responseStreamCreate();
        streams.push("ok");
      } catch (e) {
        streams.push({ threw: message(e) });
      }
    }
    const results = [];
    for (const step of body.steps || []) {
      const args = {
        resource: step.resource,
        key: step.keyLen ? "a".repeat(step.keyLen) : step.key,
        body: bodyOf(step.body),
      };
      try {
        results.push({ answer: await env.VM.invoke(step.op || "storage.put", args) });
      } catch (e) {
        results.push({ threw: message(e) });
      }
    }
    return json({ streams, results });
  },
};
