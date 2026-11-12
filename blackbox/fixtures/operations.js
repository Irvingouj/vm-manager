// An Application that runs the host's operations on a shared database as
// the test asks (`query`, `execute`, or any other name), and answers what
// the host said. It adds nothing to the arguments: each call's arguments
// are passed to env.VM.invoke(<operation>, ...) exactly as the test sent
// them. It is honest; the hostility, where there is any, is in the SQL the
// test sends through it.
//
// POST / with JSON:
//   { "calls": [ { "operation": "execute", "arguments": <arguments> }, ... ],
//     "mode": "sequential" | "together",
//     "summary": true | false,
//     "pauseMs": <n> }
//
//   sequential  each call awaited before the next is made (the default)
//   together    every call made before any is awaited
//   summary     an answer { ok: ... } is replaced by
//               { ok_summary: { columns, row_count, rows_affected, bytes } },
//               bytes being the length of its rows as JSON (for results too
//               large to send back)
//   pauseMs     a pause between calls
//
// Answers { answers: [ <what invoke resolved to, or { threw: <message> }> ],
//           user: ctx.user() }.

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const message = (e) => String((e && e.message) || e);

function call(c) {
  try {
    return Promise.resolve(env.VM.invoke(c.operation, c.arguments)).then(
      (answer) => answer,
      (e) => ({ threw: message(e) }),
    );
  } catch (e) {
    return Promise.resolve({ threw: message(e) });
  }
}

function summarize(answer) {
  if (answer && answer.ok && Array.isArray(answer.ok.rows)) {
    return {
      ok_summary: {
        columns: answer.ok.columns,
        row_count: answer.ok.row_count,
        rows_affected: answer.ok.rows_affected,
        bytes: JSON.stringify(answer.ok.rows).length,
      },
    };
  }
  return answer;
}

globalThis.default = {
  async fetch(request, ctx) {
    const body = await request.json();
    const calls = body.calls || [];
    const pause = Number(body.pauseMs || 0);
    let answers = [];
    if (body.mode === "together") {
      const pending = [];
      for (const c of calls) {
        pending.push(call(c));
        if (pause) await sleep(pause);
      }
      answers = await Promise.all(pending);
    } else {
      for (const c of calls) {
        answers.push(await call(c));
        if (pause) await sleep(pause);
      }
    }
    if (body.summary) answers = answers.map(summarize);
    return json({ answers, user: ctx.user() });
  },
};
