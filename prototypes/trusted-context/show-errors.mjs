// For each `// @ts-expect-error <label>` in <candidate>/type-tests.ts, removes
// that one directive, runs tsc, and prints the compiler message it unmasks.
// Usage: node show-errors.mjs <candidate-dir> [more dirs...]
import { readFileSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const tsc = join(import.meta.dirname, "node_modules", ".bin", "tsc");

for (const dir of process.argv.slice(2)) {
  const testsPath = join(dir, "type-tests.ts");
  const lines = readFileSync(testsPath, "utf8").split("\n");
  const tsconfig = JSON.parse(readFileSync(join(dir, "tsconfig.json"), "utf8"));
  const probeConfig = join(dir, "tsconfig.probe.json");
  const probeFile = join(dir, "type-tests.probe.ts");
  writeFileSync(
    probeConfig,
    JSON.stringify({ ...tsconfig, include: tsconfig.include.map((p) => (p === "type-tests.ts" ? "type-tests.probe.ts" : p)) }),
  );
  console.log(`\n=== ${dir} ===`);
  try {
    lines.forEach((line, i) => {
      const m = line.match(/\/\/ @ts-expect-error (\S+)/);
      if (!m) return;
      const probe = [...lines];
      probe[i] = line.replace("@ts-expect-error", "(removed)");
      writeFileSync(probeFile, probe.join("\n"));
      const r = spawnSync(tsc, ["-p", probeConfig, "--pretty", "false"], { encoding: "utf8" });
      const out = (r.stdout + r.stderr).trim().replaceAll(probeFile, "type-tests.ts");
      console.log(`\n--- ${m[1]} (line ${i + 2}) exit=${r.status}\n${out}`);
    });
  } finally {
    rmSync(probeFile, { force: true });
    rmSync(probeConfig, { force: true });
  }
}
