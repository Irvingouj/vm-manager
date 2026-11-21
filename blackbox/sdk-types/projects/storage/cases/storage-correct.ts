// A correct use of Storage compiles with no diagnostics (CONTRACT.md,
// "Storage: Through the SDK"): the handle of a Resource declared at write
// is a `Storage<"<name>">`, at read a `ReadOnlyStorage<"<name>">`; every call has the types
// the contract gives; `Refused` carries the kinds of a mismatch.
import {
  defineApp,
  OperationFailed,
  Refused,
  type FileBody,
  type FileInfo,
  type FileList,
  type ListOptions,
  type PutOptions,
  type ReadOnlyStorage,
  type Storage,
  type StorageDeclaration,
  type StoredFile,
} from "@vm/sdk";
import manifest from "../manifest.ts";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
function same<A, B>(_proof: Equal<A, B>): void {}

const declared: StorageDeclaration = { kind: "storage", access: "write" };
const options: ListOptions = { prefix: "reports/", delimiter: "/", limit: 100, cursor: "c" };
const putOptions: PutOptions = { contentType: "application/pdf" };
const bodies: FileBody[] = ["text", new Uint8Array([1, 2]), new ArrayBuffer(2)];

export default defineApp(manifest, {
  async fetch(request, ctx) {
    const files = ctx.user().storage("marketing");
    const archive = ctx.user().storage("archive");
    // The handles carry the Resource's name, and each is assignable to the
    // interface written without one.
    same<typeof files, Storage<"marketing">>(true);
    same<typeof archive, ReadOnlyStorage<"archive">>(true);
    const anyFiles: Storage = files;
    const anyArchive: ReadOnlyStorage = archive;
    const readable: ReadOnlyStorage = files;
    void anyFiles;
    void anyArchive;
    void readable;

    const page = await archive.list({ prefix: "reports/", delimiter: "/" });
    same<typeof page, FileList>(true);
    same<typeof page.files, readonly FileInfo[]>(true);
    same<typeof page.folders, readonly string[]>(true);
    same<typeof page.cursor, string | null>(true);
    const all = await files.list();
    same<typeof all, FileList>(true);
    const next = await files.list(options);
    same<typeof next, FileList>(true);

    const info = await archive.head("reports/q3.pdf");
    same<typeof info, FileInfo | null>(true);
    if (info) {
      same<typeof info.key, string>(true);
      same<typeof info.size, number>(true);
      same<typeof info.contentType, string>(true);
      same<typeof info.lastModified, Date>(true);
    }
    const file = await archive.get("reports/q3.pdf");
    same<typeof file, StoredFile | null>(true);
    if (file) {
      same<typeof file.body, Uint8Array<ArrayBuffer>>(true);
      same<ReturnType<typeof file.text>, string>(true);
    }

    const written = await files.put("reports/q4.pdf", await request.arrayBuffer(), putOptions);
    same<typeof written, FileInfo>(true);
    for (const body of bodies) await files.put("reports/q5.pdf", body);
    await files.put("notes.txt", "a string is written as UTF-8");
    const removed = await files.delete("reports/old.pdf");
    same<typeof removed, void>(true);

    try {
      await files.get("reports/q3.pdf");
    } catch (e) {
      if (e instanceof Refused) {
        const refusal = e.refusal;
        if (refusal.reason === "not-declared") {
          const kind: string | undefined = refusal.kind;
          const declaredKind: string | undefined = refusal.declared_kind;
          return Response.json({ kind, declaredKind });
        }
        if (refusal.reason === "not-registered") {
          const registeredKind: string | undefined = refusal.registered_kind;
          return Response.json({ registeredKind });
        }
      }
      if (e instanceof OperationFailed) return Response.json({ kind: e.kind });
      throw e;
    }
    return file
      ? new Response(file.body, { headers: { "content-type": file.contentType } })
      : Response.json({ declared, info, written });
  },
});
