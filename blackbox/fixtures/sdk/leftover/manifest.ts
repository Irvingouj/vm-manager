import { defineManifest } from "@vm/sdk";

// A fixture that uses its handles when no request is in flight
// (blackbox/tests/sdk_refusals.rs). It declares no Resource: the own
// database is enough to show the host's `no-request`.
export default defineManifest({
  name: "sdk-leftover",
  description: "Uses its database handles at the top level and after the response.",
  resources: {},
});
