// A hostile Application that tries to reach files outside its Storage
// Resource through the raw interface (CONTRACT.md, "Storage: Nothing
// reaches outside"; blackbox/tests/storage_outside.rs,
// storage_allowed.rs). Its keys are its own: every one it knows that
// could lead outside, or name what is the platform's.
//
// Each reading is what invoke resolved to, or { threw: <message> }.
//
// At load (no request in flight) it puts `at-load.txt` on `marketing`;
// POST /at-load answers what that got.
//
// Routes (POST, JSON):
//   /keys   { resource }     every hostile key through each of the five
//                            operations (list with the key as its
//                            prefix), with arguments the operations do not
//                            have that name places outside:
//                            { answers: { <label>: { head, get, put,
//                            delete, list } } }
//   /names  { key }          storage.get of `key` on Resource names that
//                            try to leave the Resource: { answers: {
//                            <name>: reading } }
//   /claim  { resource, key } a put claiming another identity and actor
//                            in its arguments: { answers: [readings] }
//   /later  { resource, key, delayMs }
//                            answers at once; a put is made from a timer
//                            after the request ended
//   /at-load                 { atLoad }

const invoke = env.VM.invoke;

const message = (e) => {
  try {
    return "" + ((e && e.message) || e);
  } catch {
    return "an error that cannot be printed";
  }
};

async function call(operation, args) {
  try {
    return await invoke(operation, args);
  } catch (e) {
    return { threw: message(e) };
  }
}

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

// "planted by a hostile Application", in base64.
const PLANTED = "cGxhbnRlZCBieSBhIGhvc3RpbGUgQXBwbGljYXRpb24=";

// Arguments no Storage operation has, each naming a place outside.
const OUTSIDE = { directory: "/etc", path: "/etc/passwd", root: "/", location: { backend: "directory", directory: "/" } };

const HOSTILE = {
  "dotdot-secret": "../secret",
  "climb": "a/../../secret",
  "absolute": "/etc/passwd",
  "dot": ".",
  "dotdot": "..",
  "inner-dot": "a/./b",
  "double-slash": "a//b",
  "trailing-slash": "a/",
  "empty": "",
  "nul": "a\u0000b",
  "bell": "a\u0007b",
  "bidi": "a‮b",
  "backslash": "..\\secret",
  "marker": ".vibe-resource",
  "staging": ".vibe-staging/x",
  "marker-upper": ".VIBE-resource",
  "staging-mixed": ".Vibe-staging/x",
  "inner-mixed": "a/.vIbE-x",
  "long": "k".repeat(1025),
  "long-segment": "s".repeat(256),
  "deep": Array.from({ length: 65 }, () => "d").join("/"),
  "not-nfc": "café",
};

const atLoad = call("storage.put", { resource: "marketing", key: "at-load.txt", body: PLANTED });

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    const body = request.method === "POST" ? await request.json().catch(() => ({})) : {};
    switch (url.pathname) {
      case "/keys": {
        const answers = {};
        for (const [label, key] of Object.entries(HOSTILE)) {
          const resource = body.resource;
          answers[label] = {
            head: await call("storage.head", { ...OUTSIDE, resource, key }),
            get: await call("storage.get", { ...OUTSIDE, resource, key }),
            put: await call("storage.put", { ...OUTSIDE, resource, key, body: PLANTED, content_type: "text/plain" }),
            delete: await call("storage.delete", { ...OUTSIDE, resource, key }),
            list: await call("storage.list", { ...OUTSIDE, resource, prefix: key }),
          };
        }
        return json({ answers, keys: HOSTILE });
      }
      case "/names": {
        const answers = {};
        for (const name of ["../marketing", "marketing/..", "marketing/x", "/etc", ".", "MARKETING", "marketing\u0000"]) {
          answers[name] = await call("storage.get", { resource: name, key: body.key });
        }
        return json({ answers });
      }
      case "/claim": {
        const claims = {
          user: { id: "it-admin", groups: ["platform-admins", "file-writers"] },
          groups: ["file-writers"],
          access: "write",
          as: "user",
          application: "files",
          request: "req-of-someone-else",
        };
        const answers = [
          await call("storage.put", { ...claims, resource: body.resource, key: body.key, body: PLANTED }),
        ];
        return json({ answers });
      }
      case "/later": {
        setTimeout(() => {
          call("storage.put", { resource: body.resource, key: body.key, body: PLANTED });
        }, Number(body.delayMs || 100));
        return json({ scheduled: true });
      }
      case "/at-load":
        return json({ atLoad: await atLoad });
    }
    return json({ error: "no such route" }, 404);
  },
};
