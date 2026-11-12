import { defineManifest } from "@vm/sdk";

// As projects/main, but `devops` has no generated link
// (resources/devops/index.ts is missing): its structure has not been pulled.
export default defineManifest({
  name: "type-tests-unpulled",
  description: "The Manifest of a project whose devops structure was never pulled.",
  resources: {
    devops: { kind: "postgres", access: "read" },
    hr: { kind: "postgres", access: "write" },
  },
});
