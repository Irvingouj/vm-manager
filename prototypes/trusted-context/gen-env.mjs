// Stand-in for a platform CLI command (`vm types`): reads vm.manifest.json and
// writes vm-env.d.ts, which augments the SDK's DeclaredAccess interface.
// Usage: node gen-env.mjs <candidate-dir>
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const dir = process.argv[2];
const manifest = JSON.parse(readFileSync(join(dir, "vm.manifest.json"), "utf8"));
const entries = Object.entries(manifest.resources)
  .map(([name, r]) => `    ${JSON.stringify(name)}: { kind: ${JSON.stringify(r.kind)}; access: ${JSON.stringify(r.access)} };`)
  .join("\n");
const out = `// GENERATED from vm.manifest.json by gen-env.mjs. Do not edit.
export {};
declare module "@vm/sdk" {
  interface DeclaredAccess {
${entries}
  }
}
`;
writeFileSync(join(dir, "vm-env.d.ts"), out);
console.log(`wrote ${join(dir, "vm-env.d.ts")}`);
