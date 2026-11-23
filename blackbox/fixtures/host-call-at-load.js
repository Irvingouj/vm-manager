// An Application that makes host calls at its top level, while it loads,
// when no request is in flight (CONTRACT.md, "A host call's arguments are
// bounded": the bounds are decided "with or without a request in flight").
// The handler reports what each got:
//   over    `nonesuch` with arguments of 8 MiB and one byte
//   within  `nonesuch` with arguments of 1 KiB
//   many    `nonesuch` with 262,145 values

const message = (e) => String((e && e.message) || e);
const run = (operation, args) => {
  try {
    return env.VM.invoke(operation, args).then(
      (answer) => ({ answered: answer }),
      (e) => ({ rejected: message(e) }),
    );
  } catch (e) {
    return Promise.resolve({ threw: message(e) });
  }
};

// `{ "pad": "aaa..." }` is 10 bytes of text and the pad's.
const OVER = 8 * 1024 * 1024 + 1;
const atLoad = {
  over: run("nonesuch", { pad: "a".repeat(OVER - 10) }),
  within: run("nonesuch", { pad: "a".repeat(1024 - 10) }),
  // `{ z: [0 x n] }` is n + 3 values.
  many: run("nonesuch", { z: new Array(262145 - 3).fill(0) }),
};

globalThis.default = {
  async fetch(request, ctx) {
    return new Response(
      JSON.stringify({
        over: await atLoad.over,
        within: await atLoad.within,
        many: await atLoad.many,
        user: ctx.user(),
      }),
      { headers: { "content-type": "application/json" } },
    );
  },
};
