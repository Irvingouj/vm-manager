// `bundle`: builds a project into the one classic script the platform runs
// (blackbox/CONTRACT.md, "The SDK: The tooling" and "What an Application is
// handed"). esbuild bundles an entry that brings together the Application
// (src/index.ts), its structures (the own schema, each Resource's schema and
// relations) and the SDK's runtime, and registers the handler as
// globalThis.default. Nothing is type-checked: that is tsc's job.

import { existsSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { build } from "esbuild";

/** The esbuild settings of the bundle (docs/research/drizzle-in-the-guest.md, section 1). */
export const BUILD = Object.freeze({
  bundle: true,
  write: false,
  format: "iife",
  platform: "neutral",
  mainFields: ["module", "main"],
  conditions: ["import", "default"],
  target: "es2022",
  treeShaking: true,
  legalComments: "none",
  charset: "utf8",
  logLevel: "silent",
});

/** The Resources with a structure: directories of resources/ holding a schema.ts, in name order. */
export function structuredResources(project) {
  const dir = path.join(project, "resources");
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => {
      const full = path.join(dir, name);
      return statSync(full).isDirectory() && existsSync(path.join(full, "schema.ts"));
    })
    .sort();
}

/** The entry module's source, its imports relative to the project directory. */
export function entrySource(project) {
  const lines = ['import app from "./src/index.ts";', 'import { serve } from "@vm/sdk/runtime";'];
  if (existsSync(path.join(project, "src", "db", "schema.ts"))) {
    lines.push('import * as own from "./src/db/schema.ts";');
  } else {
    lines.push("const own = {};");
  }
  const shared = [];
  structuredResources(project).forEach((name, index) => {
    const at = (file) => JSON.stringify(`./resources/${name}/${file}`);
    lines.push(`import * as schema${index} from ${at("schema.ts")};`);
    if (existsSync(path.join(project, "resources", name, "relations.ts"))) {
      lines.push(`import * as relations${index} from ${at("relations.ts")};`);
      shared.push(`${JSON.stringify(name)}: { ...schema${index}, ...relations${index} }`);
    } else {
      shared.push(`${JSON.stringify(name)}: { ...schema${index} }`);
    }
  });
  lines.push(`globalThis.default = serve(app, { own: { ...own }, shared: { ${shared.join(", ")} } });`);
  return `${lines.join("\n")}\n`;
}

/**
 * Builds the project in `project`. Answers `{ bundle }` (the script's text)
 * or `{ error }` (`{ reason: "build-failed", message, errors: [{ file, line, message }] }`).
 * `minify` is for measuring; the tooling's command does not minify.
 */
export async function bundleProject(project, { minify = false } = {}) {
  const root = path.resolve(project);
  const tsconfig = path.join(root, "tsconfig.json");
  try {
    const result = await build({
      ...BUILD,
      minify,
      absWorkingDir: root,
      tsconfig: existsSync(tsconfig) ? tsconfig : undefined,
      stdin: {
        contents: entrySource(root),
        resolveDir: root,
        sourcefile: "vm-entry.js",
        loader: "js",
      },
    });
    const [output] = result.outputFiles;
    return { bundle: output.text };
  } catch (failure) {
    if (!Array.isArray(failure?.errors)) throw failure;
    const errors = failure.errors.map((e) => ({
      file: e.location?.file ?? null,
      line: e.location?.line ?? null,
      message: e.text,
    }));
    const first = errors[0];
    const where = first?.file ? `${first.file}${first.line ? `:${first.line}` : ""}: ` : "";
    return {
      error: {
        reason: "build-failed",
        message: `The project does not build (${errors.length} error${errors.length === 1 ? "" : "s"}): ${where}${first?.message ?? "esbuild failed"}`,
        errors,
      },
    };
  }
}
