// What crosses between Drizzle and the host's raw interface
// (blackbox/CONTRACT.md, "The SDK: Values" and "What Application code is
// given"): which operation a statement is, parameters as the raw interface
// takes them, and the values of a result as Drizzle's columns expect them.
//
// The host is asked for every value as the text Postgres printed, with each
// column's type (the raw interface's `values: "text"`). Values are then read
// as node-postgres with Drizzle's own type parsers reads them, so that
// Drizzle's columns map them as they do on node-postgres: booleans and the
// smaller numbers as themselves, `int8` and `numeric` as text, dates, times
// and intervals as text, `json` and `jsonb` as text (the column parses it, so
// a JSON string "123" is parsed once and stays a string), arrays as arrays
// of their elements read the same way. Pure functions, no host, no Drizzle.

/**
 * `query` for a select (also one whose main statement after `with` is a
 * select, and a set operation of selects, which Drizzle writes as
 * `(select ...) union (select ...)`), `execute` otherwise.
 */
export function operationOf(sql) {
  return mainVerb(sql) === "select" ? "query" : "execute";
}

/**
 * The word that says what the statement does (`select`, `insert`,
 * `update`, `delete`, ...), lower-cased: its first word, or, after a `with`
 * list, the main statement's; looking inside a statement that begins with a
 * parenthesis. Comments, quoted identifiers, string constants (also E''
 * and dollar-quoted) and parameters are skipped. Null when there is none.
 */
export function mainVerb(sql) {
  const text = String(sql);
  return verbIn(text, tokens(text, 0, text.length));
}

function verbIn(text, list) {
  const first = list[0];
  if (first === undefined) return null;
  if (first.type === "group") return verbIn(text, tokens(text, first.start, first.end));
  if (first.type !== "word") return null;
  if (first.text !== "with") return first.text;
  // with [recursive] name [(columns)] as [not] [materialized] (body) [, ...] main
  let i = 1;
  const word = (w) => list[i]?.type === "word" && list[i].text === w;
  if (word("recursive")) i += 1;
  for (;;) {
    if (list[i]?.type !== "word" && list[i]?.type !== "ident") return fallback(list);
    i += 1;
    if (list[i]?.type === "group") i += 1;
    if (!word("as")) return fallback(list);
    i += 1;
    if (word("not")) i += 1;
    if (word("materialized")) i += 1;
    if (list[i]?.type !== "group") return fallback(list);
    i += 1;
    if (list[i]?.type === "punct" && list[i].text === ",") {
      i += 1;
      continue;
    }
    break;
  }
  return verbIn(text, list.slice(i));
}

/** For a `with` list that does not read as one: the first statement word at its level. */
function fallback(list) {
  const found = list.find((t) => t.type === "word" && VERBS.has(t.text));
  return found === undefined ? null : found.text;
}

const VERBS = new Set(["select", "insert", "update", "delete", "merge", "values", "table"]);

/**
 * The tokens of `text` from `start` to `end` at their own level: words
 * (lower-cased), quoted identifiers, parenthesised groups (their inside not
 * read), and single punctuation characters. Comments, string constants and
 * parameters give no token.
 */
function tokens(text, start, end) {
  const out = [];
  let i = start;
  while (i < end) {
    const c = text[i];
    if (c === "-" && text[i + 1] === "-") {
      const eol = text.indexOf("\n", i);
      i = eol === -1 || eol > end ? end : eol + 1;
    } else if (c === "/" && text[i + 1] === "*") {
      // Postgres block comments nest.
      let level = 1;
      i += 2;
      while (i < end && level > 0) {
        if (text[i] === "/" && text[i + 1] === "*") {
          level += 1;
          i += 2;
        } else if (text[i] === "*" && text[i + 1] === "/") {
          level -= 1;
          i += 2;
        } else {
          i += 1;
        }
      }
    } else if (c === '"') {
      const after = skipQuoted(text, i, '"', false);
      out.push({ type: "ident" });
      i = after;
    } else if (c === "'") {
      const escapes = (text[i - 1] === "e" || text[i - 1] === "E") && !isWordChar(text[i - 2]);
      i = skipQuoted(text, i, "'", escapes);
    } else if (c === "$" && !isWordChar(text[i - 1])) {
      const tag = /^\$([A-Za-z_][A-Za-z0-9_]*)?\$/.exec(text.slice(i, i + 66));
      if (tag) {
        const close = text.indexOf(tag[0], i + tag[0].length);
        i = close === -1 ? end : close + tag[0].length;
      } else {
        // A parameter, $1.
        i += 1;
        while (i < end && /[0-9]/.test(text[i])) i += 1;
      }
    } else if (c === "(") {
      const close = closing(text, i, end);
      out.push({ type: "group", start: i + 1, end: close });
      i = close + 1;
    } else if (isWordStart(c)) {
      let j = i + 1;
      while (j < end && isWordChar(text[j])) j += 1;
      out.push({ type: "word", text: text.slice(i, j).toLowerCase() });
      i = j;
    } else if (/\s/.test(c)) {
      i += 1;
    } else {
      out.push({ type: "punct", text: c });
      i += 1;
    }
  }
  return out;
}

