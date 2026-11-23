// An Application that makes host calls of the sizes and in the numbers a test
// asks for (CONTRACT.md, "A host call's arguments are bounded"), through the
// raw interface, and tells what the host answered and when.
//
// A call, `c`, is { op, spec, times? }: `env.VM.invoke(c.op, <arguments>)`,
// made `times` times (default 1; each its own call). Its arguments are built
// from `spec`:
//
//   { plain: <json> }        this value as it is
//   { undefined: true }      no arguments at all
//   { pad: { bytes: N, base: {..}, path: <key> | "params", multibyte: bool } }
//                            `base` plus a string of the code's own, made so
//                            that JSON.stringify(arguments) is exactly N
//                            bytes as UTF-8: the string is the key `path` (default
//                            `pad`) of the arguments, or the one element of
//                            `params`.
//                            `multibyte` fills it with characters of 3, 2 and
//                            4 bytes (and `a`s to the byte). The same string
//                            is shared by every call of the same size.
//   { array0: N }           [0, 0, ...] of N zeros (N + 1 values)
//   { sleepZeros: N, seconds: S }
//                           a query of the own database that sleeps S seconds
//                           with N parameters of 0
//   { keys: N }             an object of (N - 1) / 2 keys, each 0 (N values)
//   { zeros: N }             `{ z: [0, 0, ...] }` of exactly N values by the
//                            contract's count (the object, the key `z`, the
//                            array and its zeros): its text is far below
//                            8 MiB for any N the tests use
//
// What a call gives back: { answer } or { threw }, `at` (Date.now() when the call
// settled, compare with `startedAt`), `bytes` (UTF-8 bytes of
// JSON.stringify of its arguments, measured here) and `values` (counted as
// the contract counts them). `null` for a call not yet settled.
//
// Routes (POST, JSON body), all answer { results, ...}:
//   /seq       { calls }  one after another, each awaited
//   /together  { calls }  every call made before any is awaited
//   /hold      { held, extra, settleMs, awaitHeld, after }
//              makes every `held` call without awaiting, waits `settleMs`
//              (default 300), snapshots which have settled (`heldNow`), makes
//              the `extra` calls one after another (`extra`), then, when
//              `awaitHeld`, awaits the held ones (`heldAll`), then makes the
//              `after` calls one after another (`after`).
//              `startedAt` is Date.now() at the start of the request.
//   /leave     { op, spec, afterMs }
//              answers at once and makes this call from a timer, when no
//              request is in flight; prints "LATE <json of the answer>"

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const message = (e) => String((e && e.message) || e);
const encoder = new TextEncoder();
const utf8 = (s) => encoder.encode(s).length;

// The contract's count of values: each string, number, boolean, null, array
// and object, and each key of an object.
function values(v) {
  if (v === undefined) return 0;
  if (Array.isArray(v)) return 1 + v.reduce((n, x) => n + values(x), 0);
  if (v !== null && typeof v === "object") {
    let n = 1;
    for (const k of Object.keys(v)) {
      if (v[k] === undefined) continue;
      n += 1 + values(v[k]);
    }
    return n;
  }
  return 1;
}

let pads = new Map();
let padBytes = 0;
function padOf(length, multibyte) {
  const key = length + ":" + multibyte;
  let s = pads.get(key);
  if (s === undefined) {
    // Kept so that calls of one size share one string; dropped before it
    // would hold much of the request's own memory.
    if (padBytes + length > 24 * 1024 * 1024) {
      pads = new Map();
      padBytes = 0;
    }
    padBytes += length;
    if (multibyte) {
      const unit = "€é\u{1F600}a"; // 3 + 2 + 4 + 1 bytes
      const units = Math.floor(length / 10);
      s = unit.repeat(units) + "a".repeat(length - units * 10);
    } else {
      s = "a".repeat(length);
    }
    pads.set(key, s);
  }
  return s;
}

const zeroArrays = new Map();
function zerosOf(n) {
  let a = zeroArrays.get(n);
  if (a === undefined) {
    a = new Array(n).fill(0);
    zeroArrays.set(n, a);
  }
  return a;
}

function build(spec) {
  if (spec.undefined) return undefined;
  if (spec.plain !== undefined) return spec.plain;
  if (spec.zeros !== undefined) return { z: zerosOf(spec.zeros - 3) };
  if (spec.array0 !== undefined) return zerosOf(spec.array0);
  if (spec.sleepZeros !== undefined) {
    // A query that waits, with `n` parameters of 0 (they count as values).
    return {
      own: true,
      as: "application",
      sql: "select pg_sleep(" + spec.seconds + ")",
      params: zerosOf(spec.sleepZeros),
    };
  }
  if (spec.keys !== undefined) {
    const o = {};
    for (let i = 0; i < (spec.keys - 1) / 2; i++) o["k" + i] = 0;
    return o;
  }
  const p = spec.pad;
  const path = p.path || "pad";
  const withPad = (s) => (path === "params" ? { ...p.base, params: [s] } : { ...p.base, [path]: s });
  const overhead = utf8(JSON.stringify(withPad("")));
  const length = p.bytes - overhead;
  if (length < 0) throw new Error("the arguments are larger than " + p.bytes + " bytes without the pad");
  const args = withPad(padOf(length, !!p.multibyte));
  return args;
}

function measure(args) {
  if (args === undefined) return { bytes: 0, values: 0 };
  return { bytes: utf8(JSON.stringify(args)), values: values(args) };
}

function expand(calls) {
  const out = [];
  for (const c of calls || []) for (let i = 0; i < (c.times || 1); i++) out.push(c);
  return out;
}

function make(c) {
  let args;
  let measured;
  try {
    args = build(c.spec);
    measured = c.noMeasure ? { bytes: null, values: null } : measure(args);
  } catch (e) {
    return Promise.resolve({ threw: message(e), at: Date.now() });
  }
  const done = (r) => ({ ...r, at: Date.now(), ...measured });
  try {
    return Promise.resolve(env.VM.invoke(c.op, args)).then(
      (answer) => done({ answer }),
      (e) => done({ threw: message(e) }),
    );
  } catch (e) {
    return Promise.resolve(done({ threw: message(e) }));
  }
}

async function sequential(calls) {
  const out = [];
  for (const c of expand(calls)) out.push(await make(c));
  return out;
}

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const startedAt = Date.now();
    if (request.method !== "POST") return json({ user: ctx.user() });
    const body = await request.json();

    if (url.pathname === "/seq") {
      return json({ results: await sequential(body.calls), startedAt });
    }

    if (url.pathname === "/together") {
      const results = await Promise.all(expand(body.calls).map(make));
      return json({ results, startedAt });
    }

    if (url.pathname === "/hold") {
      const held = expand(body.held);
      const heldNow = held.map(() => null);
      const promises = held.map((c, i) =>
        make(c).then((r) => {
          heldNow[i] = r;
          return r;
        }),
      );
      await sleep(body.settleMs === undefined ? 300 : body.settleMs);
      const snapshot = heldNow.slice();
      const extra = await sequential(body.extra);
      const out = { startedAt, heldNow: snapshot, extra };
      if (body.awaitHeld) out.heldAll = await Promise.all(promises);
      out.after = await sequential(body.after);
      return json(out);
    }

    if (url.pathname === "/leave") {
      const c = { op: body.op, spec: body.spec };
      setTimeout(async () => {
        const r = await make(c);
        console.log("LATE " + JSON.stringify({ answer: r.answer, threw: r.threw }));
      }, body.afterMs || 100);
      return json({ left: true });
    }

    return new Response("no such route", { status: 404 });
  },
};
