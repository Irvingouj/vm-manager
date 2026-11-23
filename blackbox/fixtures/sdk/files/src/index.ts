// Reads and writes files of Storage Resources through the SDK
// (blackbox/tests/storage_sdk.rs, storage_stream_get.rs,
// storage_stream_put.rs; CONTRACT.md "Storage: Through the SDK"). Every
// route answers JSON unless it says otherwise. `resource` is `marketing`
// (declared at write) unless `archive` is asked for. A file's body is the
// ReadableStream the host feeds: `bytes()`, `text()`, `json()` and
// `arrayBuffer()` each read it to its end, once.
//
//   POST /put?key=&type=&as=text|bytes|buffer|blob|dataview
//                                    the request's body put as that kind:
//                                    { info }
//   POST /upload?key=                the request's body put as it came:
//                                    put(key, request.body): { info }
//                                    (a request with no body: an empty file)
//   GET  /put-null?key=              put(key, null): { info }
//   POST /own-stream?key=&chunks=&chunkSize=&failAt=
//                                    a put of a stream the code makes:
//                                    { info } or { error }
//   GET  /own-stream-cancel?key=     a put of a stream the code then tries to
//                                    cancel: { locked, cancel, info }
//   GET  /get?key=                   { file: null | { info, isStream,
//                                    bodyUsedAfter, bytesB64 } } (the body
//                                    read with bytes())
//   GET  /get-text?key=              { text } (the body read with text())
//   GET  /readers?case=&key=         one reader once: { value }
//   GET  /read-twice?case=&key=      { first, second: { error } }
//   GET  /read-after-body?key=       the body read, then text(): { error }
//   GET  /body-used?key=             { before, after } around one read
//   GET  /copy?from=&to=             put(to, (await get(from)).body):
//                                    { info }
//   GET  /head?key=                  { info: null | <info> }
//   GET  /list?prefix=&delimiter=&limit=&cursor=
//                                    { files: [info], folders, cursor }
//   GET  /delete?key=                { result: typeof what it resolved to }
//   GET  /serve?key=                 the file's bytes with its content type,
//                                    or 404
//   GET  /caught?case=               what one call forced past the types
//                                    rejected with: { error }
//   GET  /escape?case=refused|failed the rejection not caught
//   GET  /type-error?case=           a call with arguments of the wrong
//                                    type: { error }
//   GET  /big?key=&size=             a put of `size` bytes of 7, then a get
//                                    of it read back: { put, got, same }
//   GET  /hostile                    every hostile key through the five
//                                    calls, forced: { answers }
import {
  defineApp,
  OperationFailed,
  Refused,
  type FileInfo,
  type FileList,
  type FileBody,
  type ReadOnlyStorage,
  type Storage,
  type StoredFile,
} from "@vm/sdk";
import { integer, pgTable } from "drizzle-orm/pg-core";
import manifest from "../manifest.ts";

const anyTable = pgTable("anything", { id: integer("id") });

function describeInfo(info: FileInfo | null): unknown {
  if (info === null) return null;
  return {
    key: info.key,
    size: info.size,
    contentType: info.contentType,
    lastModified: info.lastModified instanceof Date ? info.lastModified.toISOString() : String(info.lastModified),
    isDate: info.lastModified instanceof Date,
  };
}

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// Standard base64 with padding, written out so as not to depend on `btoa`.
function base64(bytes: Uint8Array): string {
  let out = "";
  let i = 0;
  const at = (n: number): string => ALPHABET.charAt(n & 63);
  for (; i + 2 < bytes.length; i += 3) {
    const n = ((bytes[i] ?? 0) << 16) | ((bytes[i + 1] ?? 0) << 8) | (bytes[i + 2] ?? 0);
    out += at(n >> 18) + at(n >> 12) + at(n >> 6) + at(n);
  }
  const rest = bytes.length - i;
  if (rest === 1) {
    const n = (bytes[i] ?? 0) << 16;
    out += at(n >> 18) + at(n >> 12) + "==";
  } else if (rest === 2) {
    const n = ((bytes[i] ?? 0) << 16) | ((bytes[i + 1] ?? 0) << 8);
    out += at(n >> 18) + at(n >> 12) + at(n >> 6) + "=";
  }
  return out;
}