/** The index of the parenthesis that closes the one at `open` (or `end`), skipping what `tokens` skips. */
function closing(text, open, end) {
  let depth = 0;
  let i = open;
  while (i < end) {
    const c = text[i];
    if (c === "(") {
      depth += 1;
      i += 1;
    } else if (c === ")") {
      depth -= 1;
      if (depth === 0) return i;
      i += 1;
    } else if (c === "-" && text[i + 1] === "-") {
      const eol = text.indexOf("\n", i);
      i = eol === -1 || eol > end ? end : eol + 1;
    } else if (c === "/" && text[i + 1] === "*") {
      const close = text.indexOf("*/", i + 2);
      i = close === -1 ? end : close + 2;
    } else if (c === '"') {
      i = skipQuoted(text, i, '"', false);
    } else if (c === "'") {
      const escapes = (text[i - 1] === "e" || text[i - 1] === "E") && !isWordChar(text[i - 2]);
      i = skipQuoted(text, i, "'", escapes);
    } else if (c === "$" && !isWordChar(text[i - 1])) {
      const tag = /^\$([A-Za-z_][A-Za-z0-9_]*)?\$/.exec(text.slice(i, i + 66));
      if (tag) {
        const close = text.indexOf(tag[0], i + tag[0].length);
        i = close === -1 ? end : close + tag[0].length;
      } else {
        i += 1;
      }
    } else if (isWordStart(c)) {
      i += 1;
      while (i < end && isWordChar(text[i])) i += 1;
    } else {
      i += 1;
    }
  }
  return end;
}

function isWordStart(c) {
  return c !== undefined && /[A-Za-z_\u0080-\uffff]/.test(c);
}

function isWordChar(c) {
  return c !== undefined && /[A-Za-z0-9_$\u0080-\uffff]/.test(c);
}

/** The index just past the quoted text that starts at `i`; a doubled quote stays inside. */
function skipQuoted(text, i, quote, backslashEscapes) {
  let j = i + 1;
  while (j < text.length) {
    if (backslashEscapes && text[j] === "\\") {
      j += 2;
    } else if (text[j] === quote) {
      if (text[j + 1] === quote) {
        j += 2;
      } else {
        return j + 1;
      }
    } else {
      j += 1;
    }
  }
  return text.length;
}

// ---------------------------------------------------------------- parameters

/**
 * A parameter as the raw interface takes it: null, a string or a boolean.
 * Numbers and BigInts as their decimal text, a Date as its ISO instant, an
 * array as a Postgres array literal, bytes as `\x` hex, any other object as
 * JSON. The server reads each as its type in the statement.
 */
export function encodeParam(value) {
  if (value === null || value === undefined) return null;
  switch (typeof value) {
    case "string":
    case "boolean":
      return value;
    case "number":
      return String(value);
    case "bigint":
      return value.toString();
    case "object":
      if (value instanceof Date) return value.toISOString();
      if (Array.isArray(value)) return arrayLiteral(value);
      if (value instanceof Uint8Array) return hex(value);
      return JSON.stringify(value);
    default:
      throw new TypeError(`a ${typeof value} cannot be sent to the database as a parameter`);
  }
}

/** A Postgres array literal: every element quoted, null as NULL, nested arrays nested. */
export function arrayLiteral(array) {
  let out = "{";
  for (let i = 0; i < array.length; i += 1) {
    if (i > 0) out += ",";
    const item = array[i];
    if (Array.isArray(item)) {
      out += arrayLiteral(item);
      continue;
    }
    const text = encodeParam(item);
    if (text === null) {
      out += "NULL";
    } else {
      out += `"${String(text).replace(/[\\"]/g, (c) => `\\${c}`)}"`;
    }
  }
  return `${out}}`;
}

