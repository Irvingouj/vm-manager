import { defineManifest } from "@vm/sdk";

// A second Application with a table of the same name as sdk-chronicle's
// (blackbox/tests/sdk_history_boundaries.rs): each reads only its own history.
export default defineManifest({
  name: "sdk-chronicle-twin",
  description: "Another Application with a table called tasks.",
  resources: {},
});
