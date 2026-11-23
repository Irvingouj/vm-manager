// An Application for the request-body tests (blackbox/tests/body_request.rs,
// CONTRACT.md "What an Application is handed"). It never calls the host
// interface; every route says what it did with the request's body, so that a
// client can tell when the body was read, and when it was not.
//
// Routes:
//   /never-reads?status=<n>  answers `never-reads answered <status>` with that
//                            status (200 without one); the body is never
//                            touched, never asked for. If the platform has
//                            read it before the code, the answer cannot come
//                            while a client holds the body back.
//   /first-bytes?n=<n>       reads the body until it has n bytes (however
//                            many pulls that takes) and answers with exactly
//                            those bytes as the response's body; the rest of
//                            the request's body is never read. A client that
//                            holds the rest back still gets them, only if the
//                            body reaches the code as it arrives.
//   /sleep?ms=<n>            never touches the body; waits n ms, then answers
//                            200 `slept`. A body held back meanwhile must not
//                            fail: nothing is reading it.
//   /read-one-then-sleep?ms=<n>
//                            asks for the body (reads one chunk, prints
//                            `asked for the body`), then waits n ms without
//                            reading any more, prints `done waiting` and
//                            answers 200 `slept`. A body that goes quiet
//                            meanwhile is one the platform is waiting for.
//   /read-all                prints one line, then reads the body to its end
//                            (or until the stream fails) and answers 200 with
//                            { bytes, error }. A body the platform fails
//                            (past the bound, or no byte for 10 seconds) is
//                            answered by the platform whatever this returned.
//                            When the stream fails it prints `the body failed`
//                            before it answers.

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);

    if (url.pathname === "/never-reads") {
      const status = Number(url.searchParams.get("status") ?? "200") || 200;
      return new Response(`never-reads answered ${status}`, { status });
    }

    if (url.pathname === "/sleep") {
      const ms = Number(url.searchParams.get("ms") ?? "0") || 0;
      await new Promise((resolve) => setTimeout(resolve, ms));
      return new Response("slept");
    }

    if (url.pathname === "/read-one-then-sleep") {
      const ms = Number(url.searchParams.get("ms") ?? "0") || 0;
      const reader = request.body.getReader();
      await reader.read();
      console.log("asked for the body");
      await new Promise((resolve) => setTimeout(resolve, ms));
      console.log("done waiting");
      return new Response("slept");
    }

    if (url.pathname === "/first-bytes") {
      const want = Number(url.searchParams.get("n") ?? "1") || 1;
      const first = new Uint8Array(want);
      const reader = request.body.getReader();
      let have = 0;
      while (have < want) {
        const { done, value } = await reader.read();
        if (done) break;
        const take = Math.min(value.length, want - have);
        first.set(value.subarray(0, take), have);
        have += take;
      }
      return new Response(first.subarray(0, have));
    }

    if (url.pathname === "/read-all") {
      console.log("reading the body");
      const reader = request.body.getReader();
      let bytes = 0;
      let error = null;
      for (;;) {
        try {
          const { done, value } = await reader.read();
          if (done) break;
          bytes += value.length;
        } catch (e) {
          error = String((e && e.message) || e);
          // Printed so that a test can see the stream failed for the code.
          console.log("the body failed");
          break;
        }
      }
      return new Response(JSON.stringify({ bytes, error }), {
        headers: { "content-type": "application/json" },
      });
    }

    return new Response("no such route", { status: 404 });
  },
};
