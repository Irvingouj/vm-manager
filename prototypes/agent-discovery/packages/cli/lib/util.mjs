import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";

export class VmError extends Error {
  /** exit: 1 = failed, 2 = refused or needs a person's decision */
  constructor(message, { code = "error", exit = 1, details } = {}) {
    super(message);
    this.code = code;
    this.exit = exit;
    this.details = details;
  }
}

/** Parses argv into positionals and flags. `valued` lists flags that take a value. */
export function parseArgs(argv, valued = []) {
  const positionals = [];
  const flags = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--") {
      positionals.push(...argv.slice(i + 1));
      break;
    }
    if (a.startsWith("--") || (a.startsWith("-") && a.length === 2 && isNaN(Number(a)))) {
      let [k, v] = a.replace(/^--?/, "").split(/=(.*)/s);
      if (k === "h") k = "help";
      if (k === "H") k = "header";
      if (k === "d") k = "data";
      if (valued.includes(k) && v === undefined) v = argv[++i];
      if (valued.includes(k)) {
        if (v === undefined) throw new VmError(`--${k} needs a value.`);
        if (flags[k] === undefined) flags[k] = v;
        else flags[k] = [].concat(flags[k], v);
      } else flags[k] = v ?? true;
    } else positionals.push(a);
  }
  return { positionals, flags };
}

export function findProject(start = process.cwd()) {
  let dir = resolve(start);
  for (;;) {
    if (existsSync(join(dir, "manifest.ts")) && existsSync(join(dir, "package.json"))) return dir;
    const up = dirname(dir);
    if (up === dir) {
      throw new VmError("This is not an Application project: no manifest.ts was found here or in any parent folder.", {
        code: "no-project",
      });
    }
    dir = up;
  }
}

/** Resolves a module from the project (so the project's own drizzle-orm and typescript are used). */
export function projectRequire(root) {
  return createRequire(join(root, "package.json"));
}

export const ownRequire = createRequire(import.meta.url);

export function table(rows, headers) {
  const widths = headers.map((h, i) => Math.max(h.length, ...rows.map((r) => String(r[i] ?? "").length)));
  const line = (cells) => cells.map((c, i) => String(c ?? "").padEnd(widths[i])).join("  ").trimEnd();
  return [line(headers), ...rows.map(line)].join("\n");
}

export function readJson(file) {
  return JSON.parse(readFileSync(file, "utf8"));
}

export function git(root, args) {
  const r = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  return { ok: r.status === 0, out: (r.stdout ?? "").trimEnd(), err: (r.stderr ?? "").trim() };
}

export function plural(n, word, many = word + "s") {
  return `${n} ${n === 1 ? word : many}`;
}
