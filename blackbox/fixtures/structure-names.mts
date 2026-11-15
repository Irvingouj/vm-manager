// Lists what a Drizzle definition holds, as data (blackbox/tests/pull_hostile.rs):
// for each exported table its export name, real name and columns' real
// names, and for each exported enum its name and labels. Run with Node's
// type stripping: node structure-names.mts <absolute path of schema.ts>
import { getTableColumns, getTableName, is } from "drizzle-orm";
import { PgTable } from "drizzle-orm/pg-core";

const schema: Record<string, unknown> = await import(process.argv[2] ?? "");
const tables: { export: string; name: string; columns: string[] }[] = [];
const enums: { export: string; name: string; labels: string[] }[] = [];
for (const [name, value] of Object.entries(schema)) {
  if (is(value, PgTable)) {
    tables.push({
      export: name,
      name: getTableName(value),
      columns: Object.values(getTableColumns(value)).map((c) => c.name),
    });
  } else if (typeof value === "function" && "enumValues" in value && "enumName" in value) {
    const e = value as unknown as { enumName: string; enumValues: string[] };
    enums.push({ export: name, name: e.enumName, labels: [...e.enumValues] });
  }
}
console.log(JSON.stringify({ tables, enums }));
