import { defineManifest } from "@vm/sdk";

// What this Application is and what it reaches. The platform reads this file
// without running it, so the argument of defineManifest stays a plain
// literal: strings, numbers, booleans, null, arrays and objects, written out.
//
// `resources` lists each shared database (Resource) the Application uses, by
// name, at "read" or "write": { devops: { kind: "postgres", access: "read" } }.
export default defineManifest({
  name: "{{name}}",
  description: "A new Application: say here, in one sentence, what it is for and for whom.",
  resources: {},
});
