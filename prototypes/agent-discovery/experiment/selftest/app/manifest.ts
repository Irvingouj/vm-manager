import { defineManifest, type TrustedContext } from "@vm/sdk";

const manifest = defineManifest({
  name: "incident-notes",
  description: "Facilities staff see open incidents and keep their own notes on them.",
  resources: {
    devops: { kind: "postgres", access: "read" },
  },
});
export default manifest;

export type Ctx = TrustedContext<typeof manifest>;
