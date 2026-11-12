import { defineManifest, type TrustedContext } from "@vm/sdk";

// What this Application is and which shared Resources it uses.
// Keep it a plain literal: the platform reads it without running your code.
const manifest = defineManifest({
  name: "my-app",
  description: "Describe in one sentence what this Application does and for whom.",
  resources: {
    // <resource name>: { kind: "postgres", access: "read" },
  },
});
export default manifest;

/** The type of `ctx` in this Application's handlers. */
export type Ctx = TrustedContext<typeof manifest>;
