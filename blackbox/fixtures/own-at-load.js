// An Application that calls the host at its top level, while it loads,
// when no request is in flight: `query` and `execute` on its own database as
// the User and as the Application, and `query` on the Resource `devops` as
// the Application. CONTRACT.md: each is refused with `no-request`, which is
// checked before anything else. The handler reports what each call got.
//
// GET / answers { atLoad: { <name>: { answered } | { rejected } | { threw } },
//                 user: ctx.user() }.

const tries = {
  own_query_as_user: ["query", { own: true, as: "user", sql: "select 1 as one" }],
  own_query_as_application: ["query", { own: true, as: "application", sql: "select 1 as one" }],
  own_execute_as_application: [
    "execute",
    { own: true, as: "application", sql: "insert into notes (body) values ('written at load') returning id" },
  ],
  resource_as_application: ["query", { resource: "devops", as: "application", sql: "select 1 as one" }],
};

const message = (e) => String((e && e.message) || e);

const atLoad = {};
for (const [name, [operation, args]] of Object.entries(tries)) {
  try {
    atLoad[name] = Promise.resolve(env.VM.invoke(operation, args)).then(
      (answer) => ({ answered: answer }),
      (e) => ({ rejected: message(e) }),
    );
  } catch (e) {
    atLoad[name] = Promise.resolve({ threw: message(e) });
  }
}

globalThis.default = {
  async fetch(request, ctx) {
    const answers = {};
    for (const name of Object.keys(atLoad)) answers[name] = await atLoad[name];
    return new Response(JSON.stringify({ atLoad: answers, user: ctx.user() }), {
      headers: { "content-type": "application/json" },
    });
  },
};
