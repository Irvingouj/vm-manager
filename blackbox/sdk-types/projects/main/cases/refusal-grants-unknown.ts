// Application code tells `grants-unknown` apart as it does `no-grant`: the
// refusal names the Resource, a string, and nothing else (CONTRACT.md, "A
// write whose answer is lost": the SDK's `Refusal` type has it).
import { Refused } from "@vm/sdk";

export function resourceOf(error: Refused): string | undefined {
  const refusal = error.refusal;
  if (refusal.reason === "grants-unknown") {
    const resource: string = refusal.resource;
    return resource;
  }
  return undefined;
}

export function heldOf(error: Refused): unknown {
  const refusal = error.refusal;
  if (refusal.reason === "grants-unknown") {
    // @expect TS2339 Property 'held' does not exist
    return refusal.held;
  }
  return undefined;
}
