import { defineManifest, type TrustedContext } from "@vm/sdk";

const manifest = defineManifest({
  signIn: "optional", // /health is public
  resources: {
    devops: { kind: "postgres", access: "read" },
    facilities: { kind: "postgres", access: "write" },
  },
});
export default manifest;

/** The context type every handler in this Application takes. */
export type Ctx = TrustedContext<typeof manifest>;
