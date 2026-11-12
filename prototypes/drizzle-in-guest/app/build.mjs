// Bundle the Application into the one shape the runtime accepts: a single
// classic script (no ES modules, no Node APIs) that sets globalThis.default.
//
//   node build.mjs            -> dist/app.js (+ dist/meta.json)
//   node build.mjs --minify   -> dist/app.min.js
import { build } from "esbuild";
import { gzipSync } from "node:zlib";
import { readFileSync, writeFileSync } from "node:fs";

const minify = process.argv.includes("--minify");
const outfile = minify ? "dist/app.min.js" : "dist/app.js";

const result = await build({
  entryPoints: ["src/app.ts"],
  bundle: true,
  format: "iife", // a classic script: no import/export left
  platform: "neutral", // no Node built-ins, no "browser" field tricks
  mainFields: ["module", "main"],
  conditions: ["import", "default"],
  target: "es2022",
  minify,
  treeShaking: true,
  legalComments: "none",
  metafile: true,
  outfile,
  logLevel: "warning",
});

writeFileSync(minify ? "dist/meta.min.json" : "dist/meta.json", JSON.stringify(result.metafile));
const code = readFileSync(outfile);
const drizzleBytes = Object.entries(result.metafile.outputs[outfile].inputs)
  .filter(([path]) => path.includes("node_modules/drizzle-orm"))
  .reduce((n, [, i]) => n + i.bytesInOutput, 0);
console.log(
  JSON.stringify({ outfile, bytes: code.length, gzip: gzipSync(code).length, drizzleBytesInOutput: drizzleBytes }),
);