function hex(bytes) {
  let out = "\\x";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

// ---------------------------------------------------------------- results

// Type OIDs (pg_type.oid; stable across Postgres versions).
const BOOL = 16;
const INT2 = 21;
const INT4 = 23;
const OID = 26;
const FLOAT4 = 700;
const FLOAT8 = 701;

/** Each array type's element type, for the types read as more than text. */
const ELEMENT = new Map([
  [1000, BOOL],
  [1005, INT2],
  [1007, INT4],
  [1028, OID],
  [1021, FLOAT4],
  [1022, FLOAT8],
  // Arrays whose elements stay text: bytea, char, name, int8, int2vector,
  // regproc, text, oidvector, bpchar, varchar, xml, json, jsonb, uuid,
  // numeric, date, time, timestamp, timestamptz, interval, timetz, bit,
  // varbit, inet, cidr, macaddr, macaddr8, money, tsvector, citext-less
  // ones. Their elements are read as text.
  ...[
    1001, 1002, 1003, 1016, 1006, 1008, 1009, 1013, 1014, 1015, 143, 199, 3807, 2951, 1231, 1182, 1183, 1115, 1185,
    1187, 1270, 1561, 1563, 1041, 651, 1040, 775, 791, 3643,
  ].map((oid) => [oid, 0]),
]);

/** One value as Postgres printed it, read as node-postgres with Drizzle's parsers would. */
export function decodeValue(type, text) {
  if (text === null || text === undefined) return null;
  if (typeof text !== "string") return text;
  switch (type) {
    case BOOL:
      return text === "t" || text === "true";
    case INT2:
    case INT4:
    case OID:
      return Number.parseInt(text, 10);
    case FLOAT4:
    case FLOAT8:
      return Number.parseFloat(text);
    default: {
      const element = ELEMENT.get(type);
      if (element === undefined) return text;
      return parseArray(text, (item) => decodeValue(element, item));
    }
  }
}

/** Every row (an array of texts) read by the types of its columns. */
export function decodeRows(types, rows) {
  if (!Array.isArray(types)) return rows;
  const out = new Array(rows.length);
  for (let r = 0; r < rows.length; r += 1) {
    const row = rows[r];
    const values = new Array(row.length);
    for (let c = 0; c < row.length; c += 1) values[c] = decodeValue(types[c], row[c]);
    out[r] = values;
  }
  return out;
}

/** Rows as objects by column name, as node-postgres gives them when not asked for arrays. */
export function rowsAsObjects(columns, rows) {
  return rows.map((row) => {
    const object = {};
    for (let c = 0; c < columns.length; c += 1) object[columns[c]] = row[c];
    return object;
  });
}

/**
 * A Postgres array as it prints (`{a,"b c",NULL,{1,2}}`, maybe with
 * dimensions first: `[0:1]={...}`), each element given to `element` as its
 * text, or null for NULL. Quoted elements keep their commas, braces,
 * quotes and backslashes; an unquoted NULL is null, a quoted "NULL" is the
 * text.
 */
export function parseArray(text, element = (item) => item) {
  let i = 0;
  if (text[0] === "[") {
    const equals = text.indexOf("=");
    if (equals === -1) return text;
    i = equals + 1;
  }
  const parsed = parseLevel(text, i, element);
  return parsed === null ? text : parsed.value;
}

function parseLevel(text, start, element) {
  if (text[start] !== "{") return null;
  const items = [];
  let i = start + 1;
  if (text[i] === "}") return { value: items, end: i + 1 };
  for (;;) {
    if (text[i] === "{") {
      const inner = parseLevel(text, i, element);
      if (inner === null) return null;
      items.push(inner.value);
      i = inner.end;
    } else if (text[i] === '"') {
      let value = "";
      i += 1;
      while (i < text.length && text[i] !== '"') {
        if (text[i] === "\\") i += 1;
        value += text[i] ?? "";
        i += 1;
      }
      if (i >= text.length) return null;
      i += 1;
      items.push(element(value));
    } else {
      let value = "";
      while (i < text.length && text[i] !== "," && text[i] !== "}") {
        if (text[i] === "\\") i += 1;
        value += text[i] ?? "";
        i += 1;
      }
      items.push(value.toUpperCase() === "NULL" ? element(null) : element(value));
    }
    if (text[i] === ",") {
      i += 1;
    } else if (text[i] === "}") {
      return { value: items, end: i + 1 };
    } else {
      return null;
    }
  }
}
