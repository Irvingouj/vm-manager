// Reading the Change history of the own database (blackbox/CONTRACT.md,
// "Reading the Change history"): `history(table, key?, options?)` on the
// own database's handles. One call is one host `query`, as the handle's
// actor, of `vm.history`.
//
// Postgres reads each stored row (`old`, `new`: jsonb, as `to_jsonb` gave
// it) back into the table's present row type, `jsonb_populate_record(null::
// <table>, ...)`, in that same statement: a dropped column is not there, an
// added or renamed one is null, and a value that no longer casts fails the
// statement. The columns come back as a `select()` of the table brings them
// (text by type, values.js), and each is read by its Drizzle column's
// `mapFromDriverValue`, as Drizzle's own `mapResultRow` does; so a row of the
// history is read by the same code as a row of the table.
//
// Drizzle API used: `is`, `getTableColumns`, `getTableConfig` (its
// `columns`, `primaryKeys`, `name`, `schema`), a column's `name`, `primary`,
// `mapToDriverValue` and `mapFromDriverValue`.

import { getTableColumns, is } from "drizzle-orm";
import { getTableConfig, PgTable } from "drizzle-orm/pg-core";
import { decodeRows, encodeParam } from "./values.js";

/** What `history` forced onto a shared database's handle throws. */
export const NOT_SHARED =
  "Change history is kept only in the own database: call history(...) on ctx.user().ownDb() or ctx.app().ownDb()";

const DEFAULT_LIMIT = 100;
const MAX_LIMIT = 1000;
/** The greatest `bigint`: `vm.history.id` is one. */
const MAX_ID = 9223372036854775807n;

/** A column's name as a string constant: a name, not a value. */
function literal(name) {
  return `'${String(name).replace(/'/g, "''")}'`;
}

/** A name as a quoted SQL identifier. */
function ident(name) {
  return `"${String(name).replace(/"/g, '""')}"`;
}

/**
 * What is known of `table` for reading its history: its schema and name in
 * the database, its columns in declaration order with their property names,
 * and its primary key's columns (by property name), empty when it has none.
 */
function describeTable(table) {
  if (!is(table, PgTable)) {
    throw new TypeError(
      "history(table, ...): `table` must be a table of the own database's structure (src/db/schema.ts)",
    );
  }
  const config = getTableConfig(table);
  const columns = Object.entries(getTableColumns(table)).map(([property, column]) => ({ property, column }));
  let key = columns.filter(({ column }) => column.primary);
  const composite = config.primaryKeys[0];
  if (key.length === 0 && composite !== undefined) {
    // primaryKey({ columns }) holds the columns of the table's extra
    // config, not the table's own objects: matched by database name.
    key = composite.columns
      .map((pk) => columns.find(({ column }) => column.name === pk.name))
      .filter((found) => found !== undefined);
  }
  // A table declared without a schema is in `public`, where the
  // migrations Drizzle writes for it make it.
  const schema = config.schema ?? "public";
  return { schema, name: config.name, columns, key, relation: `${ident(schema)}.${ident(config.name)}` };
}

/** The key's values in the order of `table.key`, checked: `undefined` for no key. */
function keyValues(table, key) {
  if (key === undefined) return undefined;
  const names = table.key.map(({ property }) => property);
  const label = `history(${table.name}, key)`;
  if (names.length === 0) {
    throw new TypeError(
      `${label}: ${table.name} has no primary key, so a row of it cannot be named; ` +
        `call history(${table.name}) without a key to read the changes of every row`,
    );
  }
  const wanted = `an object with exactly the primary key's properties: { ${names.join(", ")} }`;
  if (key === null || typeof key !== "object" || Array.isArray(key)) {
    throw new TypeError(
      `${label}: the key must be ${wanted}, got ${key === null ? "null" : typeof key}; ` +
        "leave it out (or pass undefined) to read every row's changes",
    );
  }
  const given = Object.keys(key);
  const extra = given.filter((name) => !names.includes(name));
  const missing = names.filter((name) => !Object.hasOwn(key, name) || key[name] === undefined || key[name] === null);
  if (extra.length > 0 || missing.length > 0) {
    const what = [
      missing.length > 0 ? `missing ${missing.join(", ")}` : "",
      extra.length > 0 ? `not of the primary key: ${extra.join(", ")}` : "",
    ]
      .filter((s) => s !== "")
      .join("; ");
    throw new TypeError(`${label}: the key must be ${wanted} (${what})`);
  }
  // Each value as a `where` on its column would send it.
  return table.key.map(({ property, column }) => encodeParam(column.mapToDriverValue(key[property])));
}

/** `options`, checked: the limit, and `before` as a bigint or undefined. */
function optionsOf(options) {
  if (options === undefined) return { limit: DEFAULT_LIMIT, before: undefined };
  if (options === null || typeof options !== "object") {
    throw new TypeError("history(table, key, options): `options` must be an object: { limit?, before? }");
  }
  const { limit = DEFAULT_LIMIT, before } = options;
  if (!Number.isInteger(limit) || limit < 1 || limit > MAX_LIMIT) {
    throw new TypeError(
      `history(table, key, options): \`limit\` must be an integer from 1 to ${MAX_LIMIT} ` +
        `(${DEFAULT_LIMIT} when left out)`,
    );
  }
  if (before !== undefined && (typeof before !== "string" || !/^[0-9]+$/.test(before))) {
    throw new TypeError(
      "history(table, key, options): `before` must be a change's `id`: a string of decimal digits",
    );
  }
  return { limit, before: before === undefined ? undefined : BigInt(before) };
}

