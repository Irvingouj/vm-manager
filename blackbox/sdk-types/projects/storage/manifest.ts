import { defineManifest } from "@vm/sdk";

// The project the Storage type-test cases are compiled with (CONTRACT.md,
// "Storage: Through the SDK"): two databases, `devops` at read and `hr` at
// write, neither pulled nor used typed here; and two Storage Resources,
// `marketing` at write and `archive` at read.
export default defineManifest({
  name: "type-tests-storage",
  description: "The Manifest the Storage type tests are compiled against.",
  resources: {
    devops: { kind: "postgres", access: "read" },
    hr: { kind: "postgres", access: "write" },
    marketing: { kind: "storage", access: "write" },
    archive: { kind: "storage", access: "read" },
  },
});
