import { defineManifest } from "@vm/sdk";

// The SDK's main black-box fixture (blackbox/tests/sdk_*.rs). `devops` is
// read through the read login; `ops`, over the same shared database, is
// where the Application writes. The quoted key and `as const` are part of a
// plain literal (CONTRACT.md, "The Manifest").
export default defineManifest({
  name: "sdk-board",
  description: "Incidents, services and notes, read and written through the SDK.",
  resources: {
    devops: { kind: "postgres", access: "read" },
    "ops": { kind: "postgres", access: "write" },
  },
} as const);
