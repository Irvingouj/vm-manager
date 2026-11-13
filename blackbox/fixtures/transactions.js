// An Application that runs the host's transaction operations (CONTRACT.md,
// "Transactions: Through the raw interface") as the test scripts them, and
// answers what the host said. It adds nothing to the arguments but the ids
// it was told to keep: each step's arguments are passed to
// env.VM.invoke(<operation>, ...) as the test sent them, with every
// { "$ref": "<name>" } inside them replaced by the value kept under that
// name.
//
// POST / with JSON { "steps": [ <step>, ... ] }, run one after another:
//
//   { "op": "<operation>", "args": { ... },
//     "save": "<name>",       keep answer.ok.tx under <name>
//     "saveRow": "<name>" }   keep answer.ok.rows[0][0] under <name>
//   { "sleep": <ms> }         wait, with no statement running
//   { "together": [ <step>, ... ] }
//                             every inner step made before any is awaited;
//                             its answer is the array of theirs
//   { "throw": "<message>" }  throw an Error, not caught: the request fails
//   { "fire": <step> }        make the inner step and do not await it;
//                             answers { fired: true }
//   { "later": { "delayMs": <n>, "steps": [ <step>, ... ] } }
//                             arms a timer that runs these steps after the
//                             request has ended, with the names kept so far,
//                             and prints "LEFTOVER <json of their answers>"
//                             (printing is the only way such code could tell
//                             anyone); answers { armed: true }
//
// A step's answer is what invoke resolved to, or { threw: <message> };
// { slept: <ms> } for a sleep.
//
// Answers { answers: [ ... ], kept: { <name>: <value> }, user: ctx.user() }.

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const message = (e) => String((e && e.message) || e);

function substitute(value, kept) {
  if (Array.isArray(value)) return value.map((v) => substitute(v, kept));
  if (value && typeof value === "object") {
    const keys = Object.keys(value);
    if (keys.length === 1 && keys[0] === "$ref") {
      return Object.prototype.hasOwnProperty.call(kept, value.$ref) ? kept[value.$ref] : null;
    }
    const out = {};
    for (const key of keys) out[key] = substitute(value[key], kept);
    return out;
  }
  return value;
}

function invoke(operation, args) {
  try {
    return Promise.resolve(env.VM.invoke(operation, args)).then(
      (answer) => answer,
      (e) => ({ threw: message(e) }),
    );
  } catch (e) {
    return Promise.resolve({ threw: message(e) });
  }
}

async function run(step, kept) {
  if (step.sleep !== undefined) {
    await sleep(Number(step.sleep));
    return { slept: Number(step.sleep) };
  }
  if (step.together !== undefined) {
    return await Promise.all(step.together.map((inner) => run(inner, kept)));
  }
  if (step.fire !== undefined) {
    run(step.fire, kept).catch(() => {});
    return { fired: true };
  }
  if (step.throw !== undefined) {
    throw new Error(String(step.throw));
  }
  if (step.later !== undefined) {
    const steps = step.later.steps || [];
    const snapshot = Object.assign({}, kept);
    setTimeout(async () => {
      const answers = [];
      for (const s of steps) {
        try {
          answers.push(await run(s, snapshot));
        } catch (e) {
          answers.push({ threw: message(e) });
        }
      }
      console.log("LEFTOVER " + JSON.stringify(answers));
    }, Number(step.later.delayMs || 0));
    return { armed: true };
  }
  const answer = await invoke(step.op, substitute(step.args, kept));
  if (step.save && answer && answer.ok && answer.ok.tx !== undefined) {
    kept[step.save] = answer.ok.tx;
  }
  if (step.saveRow && answer && answer.ok && Array.isArray(answer.ok.rows) && answer.ok.rows[0]) {
    kept[step.saveRow] = answer.ok.rows[0][0];
  }
  return answer;
}

globalThis.default = {
  async fetch(request, ctx) {
    const body = await request.json();
    const kept = {};
    const answers = [];
    for (const step of body.steps || []) {
      answers.push(await run(step, kept));
    }
    return json({ answers, kept, user: ctx.user() });
  },
};
