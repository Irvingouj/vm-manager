// Refusals, shared by all three candidates. The host decides every refusal;
// this file only gives it a shape a Coding agent can switch on.

export type AccessLevel = "structure" | "read" | "write";

/** Which condition failed. One variant per condition the host checks. */
export type Refusal =
  /** The operation was made as the User, but this request has no signed-in User. */
  | { reason: "no-user" }
  /** No request was in flight when the operation was made (module top level, or a timer after the response). */
  | { reason: "no-request" }
  /** The Application's manifest does not declare this Resource (Declared access). */
  | { reason: "not-declared"; resource: string }
  /** The manifest declares the Resource, but at a lower Access level than the operation needs. */
  | { reason: "declared-lower"; resource: string; declared: AccessLevel; needed: AccessLevel }
  /** None of the User's Groups holds a Grant on the Resource at the needed Access level. */
  | { reason: "no-grant"; resource: string; needed: AccessLevel; held: AccessLevel | null }
  /** The Application tried to act as itself on a Resource other than its own database. */
  | { reason: "not-own-database"; resource: string }
  /** The Resource is declared but no longer registered by IT (removed or renamed after deploy). */
  | { reason: "not-registered"; resource: string };

/**
 * Thrown by every SDK operation the host refuses. Not thrown for SQL errors
 * (those are a different error type, owned by the query-surface ticket).
 * If a handler lets it escape, the host answers 403 (or a sign-in redirect for
 * "no-user") and records the refusal in the Operation log.
 */
export declare class Refused extends Error {
  private constructor();
  readonly name: "Refused";
  readonly refusal: Refusal;
  /** One sentence for a Coding agent, e.g. "Declare 'payroll' in vm.manifest.json at 'read' or 'write'." */
  readonly fix: string;
}
