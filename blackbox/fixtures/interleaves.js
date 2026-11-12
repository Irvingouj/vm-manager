// An honest Application that gives other requests every chance to run in
// the middle of its own: it reads its User, awaits the host, sleeps, reads
// again, and so on. Run by many Users at once, each must see only their own
// User throughout.
//
// Route: /?pause=<ms>  readings of ctx.user() and whoami around pauses

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

globalThis.default = {
  async fetch(request, ctx) {
    const pause = Number(new URL(request.url).searchParams.get("pause") || 0);
    const readings = [];
    const whoami = [];
    readings.push(ctx.user());
    whoami.push(await env.VM.invoke("whoami", {}));
    readings.push(ctx.user());
    await sleep(pause);
    readings.push(ctx.user());
    whoami.push(await env.VM.invoke("whoami", {}));
    await sleep(pause / 2);
    readings.push(ctx.user());
    console.log(`interleaves: I am ${ctx.user().id}`);
    return new Response(JSON.stringify({ readings, whoami }), {
      headers: { "content-type": "application/json" },
    });
  },
};