async function describeFile(file: StoredFile | null): Promise<unknown> {
  if (file === null) return null;
  const theBody = file.body;
  const isStream = typeof ReadableStream !== "undefined" && theBody instanceof ReadableStream;
  const bytes = await file.bytes();
  return {
    info: describeInfo(file),
    isStream,
    bodyUsedAfter: file.bodyUsed,
    bytesB64: base64(bytes),
  };
}

function describeList(list: FileList): unknown {
  return { files: list.files.map(describeInfo), folders: list.folders, cursor: list.cursor };
}

function describeError(e: unknown): unknown {
  if (e instanceof Refused) {
    return {
      name: e.name,
      isRefused: true,
      isError: e instanceof Error,
      message: e.message,
      refusal: e.refusal,
      fix: e.fix,
    };
  }
  if (e instanceof OperationFailed) {
    return {
      name: e.name,
      isOperationFailed: true,
      isError: e instanceof Error,
      kind: e.kind,
      detail: e.detail,
    };
  }
  if (e instanceof Error) {
    return { name: e.name, isTypeError: e instanceof TypeError, message: e.message };
  }
  return { thrown: String(e) };
}

async function caught(run: () => Promise<unknown>): Promise<unknown> {
  try {
    const value = await run();
    return { resolved: value === undefined ? "undefined" : value };
  } catch (e) {
    return { error: describeError(e) };
  }
}

// The value a reader answered, as JSON, for a body of known content.
async function readerValue(file: StoredFile, which: string): Promise<unknown> {
  switch (which) {
    case "arrayBuffer": {
      const buffer = await file.arrayBuffer();
      return { byteLength: buffer.byteLength, b64: base64(new Uint8Array(buffer)) };
    }
    case "bytes": {
      const bytes = await file.bytes();
      return { length: bytes.length, b64: base64(bytes) };
    }
    case "text":
      return { text: await file.text() };
    case "json":
      return { value: await file.json() };
  }
  return { unknown: which };
}

// A stream the code makes, of `chunks` chunks of `chunkSize` bytes, that
// errors at `failAt` when given.
function madeStream(chunks: number, chunkSize: number, failAt: number): ReadableStream<Uint8Array> {
  let at = 0;
  return new ReadableStream({
    pull(controller) {
      if (at === failAt) {
        controller.error(new Error("the stream failed on purpose"));
        return;
      }
      if (at >= chunks) {
        controller.close();
        return;
      }
      controller.enqueue(new Uint8Array(chunkSize).fill(at & 0xff));
      at += 1;
    },
  });
}

