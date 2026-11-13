// What a value was inside the Application, as JSON can carry it: a BigInt
// and a Date do not survive JSON.stringify by themselves, and a number, a
// string holding digits and a JSON value all look alike once printed. The
// tests in blackbox/tests/sdk_*.rs assert on these descriptions.
import { OperationFailed, Refused } from "@vm/sdk";

export type Described =
  | { type: "null" }
  | { type: "undefined" }
  | { type: "bigint"; text: string }
  | { type: "date"; iso: string }
  | { type: "number"; value: number; text: string }
  | { type: "string"; value: string }
  | { type: "boolean"; value: boolean }
  | { type: "array"; items: Described[] }
  | { type: "object"; entries: { [key: string]: Described } }
  | { type: "other"; text: string };

export function describe(value: unknown): Described {
  if (value === null) return { type: "null" };
  if (value === undefined) return { type: "undefined" };
  if (typeof value === "bigint") return { type: "bigint", text: value.toString() };
  if (value instanceof Date) {
    return { type: "date", iso: Number.isNaN(value.getTime()) ? "Invalid Date" : value.toISOString() };
  }
  if (typeof value === "number") return { type: "number", value, text: String(value) };
  if (typeof value === "string") return { type: "string", value };
  if (typeof value === "boolean") return { type: "boolean", value };
  if (Array.isArray(value)) return { type: "array", items: value.map(describe) };
  if (typeof value === "object") {
    const entries: { [key: string]: Described } = {};
    for (const [key, item] of Object.entries(value)) entries[key] = describe(item);
    return { type: "object", entries };
  }
  return { type: "other", text: String(value) };
}

/** Each row as an object of described columns. */
export function describeRows(rows: readonly object[]): Described[] {
  return rows.map(describe);
}

/** What a caught error was: which classes it is an instance of, and what it carries. */
export function describeError(error: unknown): Record<string, unknown> {
  const out: Record<string, unknown> = {
    isRefused: error instanceof Refused,
    isOperationFailed: error instanceof OperationFailed,
    isError: error instanceof Error,
    isTypeError: error instanceof TypeError,
  };
  if (error instanceof Error) {
    out.name = error.name;
    out.message = error.message;
  } else {
    out.thrown = String(error);
  }
  if (error instanceof Refused) {
    out.refusal = error.refusal;
    out.fix = error.fix;
  }
  if (error instanceof OperationFailed) {
    out.kind = error.kind;
    out.code = error.code ?? null;
    out.detail = error.detail;
  }
  return out;
}
