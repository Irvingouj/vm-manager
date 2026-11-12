// Bundles the Application with esbuild: for `vm dev` an ES module for the local
// host, for `vm deploy` the single classic script the platform runtime runs.
import { existsSync, mkdirSync, readdirSync, writeFileSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { build } from "esbuild";
import { gzipSync } from "node:zlib";

export function pulledNames(root) {
  const dir = join(root, "resources");
  if (!existsSync(dir)) return [];
  return readdirSync(dir).filter((n) => existsSync(join(dir, n, "schema.ts")));
}

function entrySource(root, outDir, names) {
  const rel = (p) => JSON.stringify(relative(outDir, join(root, p)).split("\\").join("/"));
  const lines = [
    `import app from ${rel("src/index.ts")};`,
    `import { serve } from "@vm/sdk/runtime";`,
    existsSync(join(root, "src/db/schema.ts")) ? `import * as own from ${rel("src/db/schema.ts")};` : `const own = {};`,
  ];
  const shared = [];
  names.forEach((n, i) => {
    lines.push(`import * as s${i} from ${rel(`resources/${n}/schema.ts`)};`);
    if (existsSync(join(root, "resources", n, "relations.ts"))) {
      lines.push(`import * as r${i} from ${rel(`resources/${n}/relations.ts`)};`);
      shared.push(`${JSON.stringify(n)}: { ...s${i}, ...r${i} }`);
    } else shared.push(`${JSON.stringify(n)}: s${i}`);
  });
  lines.push(`export default serve(app, { own: { ...own }, shared: { ${shared.join(", ")} } });`);
  return lines.join("\n") + "\n";
}

export async function bundle(root, { mode }) {
  const outDir = join(root, ".vm", mode === "dev" ? "dev" : "build");
  mkdirSync(outDir, { recursive: true });
  const entry = join(outDir, "entry.mjs");
  writeFileSync(entry, entrySource(root, outDir, pulledNames(root)));
  const outfile = join(outDir, mode === "dev" ? "app.mjs" : "app.js");
  const common = {
    entryPoints: [entry],
    bundle: true,
    outfile,
    logLevel: "silent",
    treeShaking: true,
    legalComments: "none",
    absWorkingDir: root,
    tsconfig: join(root, "tsconfig.json"),
  };
  const options =
    mode === "dev"
      ? { ...common, format: "esm", platform: "neutral", mainFields: ["module", "main"], conditions: ["import", "default"], target: "es2022", sourcemap: "inline" }
      : { ...common, format: "iife", globalName: "__application", platform: "neutral", mainFields: ["module", "main"], conditions: ["import", "default"], target: "es2022", minify: true };
  try {
    await build(options);
  } catch (e) {
    const errors = (e.errors ?? []).map((m) => `  ${m.location ? `${m.location.file}:${m.location.line}: ` : ""}${m.text}`);
    const err = new Error(`The Application does not build:\n${errors.join("\n") || e.message}`);
    err.buildErrors = errors;
    throw err;
  }
  const code = readFileSync(outfile);
  return { outfile, bytes: code.length, gzip: gzipSync(code).length };
}