const HOSTILE: Record<string, string> = {
  "dotdot-secret": "../secret",
  "absolute": "/etc/passwd",
  "dotdot": "..",
  "nul": "a\u0000b",
  "backslash": "..\\secret",
  "marker": ".vibe-resource",
  "not-nfc": "cafe\u0301",
};

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const url = new URL(request.url);
    const q = (name: string): string | null => url.searchParams.get(name);
    const key = q("key") ?? "";
    const files: Storage = ctx.user().storage("marketing");
    const archive: ReadOnlyStorage = ctx.user().storage("archive");
    const reading: ReadOnlyStorage = q("resource") === "archive" ? archive : files;
    switch (url.pathname) {
      case "/put": {
        const bytes = new Uint8Array(await request.arrayBuffer());
        const as = q("as") ?? "bytes";
        let body: FileBody;
        if (as === "text") body = new TextDecoder().decode(bytes);
        else if (as === "buffer") body = bytes.slice().buffer;
        else if (as === "blob") body = new Blob([bytes]);
        else if (as === "dataview") body = new DataView(bytes.slice().buffer);
        else body = bytes;
        const type = q("type");
        const info = type === null ? await files.put(key, body) : await files.put(key, body, { contentType: type });
        return Response.json({ info: describeInfo(info) });
      }
      case "/upload": {
        // The request's body itself, unread: the host writes it to the file
        // as it arrives from the socket. A request with no body has a null
        // `body`, which is an empty file.
        const info = await files.put(key, request.body);
        return Response.json({ info: describeInfo(info) });
      }
      case "/put-null": {
        const info = await files.put(key, null);
        return Response.json({ info: describeInfo(info) });
      }
      case "/own-stream": {
        const chunks = Number(q("chunks") ?? 4);
        const chunkSize = Number(q("chunkSize") ?? 65536);
        const failAt = Number(q("failAt") ?? -1);
        return Response.json(await caught(async () => ({ info: describeInfo(await files.put(key, madeStream(chunks, chunkSize, failAt))) })));
      }
      case "/own-stream-cancel": {
        // The stream is the put's from the call on: locked, so the code
        // cannot cancel it, and the put goes on.
        const stream = madeStream(3, 1024, -1);
        const pending = files.put(key, stream);
        const locked = stream.locked;
        const cancel = await stream.cancel().then(
          () => ({ cancelled: true }),
          (e: unknown) => ({ isTypeError: e instanceof TypeError }),
        );
        return Response.json({ locked, cancel, info: describeInfo(await pending) });
      }
      case "/get":
        return Response.json({ file: await describeFile(await reading.get(key)) });
      case "/get-text": {
        const file = await reading.get(key);
        if (file === null) return Response.json({ file: null });
        return Response.json({ text: await file.text() });
      }
      case "/readers": {
        const file = await reading.get(key);
        if (file === null) return Response.json({ file: null });
        return Response.json({ value: await readerValue(file, q("case") ?? "bytes") });
      }
      case "/read-twice": {
        const file = await reading.get(key);
        if (file === null) return Response.json({ file: null });
        const first = await readerValue(file, q("case") ?? "bytes");
        const second = await caught(() => readerValue(file, q("case") ?? "bytes"));
        return Response.json({ first, second });
      }
      case "/read-after-body": {
        const file = await reading.get(key);
        if (file === null) return Response.json({ file: null });
        const reader = file.body.getReader();
        for (;;) {
          const { done } = await reader.read();
          if (done) break;
        }
        return Response.json(await caught(() => file.text()));
      }
      case "/body-used": {
        const file = await reading.get(key);
        if (file === null) return Response.json({ file: null });
        const before = file.bodyUsed;
        const reader = file.body.getReader();
        await reader.read();
        const after = file.bodyUsed;
        await reader.cancel();
        return Response.json({ before, after });
      }
      case "/copy": {
        const from = await reading.get(q("from") ?? "");
        if (from === null) return Response.json({ file: null });
        const info = await files.put(q("to") ?? "", from.body);
        return Response.json({ info: describeInfo(info) });
      }
      case "/head":
        return Response.json({ info: describeInfo(await reading.head(key)) });
      case "/list": {
        const limit = q("limit");
        const list = await reading.list({
          ...(q("prefix") !== null ? { prefix: q("prefix") ?? "" } : {}),
          ...(q("delimiter") === "/" ? { delimiter: "/" as const } : {}),
          ...(limit !== null ? { limit: Number(limit) } : {}),
          ...(q("cursor") !== null ? { cursor: q("cursor") ?? "" } : {}),
        });
        return Response.json(describeList(list));
      }
      case "/list-all":
        return Response.json(describeList(await reading.list()));
      case "/delete": {
        const result = await files.delete(key);
        return Response.json({ result: typeof result });
      }
      case "/serve": {
        const file = await reading.get(key);
        return file
          ? new Response(file.body, { headers: { "content-type": file.contentType } })
          : new Response("none", { status: 404 });
      }
      case "/caught": {
        const forced = ctx.user() as unknown as {
          storage(name: string): Storage;
          db(name: string): { select(): { from(t: typeof anyTable): Promise<unknown> } };
        };
        const app = ctx.app() as unknown as { storage(name: string): Storage };
        const cases: Record<string, () => Promise<unknown>> = {
          "put-through-read": () => (archive as unknown as Storage).put("x.txt", "never"),
          "delete-through-read": () => (archive as unknown as Storage).delete("x.txt"),
          "undeclared": () => forced.storage("payroll").head("x.txt"),
          "declared-postgres": () => forced.storage("devops").head("x.txt"),
          "db-on-storage": () => forced.db("marketing").select().from(anyTable),
          "app-storage": () => app.storage("marketing").head("x.txt"),
          "invalid-key": () => files.get("../x"),
          "get": () => files.get(key),
          "put": () => files.put(key, "from caught"),
        };
        const run = cases[q("case") ?? ""];
        if (!run) return Response.json({ error: "no such case" }, { status: 404 });
        return Response.json(await caught(run));
      }
      case "/escape": {
        if (q("case") === "refused") await (archive as unknown as Storage).put("x.txt", "never");
        else await files.get("../x");
        return Response.json({ escaped: false });
      }
      case "/type-error": {
        const loose = files as unknown as {
          put(key: unknown, body: unknown, options?: unknown): Promise<unknown>;
          get(key: unknown): Promise<unknown>;
          head(key: unknown): Promise<unknown>;
          delete(key: unknown): Promise<unknown>;
          list(options?: unknown): Promise<unknown>;
        };
        const locked = (): ReadableStream<Uint8Array> => {
          const stream = madeStream(1, 16, -1);
          stream.getReader();
          return stream;
        };
        const alreadyRead = async (): Promise<ReadableStream<Uint8Array>> => {
          const stream = madeStream(1, 16, -1);
          const reader = stream.getReader();
          for (;;) {
            const { done } = await reader.read();
            if (done) break;
          }
          reader.releaseLock();
          return stream;
        };
        const cases: Record<string, () => Promise<unknown>> = {
          "body-number": () => loose.put("x.txt", 5),
          "body-object": () => loose.put("x.txt", { bytes: [1, 2] }),
          "body-missing": () => loose.put("x.txt", undefined),
          "body-boolean": () => loose.put("x.txt", true),
          "body-locked-stream": () => loose.put("x.txt", locked()),
          "body-read-stream": async () => loose.put("x.txt", await alreadyRead()),
          "key-number": () => loose.get(5),
          "key-missing": () => loose.head(undefined),
          "delete-key-object": () => loose.delete({ key: "x.txt" }),
          "put-options-number": () => loose.put("x.txt", "body", 5),
          "content-type-number": () => loose.put("x.txt", "body", { contentType: 5 }),
          "list-options-string": () => loose.list("reports/"),
          "list-prefix-number": () => loose.list({ prefix: 5 }),
        };
        const run = cases[q("case") ?? ""];
        if (!run) return Response.json({ error: "no such case" }, { status: 404 });
        return Response.json(await caught(run));
      }
      case "/big": {
        const size = Number(q("size"));
        const body = new Uint8Array(size).fill(7);
        const put = await files.put(key, body);
        const got = await files.get(key);
        let same = false;
        if (got !== null) {
          const bytes = await got.bytes();
          same = bytes.length === size && bytes.every((b) => b === 7);
        }
        return Response.json({ put: describeInfo(put), got: got === null ? null : describeInfo(got), same });
      }
      case "/hostile": {
        const forced = files as unknown as Storage;
        const answers: Record<string, unknown> = {};
        for (const [label, hostile] of Object.entries(HOSTILE)) {
          answers[label] = {
            head: await caught(() => forced.head(hostile)),
            get: await caught(() => forced.get(hostile)),
            put: await caught(() => forced.put(hostile, "planted")),
            delete: await caught(() => forced.delete(hostile)),
            list: await caught(() => forced.list({ prefix: hostile })),
          };
        }
        return Response.json({ answers });
      }
    }
    return Response.json({ error: "no such route" }, { status: 404 });
  },
});
