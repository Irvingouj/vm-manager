// A Storage Resource's handle (blackbox/CONTRACT.md, "Storage: Through the
// SDK"): each call is exactly one host operation (`storage.head`, `.get`,
// `.put`, `.delete`, `.list`) on `{ resource, as }`. Arguments of the wrong
// type are a TypeError and nothing is sent; everything else (the key's
// rules, the bounds) is the host's to decide, so that it is logged.
//
// Bodies cross the raw interface as standard base64. An 8 MiB body is held
// in the heap as its bytes and its text, once each.

const freeze = Object.freeze;
const hasOwn = (object, key) => object !== null && typeof object === "object" && Object.hasOwn(object, key);

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const CODES = new Uint8Array(64);
for (let i = 0; i < 64; i += 1) CODES[i] = ALPHABET.charCodeAt(i);
const VALUES = new Uint8Array(128).fill(255);
for (let i = 0; i < 64; i += 1) VALUES[ALPHABET.charCodeAt(i)] = i;
const PAD = 61; // "="

/** The ASCII text of `codes`, in pieces small enough for one call. */
function ascii(codes) {
  // ASCII is UTF-8 as it is.
  if (typeof TextDecoder === "function") return new TextDecoder().decode(codes);
  let text = "";
  for (let i = 0; i < codes.length; i += 8192) {
    text += String.fromCharCode.apply(null, codes.subarray(i, i + 8192));
  }
  return text;
}

/** Standard base64 with padding. */
function encode(bytes) {
  const out = new Uint8Array(Math.ceil(bytes.length / 3) * 4);
  let o = 0;
  let i = 0;
  for (; i + 2 < bytes.length; i += 3) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
    out[o++] = CODES[n >>> 18];
    out[o++] = CODES[(n >>> 12) & 63];
    out[o++] = CODES[(n >>> 6) & 63];
    out[o++] = CODES[n & 63];
  }
  const rest = bytes.length - i;
  if (rest === 1) {
    const n = bytes[i] << 16;
    out[o++] = CODES[n >>> 18];
    out[o++] = CODES[(n >>> 12) & 63];
    out[o++] = PAD;
    out[o++] = PAD;
  } else if (rest === 2) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8);
    out[o++] = CODES[n >>> 18];
    out[o++] = CODES[(n >>> 12) & 63];
    out[o++] = CODES[(n >>> 6) & 63];
    out[o++] = PAD;
  }
  return ascii(out);
}

/** The bytes of the host's base64 text, over an ArrayBuffer of their own. */
function decode(text) {
  if (typeof text !== "string") return new Uint8Array(0);
  let end = text.length;
  while (end > 0 && text.charCodeAt(end - 1) === PAD) end -= 1;
  const out = new Uint8Array(Math.floor((end * 3) / 4));
  let o = 0;
  let n = 0;
  let bits = 0;
  for (let i = 0; i < end; i += 1) {
    const c = text.charCodeAt(i);
    const v = c < 128 ? VALUES[c] : 255;
    if (v === 255) continue;
    n = (n << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[o++] = (n >>> bits) & 255;
    }
  }
  return o === out.length ? out : out.slice(0, o);
}

/** A file's metadata as the SDK gives it. */
function infoOf(answered) {
  return freeze({
    key: answered.key,
    size: answered.size,
    contentType: answered.content_type,
    lastModified: new Date(answered.last_modified),
  });
}

/** A file read whole. */
function fileOf(answered) {
  const body = decode(answered.body);
  return freeze({
    key: answered.key,
    size: answered.size,
    contentType: answered.content_type,
    lastModified: new Date(answered.last_modified),
    body,
    text: () => new TextDecoder("utf-8").decode(body),
  });
}

function checkKey(key) {
  if (typeof key !== "string") throw new TypeError("a Storage key must be a string");
}

/** The bytes of a put's body: a string as UTF-8, a Uint8Array or an ArrayBuffer. */
function bytesOf(body) {
  if (typeof body === "string") return new TextEncoder().encode(body);
  if (body instanceof Uint8Array) return body;
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  throw new TypeError("a put's body must be a string, a Uint8Array or an ArrayBuffer");
}

const isPlainObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/**
 * The handle of the Storage Resource `name`, as `as`. `run(operation,
 * args)` makes one host operation and answers its `ok`, or rejects with
 * `Refused` or `OperationFailed`.
 */
export function storageHandle(name, as, run) {
  const target = (more) => ({ resource: name, as, ...more });
  return freeze({
    async head(key) {
      checkKey(key);
      const answered = await run("storage.head", target({ key }));
      return answered === null || answered === undefined ? null : infoOf(answered);
    },
    async get(key) {
      checkKey(key);
      const answered = await run("storage.get", target({ key }));
      return answered === null || answered === undefined ? null : fileOf(answered);
    },
    async put(key, body, options) {
      checkKey(key);
      const bytes = bytesOf(body);
      if (options !== undefined && !isPlainObject(options)) {
        throw new TypeError("a put's options must be an object: { contentType }");
      }
      const contentType = options?.contentType;
      if (contentType !== undefined && typeof contentType !== "string") {
        throw new TypeError("a put's contentType must be a string");
      }
      const args = target({ key, body: encode(bytes) });
      if (contentType !== undefined) args.content_type = contentType;
      return infoOf(await run("storage.put", args));
    },
    async delete(key) {
      checkKey(key);
      await run("storage.delete", target({ key }));
      return undefined;
    },
    async list(options) {
      if (options !== undefined && !isPlainObject(options)) {
        throw new TypeError("a list's options must be an object: { prefix, delimiter, limit, cursor }");
      }
      const args = target({});
      for (const [field, type] of [
        ["prefix", "string"],
        ["delimiter", "string"],
        ["limit", "number"],
        ["cursor", "string"],
      ]) {
        if (!hasOwn(options, field) || options[field] === undefined) continue;
        if (typeof options[field] !== type) throw new TypeError(`a list's ${field} must be a ${type}`);
        args[field] = options[field];
      }
      const answered = await run("storage.list", args);
      return freeze({
        files: freeze((Array.isArray(answered?.files) ? answered.files : []).map(infoOf)),
        folders: freeze(Array.isArray(answered?.folders) ? [...answered.folders] : []),
        cursor: typeof answered?.cursor === "string" ? answered.cursor : null,
      });
    },
  });
}
