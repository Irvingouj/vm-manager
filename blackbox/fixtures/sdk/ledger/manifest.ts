import { defineManifest } from "@vm/sdk";

// The SDK fixture of transactions (blackbox/tests/sdk_transactions.rs):
// `devops` read through the read login, `ops` (the same shared database)
// written, and the own database.
export default defineManifest({
  name: "sdk-ledger",
  description: "Notes and counters, changed together in transactions through the SDK.",
  resources: {
    devops: { kind: "postgres", access: "read" },
    ops: { kind: "postgres", access: "write" },
  },
});
