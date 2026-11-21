// Reads and writes files of Storage Resources through the SDK
// (blackbox/tests/storage_sdk.rs, CONTRACT.md "Storage: Through the SDK").
// Every route answers JSON unless it says otherwise. `resource` is
// `marketing` (declared at write) unless `archive` (declared at read) is
// asked for.
//
//   POST /put?key=&type=&as=text|bytes|buffer   the request's body put:
//                                    { info }
//   GET  /get?key=                   { file: null | { info, bodyB64, text,
//                                    isUint8Array } }
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
//                                    of it: { put, got, same }
//   POST /upload?key=                the request's body put as it came:
//                                    { info }
//   GET  /hostile                    every hostile key through the five
//                                    calls, forced: { answers }
import {
  defineApp,
  OperationFailed,
  Refused,
  type FileInfo,
  type FileList,
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

function describeFile(file: StoredFile | null): unknown {
  if (file === null) return null;
  return {
    info: describeInfo(file),
    isUint8Array: file.body instanceof Uint8Array,
    bodyB64: base64(file.body),
    text: file.text(),
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

const HOSTILE: Record<string, string> = {
  "dotdot-secret": "../secret",
  "absolute": "/etc/passwd",
  "dotdot": "..",
  "nul": "a\u0000b",
  "backslash": "..\\secret",
  "marker": ".vibe-resource",
  "not-nfc": "café",
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
        const body = as === "text" ? new TextDecoder().decode(bytes) : as === "buffer" ? bytes.slice().buffer : bytes;
        const type = q("type");
        const info = type === null ? await files.put(key, body) : await files.put(key, body, { contentType: type });
        return Response.json({ info: describeInfo(info) });
      }
      case "/get":
        return Response.json({ file: describeFile(await reading.get(key)) });
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
        const cases: Record<string, () => Promise<unknown>> = {
          "body-number": () => loose.put("x.txt", 5),
          "body-object": () => loose.put("x.txt", { bytes: [1, 2] }),
          "body-missing": () => loose.put("x.txt", undefined),
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
        const same = got !== null && got.body.length === size && got.body.every((b) => b === 7);
        return Response.json({ put: describeInfo(put), got: got === null ? null : describeInfo(got), same });
      }
      case "/upload": {
        const info = await files.put(key, await request.arrayBuffer());
        return Response.json({ info: describeInfo(info) });
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
