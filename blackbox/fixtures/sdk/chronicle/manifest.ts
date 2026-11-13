import { defineManifest } from "@vm/sdk";

// The SDK fixture of reading the Change history (blackbox/tests/sdk_history_*.rs):
// the own database, and `devops` (read) and `ops` (write) only so that
// `history` can be forced on a shared database's handle.
export default defineManifest({
  name: "sdk-chronicle",
  description: "Shows who changed what, from the Change history of its own database.",
  resources: {
    devops: { kind: "postgres", access: "read" },
    ops: { kind: "postgres", access: "write" },
  },
});
