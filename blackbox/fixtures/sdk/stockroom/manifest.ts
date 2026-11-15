import { defineManifest } from "@vm/sdk";

// The fixture project of the structure pull (blackbox/tests/pull_*.rs).
// `stock` is the fixture shared database of blackbox/src/structure.rs; the
// tests pull its definition into a copy of this project.
export default defineManifest({
  name: "sdk-stockroom",
  description: "Sites and the items they stock, read through a pulled definition.",
  resources: {
    stock: { kind: "postgres", access: "write" },
  },
});
