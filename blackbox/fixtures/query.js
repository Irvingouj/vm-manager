// An honest Application that runs the host's `query` operation as the test
// asks, and answers what the host said. It adds nothing to the arguments:
// each call's arguments are passed to env.VM.invoke("query", ...) exactly
// as the test sent them.
//
// POST / with JSON:
//   { "calls": [ <arguments>, ... ],
//     "mode": "sequential" | "together" | "unawaited",
//     "summary": true | false,
//     "pauseMs": <n> }
//
//   sequential  each call awaited before the next is made (the default)
//   together    every call made before any is awaited
//   unawaited   every call made, none awaited: answers { fired: <n> } at once
//   summary     an answer { ok: ... } is replaced by
//               { ok_summary: { columns, row_count, bytes } }, bytes being
//               the length of its rows as JSON (for results too large to
//               send back)
//   pauseMs     a pause between calls (sequential and together)
//
// Answers { answers: [ <what invoke resolved to, or { threw: <message> }> ],
//           user: ctx.user() }.
//
// GET /whoami answers the host's whoami.

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const message = (e) => String((e && e.message) || e);

function call(args) {
  try {
    return Promise.resolve(env.VM.invoke("query", args)).then(
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
        bytes: JSON.stringify(answer.ok.rows).length,
      },
    };
  }
  return answer;
}

globalThis.default = {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/whoami") {
      return json({ whoami: await env.VM.invoke("whoami", {}) });
    }
    const body = await request.json();
    const calls = body.calls || [];
    const pause = Number(body.pauseMs || 0);
    const mode = body.mode || "sequential";

    if (mode === "unawaited") {
      for (const args of calls) call(args);
      return json({ fired: calls.length, user: ctx.user() });
    }

    let answers = [];
    if (mode === "together") {
      const pending = [];
      for (const args of calls) {
        pending.push(call(args));
        if (pause) await sleep(pause);
      }
      answers = await Promise.all(pending);
    } else {
      for (const args of calls) {
        answers.push(await call(args));
        if (pause) await sleep(pause);
      }
    }
    if (body.summary) answers = answers.map(summarize);
    return json({ answers, user: ctx.user() });
  },
};
