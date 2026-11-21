// An honest Application that moves bodies of the bound's size through
// Storage's raw interface without sending them through the test's request
// (CONTRACT.md, "Storage: Bounds"; blackbox/tests/storage_bounds.rs). A
// body it makes is `size` bytes each of the value `byte`, so that its
// standard base64 is one group of four characters repeated, and a tail.
//
// Each reading is what invoke resolved to, or { threw: <message> }.
//
// Routes (POST):
//   /size                      { bytes: <the length of the request's body> }
//   /put-made { resource, key, size, byte, unpadded, spoil, content_type }
//                              a put of the body made; `unpadded` drops
//                              the `=` padding, `spoil` puts a character
//                              that is not base64 at its start:
//                              { answer: reading }
//   /get-made { resource, key, size, byte }
//                              a get; its body replaced by
//                              { body_length, same } (whether it is the
//                              body made with this size and byte):
//                              { answer: reading }

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

function encodeSmall(bytes) {
  let out = "";
  let i = 0;
  for (; i + 2 < bytes.length; i += 3) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
    out += ALPHABET[(n >> 18) & 63] + ALPHABET[(n >> 12) & 63] + ALPHABET[(n >> 6) & 63] + ALPHABET[n & 63];
  }
  const rest = bytes.length - i;
  if (rest === 1) {
    const n = bytes[i] << 16;
    out += ALPHABET[(n >> 18) & 63] + ALPHABET[(n >> 12) & 63] + "==";
  } else if (rest === 2) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8);
    out += ALPHABET[(n >> 18) & 63] + ALPHABET[(n >> 12) & 63] + ALPHABET[(n >> 6) & 63] + "=";
  }
  return out;
}

// The standard base64, with padding, of `size` bytes of the value `byte`.
function made(size, byte) {
  const groups = Math.floor(size / 3);
  const quad = encodeSmall([byte, byte, byte]);
  const tail = encodeSmall(Array.from({ length: size - groups * 3 }, () => byte));
  return quad.repeat(groups) + tail;
}

const message = (e) => String((e && e.message) || e);

async function call(operation, args) {
  try {
    return await env.VM.invoke(operation, args);
  } catch (e) {
    return { threw: message(e) };
  }
}

const json = (value, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });

globalThis.default = {
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/size") {
      const body = await request.arrayBuffer();
      return json({ bytes: body.byteLength });
    }
    const b = await request.json();
    switch (url.pathname) {
      case "/put-made": {
        let text = made(b.size, b.byte);
        if (b.unpadded) text = text.replace(/=+$/, "");
        if (b.spoil) text = "!" + text.slice(1);
        const args = { resource: b.resource, key: b.key, body: text };
        if (b.content_type !== undefined) args.content_type = b.content_type;
        return json({ answer: await call("storage.put", args) });
      }
      case "/get-made": {
        const answer = await call("storage.get", { resource: b.resource, key: b.key });
        if (answer && answer.ok && typeof answer.ok.body === "string") {
          const body = answer.ok.body;
          answer.ok.body = { body_length: body.length, same: body === made(b.size, b.byte) };
        }
        return json({ answer });
      }
    }
    return json({ error: "no such route" }, 404);
  },
};
