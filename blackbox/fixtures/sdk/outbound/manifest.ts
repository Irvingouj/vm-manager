import { defineManifest } from "@vm/sdk";

// The SDK's fixture of outbound calls (blackbox/tests/outbound_sdk.rs): no
// Resource, and a handler that calls the outside with plain `fetch`.
export default defineManifest({
  name: "sdk-outbound",
  description: "Calls the outside with fetch; refusals and failures through the SDK.",
  resources: {},
});
