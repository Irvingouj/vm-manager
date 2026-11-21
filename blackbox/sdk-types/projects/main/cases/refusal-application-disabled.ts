// Application code tells `application-disabled` apart by its reason alone:
// the refusal has nothing else, no Resource (CONTRACT.md, "What Platform
// administrators see and stop: Requests in flight": the SDK's `Refusal` type
// has `{ readonly reason: "application-disabled" }`).
import { Refused } from "@vm/sdk";

export function isDisabled(error: Refused): boolean {
  const refusal = error.refusal;
  if (refusal.reason === "application-disabled") {
    const reason: "application-disabled" = refusal.reason;
    return reason === "application-disabled";
  }
  return false;
}

export function resourceOf(error: Refused): unknown {
  const refusal = error.refusal;
  if (refusal.reason === "application-disabled") {
    // @expect TS2339 Property 'resource' does not exist
    return refusal.resource;
  }
  return undefined;
}
