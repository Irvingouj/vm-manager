import { defineManifest } from "@vm/sdk";

// The project the type-test cases are compiled with: `devops` declared at
// read, `hr` at write, both with a generated link.
export default defineManifest({
  name: "type-tests",
  description: "The Manifest the type tests are compiled against.",
  resources: {
    devops: { kind: "postgres", access: "read" },
    hr: { kind: "postgres", access: "write" },
  },
});
