#!/usr/bin/env node
// The SDK's build-side program (blackbox/CONTRACT.md, "The SDK: The tooling"):
//
//   node <sdk>/tooling/main.js manifest <project directory>
//   node <sdk>/tooling/main.js bundle <project directory>
//
// Exit code 0 with the answer on standard output; 2 with { "error": ... } on
// standard output for a Manifest that cannot be taken or a project that does
// not build; 1 with a message on standard error for anything else.

import { existsSync, statSync } from "node:fs";
import { bundleProject } from "./bundle.js";
import { readManifest } from "./manifest.js";

const USAGE = "usage: node tooling/main.js manifest|bundle <project directory>";

function out(text, code) {
  process.exitCode = code;
  process.stdout.write(text);
}

async function main(args) {
  const [command, project, ...rest] = args;
  if ((command !== "manifest" && command !== "bundle") || project === undefined || rest.length > 0) {
    throw new Error(USAGE);
  }
  if (!existsSync(project) || !statSync(project).isDirectory()) {
    throw new Error(`no project directory ${project}`);
  }
  if (command === "manifest") {
    const read = readManifest(project);
    if (read.error) return out(`${JSON.stringify({ error: read.error })}\n`, 2);
    return out(`${JSON.stringify(read.manifest)}\n`, 0);
  }
  const built = await bundleProject(project);
  if (built.error) return out(`${JSON.stringify({ error: built.error })}\n`, 2);
  return out(built.bundle, 0);
}

main(process.argv.slice(2)).catch((error) => {
  process.exitCode = 1;
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
});
