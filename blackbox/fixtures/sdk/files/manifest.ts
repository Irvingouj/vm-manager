import { defineManifest } from "@vm/sdk";

// The SDK's fixture of Storage (blackbox/tests/storage_sdk.rs): `marketing`
// is written, `archive` only read, and `devops` is a database, so that a
// name of one kind can be asked for as the other.
export default defineManifest({
  name: "sdk-files",
  description: "Files listed, read, written and deleted through the SDK.",
  resources: {
    devops: { kind: "postgres", access: "read" },
    marketing: { kind: "storage", access: "write" },
    archive: { kind: "storage", access: "read" },
  },
});
