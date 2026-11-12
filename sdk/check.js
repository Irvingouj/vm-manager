// The SDK's own check, run by `npm test`: its declarations compile with
// nothing skipped (the projects that use them skip library checks), and
// its modules load. Behaviour is tested from outside, in blackbox/.
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const here = path.dirname(fileURLToPath(import.meta.url));
const options = {
  strict: true,
  noEmit: true,
  skipLibCheck: false,
  target: ts.ScriptTarget.ES2022,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  lib: ["lib.es2022.d.ts", "lib.dom.d.ts"],
  types: [],
  // An own structure of no tables stands in for a project's src/db/schema.ts.
  paths: { "@vm/own-structure": [path.join(here, "check-own-structure.d.ts")] },
};
const program = ts.createProgram([path.join(here, "index.d.ts")], options);
const ours = ts
  .getPreEmitDiagnostics(program)
  .filter((d) => d.file === undefined || !d.file.fileName.includes("/node_modules/"));
for (const d of ours) {
  const where = d.file ? `${path.relative(here, d.file.fileName)}: ` : "";
  console.log(`${where}TS${d.code} ${ts.flattenDiagnosticMessageText(d.messageText, " ")}`);
}

await import("./index.js");
await import("./runtime.js");
await import("./tooling/manifest.js");
await import("./tooling/bundle.js");

if (ours.length > 0) {
  process.exitCode = 1;
} else {
  console.log("ok    @vm/sdk: declarations compile, modules load");
}
