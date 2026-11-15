// An Application that prints what the test asks it to, so that the tests of
// logs can say what each log entry must hold. The tests replace %MARKER%
// and %AT_LOAD% before sending the bundle as a Version (blackbox/src/logs.rs,
// `prints_bundle`): each Version answers its marker, and its top level
// prints %AT_LOAD% lines while it loads, with no request in flight. Left as
// they are, the marker is "%MARKER%" and nothing is printed while loading.
//
// Routes:
//   GET  <any path>   answers { marker } and prints nothing
//   POST <any path>   with JSON
//                       { "print": [ { "level": "log", "message": "...",
//                                      "repeat": <n>, "times": <n> } ],
//                         "throw": "<message>", "status": <n> }
//                     prints each item `times` times (1 when left out), its
//                     message repeated `repeat` times (1 when left out), with
//                     console[level] (`log` when left out); then throws an
//                     Error with the message `throw` from the function
//                     `throwFromTheNamedFunction` when it is given; else
//                     answers { marker } with `status` (200 when left out).

const MARKER = "%MARKER%";
const AT_LOAD = Number("%AT_LOAD%") || 0;

for (let i = 0; i < AT_LOAD; i++) {
  console.log(`${MARKER} loading ${i}`);
}

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

function throwFromTheNamedFunction(message) {
  throw new Error(message);
}

globalThis.default = {
  async fetch(request) {
    if (request.method !== "POST") {
      return json({ marker: MARKER });
    }
    const body = await request.json();
    for (const item of body.print || []) {
      const level = item.level || "log";
      const message = String(item.message).repeat(item.repeat || 1);
      for (let i = 0; i < (item.times || 1); i++) {
        console[level](message);
      }
    }
    if (body.throw) {
      throwFromTheNamedFunction(body.throw);
    }
    return json({ marker: MARKER }, body.status || 200);
  },
};
