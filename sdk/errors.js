// The two errors an operation rejects with (blackbox/CONTRACT.md, "The SDK:
// Refusals and failures"). Only the SDK makes them: each constructor wants a
// key that never leaves this module, so `new Refused(...)` in Application
// code throws a TypeError. This module is not in the package's exports;
// Application code gets the classes from "@vm/sdk" and the runtime gets the
// makers below.

const MINT = Symbol("made by @vm/sdk");

const freezeCopy = (value) => (value !== null && typeof value === "object" ? Object.freeze({ ...value }) : {});

export class Refused extends Error {
  constructor(key, refusal, fix) {
    if (key !== MINT) {
      throw new TypeError(
        "Refused is made by the platform only: Application code cannot construct one. Catch it with `error instanceof Refused`.",
      );
    }
    const reason = refusal !== null && typeof refusal === "object" ? String(refusal.reason) : "unknown";
    const sentence = typeof fix === "string" ? fix : "";
    super(`${reason}: ${sentence}`);
    Object.defineProperties(this, {
      refusal: { value: freezeCopy(refusal), enumerable: true },
      fix: { value: sentence, enumerable: true },
    });
  }
}
Object.defineProperty(Refused.prototype, "name", { value: "Refused", writable: true, configurable: true });

export class OperationFailed extends Error {
  constructor(key, error) {
    if (key !== MINT) {
      throw new TypeError(
        "OperationFailed is made by the platform only: Application code cannot construct one. Catch it with `error instanceof OperationFailed`.",
      );
    }
    const detail = freezeCopy(error);
    const kind = typeof detail.kind === "string" ? detail.kind : "unknown";
    const code = kind === "database" && typeof detail.code === "string" ? detail.code : undefined;
    const said = typeof detail.message === "string" ? `: ${detail.message}` : detail.limit ? `: ${detail.limit}` : "";
    super(`${kind}${code ? ` ${code}` : ""}${said}`);
    Object.defineProperties(this, {
      kind: { value: kind, enumerable: true },
      code: { value: code, enumerable: true },
      detail: { value: detail, enumerable: true },
    });
  }
}
Object.defineProperty(OperationFailed.prototype, "name", {
  value: "OperationFailed",
  writable: true,
  configurable: true,
});

/** The `Refused` of the host's `{ refused: { refusal, fix } }`. */
export function refusedOf(refused) {
  return new Refused(MINT, refused?.refusal, refused?.fix);
}

/** The `OperationFailed` of the host's `{ error: { kind, ... } }`. */
export function failedOf(error) {
  return new OperationFailed(MINT, error);
}
