// Reads manifest.ts without running it. The manifest must be a plain literal:
// the platform turns it into the Application's Declared access.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { transformSync } from "esbuild";
import { parse } from "acorn";
import { VmError } from "./util.mjs";

const NAME_RE = /^[a-z][a-z0-9-]{1,38}[a-z0-9]$/;

function describe(node) {
  switch (node.type) {
    case "SpreadElement":
      return "a spread (...)";
    case "Identifier":
      return `a variable (${node.name})`;
    case "CallExpression":
      return "a function call";
    case "MemberExpression":
      return "a value read from another object";
    case "TemplateLiteral":
      return "a template string with ${...}";
    case "ConditionalExpression":
    case "LogicalExpression":
    case "BinaryExpression":
      return "a computed expression";
    default:
      return `an expression (${node.type})`;
  }
}

function literal(node, src, path, errors) {
  const where = path || "the manifest";
  const bad = (n) => errors.push(`${where}: ${describe(n)} is not allowed; write the value out in full. Found: ${src.slice(n.start, n.end).slice(0, 80)}`);
  switch (node.type) {
    case "Literal":
      return node.value;
    case "TemplateLiteral":
      if (node.expressions.length === 0) return node.quasis[0].value.cooked;
      bad(node);
      return undefined;
    case "ArrayExpression":
      return node.elements.map((e, i) => (e ? literal(e, src, `${path}[${i}]`, errors) : null));
    case "ObjectExpression": {
      const out = {};
      for (const p of node.properties) {
        if (p.type !== "Property" || p.computed || p.method || p.shorthand || p.kind !== "init") {
          bad(p.type === "Property" && p.shorthand ? p.value : p);
          continue;
        }
        const key = p.key.type === "Identifier" ? p.key.name : String(p.key.value);
        out[key] = literal(p.value, src, path ? `${path}.${key}` : key, errors);
      }
      return out;
    }
    default:
      bad(node);
      return undefined;
  }
}

/** Returns { manifest, errors } where errors are sentences for the Builder's agent. */
export function evaluateManifest(root) {
  const file = join(root, "manifest.ts");
  let code;
  try {
    code = transformSync(readFileSync(file, "utf8"), { loader: "ts", format: "esm", target: "es2022" }).code;
  } catch (e) {
    throw new VmError(`manifest.ts does not parse: ${e.message.split("\n")[0]}`);
  }
  const ast = parse(code, { ecmaVersion: "latest", sourceType: "module" });
  const consts = new Map();
  let exported;
  for (const s of ast.body) {
    if (s.type === "VariableDeclaration") {
      for (const d of s.declarations) if (d.id.type === "Identifier" && d.init) consts.set(d.id.name, d.init);
    }
    if (s.type === "ExportDefaultDeclaration") exported = s.declaration;
    if (s.type === "ExportNamedDeclaration" && !s.source) {
      for (const sp of s.specifiers) if (sp.exported.name === "default") exported = sp.local;
    }
  }
  if (!exported) return { manifest: null, errors: ["manifest.ts has no default export. It must end with: export default manifest;"] };
  for (let i = 0; i < 5 && exported.type === "Identifier" && consts.has(exported.name); i++) exported = consts.get(exported.name);
  const isCall =
    exported.type === "CallExpression" && exported.callee.type === "Identifier" && exported.callee.name === "defineManifest";
  if (!isCall || exported.arguments.length !== 1) {
    return { manifest: null, errors: ["The default export of manifest.ts must be defineManifest({ ... }) with the object written in place."] };
  }
  const arg = exported.arguments[0];
  const errors = [];
  if (arg.type !== "ObjectExpression") {
    errors.push(`defineManifest must be given the object itself, not ${describe(arg)}.`);
    return { manifest: null, errors };
  }
  const m = literal(arg, code, "", errors);
  if (errors.length) return { manifest: null, errors: ["manifest.ts must be a plain literal.", ...errors] };
  return { manifest: m, errors: validate(m) };
}

function validate(m) {
  const errors = [];
  if (typeof m.name !== "string" || !NAME_RE.test(m.name)) {
    errors.push(`name must be 3 to 40 lowercase letters, digits or dashes, starting with a letter (it becomes the address). Found: ${JSON.stringify(m.name)}`);
  }
  if (typeof m.description !== "string" || m.description.trim().length < 10) {
    errors.push("description must be one sentence saying what the Application does and for whom.");
  }
  if (m.resources === undefined || typeof m.resources !== "object" || Array.isArray(m.resources) || m.resources === null) {
    errors.push("resources must be an object: { <resource name>: { kind: \"postgres\", access: \"read\" | \"write\" } }.");
    return errors;
  }
  for (const [name, d] of Object.entries(m.resources)) {
    if (!d || typeof d !== "object") {
      errors.push(`resources.${name} must be { kind: "postgres", access: "read" | "write" }.`);
      continue;
    }
    if (d.kind !== "postgres") errors.push(`resources.${name}.kind must be "postgres".`);
    if (d.access === "structure") {
      errors.push(`resources.${name}.access cannot be "structure": that level is for building only. Declare "read" or "write".`);
    } else if (d.access !== "read" && d.access !== "write") {
      errors.push(`resources.${name}.access must be "read" or "write".`);
    }
    const extra = Object.keys(d).filter((k) => k !== "kind" && k !== "access");
    if (extra.length) errors.push(`resources.${name} has unknown fields: ${extra.join(", ")}.`);
  }
  const extra = Object.keys(m).filter((k) => !["name", "description", "resources"].includes(k));
  if (extra.length) errors.push(`manifest has unknown fields: ${extra.join(", ")}.`);
  return errors;
}

export function requireManifest(root) {
  const { manifest, errors } = evaluateManifest(root);
  if (errors.length) throw new VmError(["manifest.ts is not valid:", ...errors.map((e) => `  - ${e}`)].join("\n"), { code: "manifest" });
  return manifest;
}
