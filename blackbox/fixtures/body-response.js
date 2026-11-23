// An Application for the response-body tests (blackbox/tests/body_response.rs,
// body_sent_ends.rs and body_framing.rs, CONTRACT.md "What an Application is
// handed"). It never calls the host interface.
//
// Every route takes any number of `h=<name>:<value>` query parameters: each
// is a header the code sets on its response, whatever the name (the
// framing headers included: the platform must not send them as set).
//
// Routes:
//   /echo-unread?status=<n>&type=<t>&set_length=<n>
//                    the request's own body given back unread:
//                    new Response(request.body); `set_length` is a
//                    content-length the code chooses (it may lie).
//   /echo-read-then-give-back
//                    reads one chunk of the body, releases the reader and
//                    gives the body back; `x-first-chunk` says how many
//                    bytes the code read and so no longer answers. This is
//                    the buffered path: whole, as before.
//   /made?kind=<string|bytes|stream|transform>&size=<n>&chunk=<n>
//                    a body the code makes: n bytes of `x` (a stream's
//                    chunks are at most `chunk` bytes, 64 KiB without it)
//   /text?body=<text>
//                    the text, as a string

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    const number = (name, fallback) =>
      Number(url.searchParams.get(name) ?? String(fallback)) || fallback;
    const headers = new Headers();
    for (const pair of url.searchParams.getAll("h")) {
      const at = pair.indexOf(":");
      headers.set(pair.slice(0, at), pair.slice(at + 1));
    }

    if (url.pathname === "/echo-unread") {
      const type = url.searchParams.get("type");
      if (type) headers.set("content-type", type);
      const setLength = url.searchParams.get("set_length");
      if (setLength) headers.set("content-length", setLength);
      return new Response(request.body, { status: number("status", 200), headers });
    }

    if (url.pathname === "/echo-read-then-give-back") {
      const reader = request.body.getReader();
      const first = await reader.read();
      reader.releaseLock();
      const read = first.done ? 0 : first.value.length;
      headers.set("x-first-chunk", String(read));
      return new Response(request.body, { headers });
    }

    if (url.pathname === "/text") {
      return new Response(url.searchParams.get("body") ?? "", { headers });
    }

    if (url.pathname === "/made") {
      const size = number("size", 0);
      const kind = url.searchParams.get("kind") ?? "string";
      if (kind === "string") {
        return new Response("x".repeat(size), { headers });
      }
      if (kind === "bytes") {
        return new Response(new Uint8Array(size).fill(0x78), { headers });
      }
      if (kind === "stream") {
        const chunk = number("chunk", 65536);
        let sent = 0;
        const stream = new ReadableStream({
          pull(controller) {
            if (sent >= size) {
              controller.close();
              return;
            }
            const take = Math.min(chunk, size - sent);
            controller.enqueue(new Uint8Array(take).fill(0x78));
            sent += take;
          },
        });
        return new Response(stream, { headers });
      }
      if (kind === "transform") {
        const { readable, writable } = new TransformStream();
        // The platform reads the response's body to its end within the
        // request, and that reading is what drives this pump.
        (async () => {
          const writer = writable.getWriter();
          for (let sent = 0; sent < size; sent += 65536) {
            const take = Math.min(65536, size - sent);
            await writer.write(new Uint8Array(take).fill(0x78));
          }
          await writer.close();
        })();
        return new Response(readable, { headers });
      }
    }

    return new Response("no such route", { status: 404 });
  },
};