/**
 * The one statement, and its parameters: the changes of `table` (of the row
 * whose stored key equals `key`, when given), newest first, at most `limit`,
 * older than `before`; each with its `old` and `new` read into the table's
 * present row type. No value is written into the statement.
 *
 * The key is compared as stored: the given values are read into the key
 * columns' present types by the same `jsonb_populate_record`, and turned
 * back into jsonb as the capture did (`to_jsonb` of each column), so the
 * comparison is exact whatever the columns' types. The hash of the key
 * finds the rows through the index `vm.history_row` (own_db_schema.sql).
 */
function statementOf(table, values, options) {
  const params = [table.schema, table.name];
  const param = (value) => {
    params.push(value);
    return `$${params.length}`;
  };
  let keyed = "";
  let filter = "";
  if (values !== undefined) {
    const given = table.key.map(({ column }, i) => `${literal(column.name)}, ${param(values[i])}::text`).join(", ");
    const built = table.key.map(({ column }) => `${literal(column.name)}, k.${ident(column.name)}`).join(", ");
    keyed =
      `with wanted as (select pg_catalog.jsonb_build_object(${built}) as key ` +
      `from pg_catalog.jsonb_populate_record(null::${table.relation}, pg_catalog.jsonb_build_object(${given})) k) `;
    filter =
      " and pg_catalog.jsonb_hash_extended(h.key, 0)" +
      " = (select pg_catalog.jsonb_hash_extended(w.key, 0) from wanted w)" +
      " and h.key = (select w.key from wanted w)";
  }
  // Older than every change there is: nothing to leave out.
  if (options.before !== undefined && options.before <= MAX_ID) {
    filter += ` and h.id < ${param(options.before.toString())}::bigint`;
  }
  const limit = param(String(options.limit));
  const row = (alias) => table.columns.map(({ column }) => `${alias}.${ident(column.name)}`).join(", ");
  const sql =
    `${keyed}select h.id::text, h.at, h.kind, h.changed, h.user_id, h.application, h.actor, h.request, h.migration, ` +
    `h.old is not null, ${row("o")}, h.new is not null, ${row("n")} ` +
    "from (select h.* from vm.history h " +
    `where h.schema_name = $1 and h.table_name = $2${filter} ` +
    `order by h.id desc limit ${limit}::integer) h ` +
    `cross join lateral pg_catalog.jsonb_populate_record(null::${table.relation}, h.old) o ` +
    `cross join lateral pg_catalog.jsonb_populate_record(null::${table.relation}, h.new) n ` +
    "order by h.id desc";
  return { sql, params };
}

/** The leading columns of a result row, before `old`. */
const LEADING = 9;

/**
 * An update's `changed` (database names, in the database's column order) as
 * properties in the order the Drizzle table declares them; then the names
 * of columns it does not declare (dropped since, or not in schema.ts), by
 * their database names, in the order they were recorded.
 */
function changedOf(table, properties, names) {
  const recorded = new Set(names.filter((name) => typeof name === "string"));
  const declared = table.columns.filter(({ column }) => recorded.has(column.name)).map(({ property }) => property);
  const others = [...recorded].filter((name) => !properties.has(name));
  return [...declared, ...others];
}

/** One result row as a `Change`. */
function changeOf(table, properties, row) {
  const width = table.columns.length;
  const rowAt = (start) => {
    if (row[start] !== true) return null;
    const out = {};
    table.columns.forEach(({ property, column }, i) => {
      const value = row[start + 1 + i];
      // As Drizzle's mapResultRow reads a column.
      out[property] = value === null || value === undefined ? null : column.mapFromDriverValue(value);
    });
    return out;
  };
  const changed = row[3] === null ? null : JSON.parse(row[3]);
  return {
    id: row[0],
    // As Drizzle's timestamp({ withTimezone: true }) reads its text.
    at: new Date(row[1]),
    kind: row[2],
    old: rowAt(LEADING),
    new: rowAt(LEADING + 1 + width),
    changed: Array.isArray(changed) ? changedOf(table, properties, changed) : null,
    userId: row[4],
    application: row[5],
    actor: row[6],
    request: row[7],
    migration: row[8],
  };
}

/**
 * `history(table, key?, options?)` through `run(operation, args)`, which
 * makes one host operation and gives what it answered (`settle`d), on
 * `target` (`{ own: true, as }`). Arguments are checked before anything is
 * sent.
 */
export async function readHistory(run, target, table, key, options) {
  const described = describeTable(table);
  const values = keyValues(described, key);
  const checked = optionsOf(options);
  const { sql, params } = statementOf(described, values, checked);
  const result = await run("query", { ...target, sql, params, values: "text" });
  const rows = decodeRows(result.types, Array.isArray(result.rows) ? result.rows : []);
  const properties = new Map(described.columns.map(({ property, column }) => [column.name, property]));
  return rows.map((row) => changeOf(described, properties, row));
}
