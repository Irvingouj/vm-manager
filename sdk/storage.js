// A Storage Resource's handle (blackbox/CONTRACT.md, "Storage: Through the
// SDK"): each call is exactly one host operation (`storage.head`, `.get`,
// `.put`, `.delete`, `.list`) on `{ resource, as }`. Arguments of the wrong
// type are a TypeError and nothing is sent; everything else (the key's
// rules, the bounds) is the host's to decide, so that it is logged.
//
// A file's body never enters the Application's memory whole: `get` resolves
// to a handle whose `body` is the ReadableStream the host feeds as it is
// read (`arrayBuffer()`, `bytes()`, `text()` and `json()` each read it to
// its end, once), and a put's body is a stream, a string, bytes, a Blob or
// null (an empty file), handed to the host as it is.

const freeze = Object.freeze;
const hasOwn = (object, key) => object !== null && typeof object === "object" && Object.hasOwn(object, key);

/** The state of a stream the code may still read. */
function streamState(stream) {
  if (stream.locked === true) return "locked";
  const state = stream._state;
  return state === "closed" || state === "errored" ? "ended" : "readable";
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

/**
 * A file the host opened, its body the ReadableStream the host feeds as
 * the code reads it. Each of `arrayBuffer()`, `bytes()`, `text()` and
 * `json()` reads the stream to its end, so each can be used once, and
 * none after `body` itself was read: a second use rejects with a
 * TypeError, as a Response's does.
 */
function fileOf(answered) {
  const body = answered.body;
  const once = () => {
    if (used) {
      throw new TypeError("the file's body was already read: each of body, arrayBuffer(), bytes(), text() and json() can be used once");
    }
    const state = streamState(body);
    if (state === "locked") {
      throw new TypeError("the file's body is held by a reader of this code's own: release it before reading the file");
    }
    if (state === "ended") {
      throw new TypeError("the file's body was already read: each of body, arrayBuffer(), bytes(), text() and json() can be used once");
    }
    used = true;
  };
  let used = false;
  const drain = async () => {
    once();
    const reader = body.getReader();
    const parts = [];
    for (;;) {
      const read = await reader.read();
      if (read.done) break;
      parts.push(read.value instanceof Uint8Array ? read.value : new Uint8Array(0));
    }
    const total = parts.reduce((n, part) => n + part.length, 0);
    const out = new Uint8Array(total);
    let at = 0;
    for (const part of parts) {
      out.set(part, at);
      at += part.length;
    }
    return out;
  };
  return freeze({
    key: answered.key,
    size: answered.size,
    contentType: answered.content_type,
    lastModified: new Date(answered.last_modified),
    get bodyUsed() {
      return used || streamState(body) !== "readable";
    },
    body,
    async arrayBuffer() {
      return (await drain()).buffer;
    },
    async bytes() {
      return drain();
    },
    async text() {
      return new TextDecoder("utf-8").decode(await drain());
    },
    async json() {
      return JSON.parse(new TextDecoder("utf-8").decode(await drain()));
    },
  });
}

function checkKey(key) {
  if (typeof key !== "string") throw new TypeError("a Storage key must be a string");
}

/**
 * Whether `body` is one of the kinds a put takes (CONTRACT.md, "Storage:
 * Through the SDK"): a ReadableStream that is neither locked nor already
 * read, a string, an ArrayBuffer, an ArrayBufferView, a Blob, or null (an
 * empty file). A Blob is read into bytes before the host is called.
 */
function bodyOf(body) {
  if (body === null) return null;
  if (typeof body === "string") return body;
  if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) return body;
  if (typeof ReadableStream === "function" && body instanceof ReadableStream) {
    const state = streamState(body);
    if (state === "locked") {
      throw new TypeError("a put's body must be a stream nobody is reading: this one is locked to a reader");
    }
    if (state === "ended") {
      throw new TypeError("a put's body must be a stream that can still be read: this one has already been read");
    }
    return body;
  }
  throw new TypeError("a put's body must be a ReadableStream, a string, an ArrayBuffer, an ArrayBufferView, a Blob, or null (an empty file)");
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
    put(key, body, options) {
      checkKey(key);
      if (options !== undefined && !isPlainObject(options)) {
        throw new TypeError("a put's options must be an object: { contentType }");
      }
      const contentType = options?.contentType;
      if (contentType !== undefined && typeof contentType !== "string") {
        throw new TypeError("a put's contentType must be a string");
      }
      if (body === undefined) {
        throw new TypeError("a put's body must be given: a ReadableStream, a string, an ArrayBuffer, an ArrayBufferView, a Blob, or null (an empty file)");
      }
      // The call itself, made with the body as it is: a stream is the
      // put's from the call on — the platform's glue locks it as the call
      // is made — so this is not an `async` method: `put` returns the
      // promise with the stream already locked.
      const write = (given) => {
        const args = target({ key, body: given });
        if (contentType !== undefined) args.content_type = contentType;
        return run("storage.put", args).then(infoOf);
      };
      if (typeof Blob === "function" && body instanceof Blob) {
        // A Blob is read into bytes first; there is nothing to lock.
        return body.arrayBuffer().then(write);
      }
      return write(bodyOf(body));
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
