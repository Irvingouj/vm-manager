// Reading the Change history of the own database (blackbox/CONTRACT.md,
// "Reading the Change history" and "The Change history in the platform
// database: Through the SDK"): `history(table, key?, options?)` on the own
// database's handles. One call is one host operation `history`, as the
// handle's actor.
//
// The host reads each stored row (`old`, `new`) back into the table's
// present row type and answers it as `query` answers a row of the table
// (with `values: "text"`: text by type, values.js); each column is read by
// its Drizzle column's `mapFromDriverValue`, as Drizzle's own
// `mapResultRow` does, so a row of the history is read by the same code as
// a row of the table. The host compares the key as the key columns compare
// values.
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
  return { schema, name: config.name, columns, key };
}

/** The key as the host takes it, by database column, checked: `undefined` for no key. */
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
  const out = {};
  for (const { property, column } of table.key) {
    out[column.name] = encodeParam(column.mapToDriverValue(key[property]));
  }
  return out;
}

/** `options`, checked: the limit, and `before` or undefined. */
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
  return { limit, before };
}

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

/** One change as the host answered it, as a `Change`. */
function changeOf(table, properties, at, types, change) {
  const rowOf = (values) => {
    if (!Array.isArray(values)) return null;
    const [decoded] = decodeRows(types, [values]);
    const out = {};
    for (const { property, column } of table.columns) {
      const i = at.get(column.name);
      const value = i === undefined ? null : decoded[i];
      // As Drizzle's mapResultRow reads a column.
      out[property] = value === null || value === undefined ? null : column.mapFromDriverValue(value);
    }
    return out;
  };
  const structure = change.kind === "structure";
  return {
    id: change.id,
    at: new Date(change.at),
    kind: change.kind,
    old: structure ? null : rowOf(change.old),
    new: structure ? null : rowOf(change.new),
    changed: Array.isArray(change.changed) ? changedOf(table, properties, change.changed) : null,
    userId: change.user_id,
    application: change.application,
    actor: change.actor,
    request: change.request,
    migration: change.migration,
    structure: structure ? change.structure : null,
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
  const keyed = keyValues(described, key);
  const checked = optionsOf(options);
  const args = { ...target, schema: described.schema, table: described.name, limit: checked.limit, values: "text" };
  if (keyed !== undefined) args.key = keyed;
  if (checked.before !== undefined) args.before = checked.before;
  const result = await run("history", args);
  const columns = Array.isArray(result.columns) ? result.columns : [];
  const at = new Map(columns.map((name, i) => [name, i]));
  const properties = new Map(described.columns.map(({ property, column }) => [column.name, property]));
  const changes = Array.isArray(result.changes) ? result.changes : [];
  return changes.map((change) => changeOf(described, properties, at, result.types, change));
}
