// Lists what a Drizzle definition holds, as data (blackbox/tests/pull_hostile.rs):
// for each exported table its export name, real name and columns' real
// names, and for each exported enum its name and labels. Run with Node's
// type stripping: node structure-names.mts <absolute path of schema.ts>
import { getTableColumns, getTableName, is } from "drizzle-orm";
import { PgTable } from "drizzle-orm/pg-core";

const schema: Record<string, unknown> = await import(process.argv[2] ?? "");
const tables: { export: string; name: string; columns: string[]; defaults: Record<string, string | null> }[] = [];

/** The text of an `sql` template: its string chunks joined. */
function sqlText(value: unknown): string | null {
  if (typeof value !== "object" || value === null || !("queryChunks" in value)) return null;
  const chunks = (value as { queryChunks: unknown[] }).queryChunks;
  return chunks
    .map((c) => (typeof c === "object" && c !== null && "value" in c && Array.isArray(c.value) ? c.value.join("") : ""))
    .join("");
}
const enums: { export: string; name: string; labels: string[] }[] = [];
for (const [name, value] of Object.entries(schema)) {
  if (is(value, PgTable)) {
    tables.push({
      export: name,
      name: getTableName(value),
      columns: Object.values(getTableColumns(value)).map((c) => c.name),
      defaults: Object.fromEntries(Object.values(getTableColumns(value)).map((c) => [c.name, sqlText(c.default)])),
    });
  } else if (typeof value === "function" && "enumValues" in value && "enumName" in value) {
    const e = value as unknown as { enumName: string; enumValues: string[] };
    enums.push({ export: name, name: e.enumName, labels: [...e.enumValues] });
  }
}
console.log(JSON.stringify({ tables, enums }));
