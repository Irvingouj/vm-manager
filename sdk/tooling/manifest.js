// `manifest`: reads a project's manifest.ts without running it or anything
// it imports (blackbox/CONTRACT.md, "The SDK: The Manifest" and "The
// tooling"). The file is parsed with the TypeScript compiler and the one
// `defineManifest(...)` call's argument is read as a plain literal: object
// literals with plain or quoted keys, strings, numbers, booleans, null,
// arrays, and `as const`. Anything else is refused, with where it is.

import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import ts from "typescript";

/** The platform's own names, which no Application may take (the API's `reserved-name`). */
const RESERVED = new Set(["api", "auth", "dashboard", "git", "admin", "www"]);

const PLAIN =
  "The Manifest must be a plain literal (objects with plain or quoted keys, strings, numbers, booleans, null, arrays, as const), because the platform reads manifest.ts without running any code: write the value out in place.";

/** A Manifest that cannot be taken: `reason`, a message, and the node it is about (if any). */
class ManifestError extends Error {
  constructor(reason, message, node = null) {
    super(message);
    this.reason = reason;
    this.node = node;
  }
}

/**
 * Reads `<project>/manifest.ts`. Answers `{ manifest }` (what the command
 * prints) or `{ error }` (`{ reason, message, file, line, column }`).
 */
export function readManifest(project) {
  const file = path.join(project, "manifest.ts");
  if (!existsSync(file)) {
    return failure(file, null, new ManifestError("manifest-missing", `There is no manifest.ts in ${project}. ${HOW}`));
  }
  const text = readFileSync(file, "utf8");
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  try {
    return { manifest: manifestOf(source) };
  } catch (error) {
    if (error instanceof ManifestError) return failure(file, source, error);
    throw error;
  }
}

const HOW =
  'manifest.ts must hold exactly one call: export default defineManifest({ name: "...", description: "...", resources: { ... } }).';

function failure(file, source, error) {
  let line = null;
  let column = null;
  if (source !== null && error.node !== null) {
    const at = source.getLineAndCharacterOfPosition(error.node.getStart(source));
    line = at.line + 1;
    column = at.character + 1;
  }
  return { error: { reason: error.reason, message: error.message, file, line, column } };
}

function manifestOf(source) {
  const calls = [];
  const visit = (node) => {
    if (ts.isCallExpression(node) && calleeName(node.expression) === "defineManifest") calls.push(node);
    ts.forEachChild(node, visit);
  };
  visit(source);
  if (calls.length === 0) {
    throw new ManifestError("manifest-missing", `manifest.ts has no defineManifest(...) call. ${HOW}`);
  }
  if (calls.length > 1) {
    throw new ManifestError(
      "manifest-missing",
      `manifest.ts has ${calls.length} defineManifest(...) calls; it must have exactly one. ${HOW}`,
      calls[1],
    );
  }
  const call = calls[0];
  if (call.arguments.length !== 1) {
    throw new ManifestError(
      "manifest-invalid",
      `defineManifest takes exactly one argument, the Manifest; it was given ${call.arguments.length}.`,
      call,
    );
  }
  const argument = call.arguments[0];
  return validate(literal(argument, source, "the Manifest"), argument);
}

function calleeName(expression) {
  if (ts.isIdentifier(expression)) return expression.text;
  if (ts.isPropertyAccessExpression(expression)) return expression.name.text;
  return null;
}

// ---------------------------------------------------------------- the literal

/** A value read from a plain literal, remembering the node each part was written at. */
class Read {
  constructor(value, node, entries = null) {
    this.value = value;
    this.node = node;
    // For an object: Map of key → Read.
    this.entries = entries;
  }
}

function notLiteral(node, source, what, where) {
  const { line, character } = source.getLineAndCharacterOfPosition(node.getStart(source));
  return new ManifestError(
    "manifest-not-literal",
    `${where} is ${what} (line ${line + 1}, column ${character + 1}: ${snippet(node, source)}). ${PLAIN}`,
    node,
  );
}

function snippet(node, source) {
  const text = node.getText(source).replace(/\s+/g, " ");
  return text.length > 60 ? `${text.slice(0, 57)}...` : text;
}

function literal(node, source, where) {
  if (ts.isAsExpression(node)) {
    if (ts.isTypeReferenceNode(node.type) && ts.isIdentifier(node.type.typeName) && node.type.typeName.text === "const") {
      return literal(node.expression, source, where);
    }
    throw notLiteral(node, source, "an `as` with a type other than `as const`", where);
  }
  if (ts.isParenthesizedExpression(node)) return literal(node.expression, source, where);
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return new Read(node.text, node);
  if (ts.isNumericLiteral(node)) return new Read(Number(node.text), node);
  if (
    ts.isPrefixUnaryExpression(node) &&
    (node.operator === ts.SyntaxKind.MinusToken || node.operator === ts.SyntaxKind.PlusToken) &&
    ts.isNumericLiteral(node.operand)
  ) {
    const n = Number(node.operand.text);
    return new Read(node.operator === ts.SyntaxKind.MinusToken ? -n : n, node);
  }
  switch (node.kind) {
    case ts.SyntaxKind.TrueKeyword:
      return new Read(true, node);
    case ts.SyntaxKind.FalseKeyword:
      return new Read(false, node);
    case ts.SyntaxKind.NullKeyword:
      return new Read(null, node);
    default:
      break;
  }
  if (ts.isArrayLiteralExpression(node)) {
    const items = node.elements.map((element, index) => {
      if (ts.isSpreadElement(element)) throw notLiteral(element, source, "a spread (...)", `${where}[${index}]`);
      if (ts.isOmittedExpression(element)) throw notLiteral(node, source, "an array with a hole", where);
      return literal(element, source, `${where}[${index}]`).value;
    });
    return new Read(items, node);
  }
  if (ts.isObjectLiteralExpression(node)) {
    const entries = new Map();
    const value = {};
    for (const property of node.properties) {
      if (ts.isSpreadAssignment(property)) throw notLiteral(property, source, "a spread (...)", `in ${where}, an entry`);
      if (ts.isShorthandPropertyAssignment(property)) {
        throw notLiteral(
          property,
          source,
          `a shorthand property (\`${property.name.text}\`, a value read from elsewhere)`,
          `${where}.${property.name.text}`,
        );
      }
      if (!ts.isPropertyAssignment(property)) {
        throw notLiteral(property, source, "a method or accessor", `in ${where}, an entry`);
      }
      const name = property.name;
      let key;
      if (ts.isIdentifier(name) || ts.isStringLiteral(name) || ts.isNoSubstitutionTemplateLiteral(name)) {
        key = name.text;
      } else if (ts.isNumericLiteral(name)) {
        key = String(Number(name.text));
      } else if (ts.isComputedPropertyName(name)) {
        throw notLiteral(name, source, "a computed key ([...])", `in ${where}, a key`);
      } else {
        throw notLiteral(name, source, "a key that is not a plain or quoted name", `in ${where}, a key`);
      }
      const inner = `${where === "the Manifest" ? "" : `${where}.`}${key}`;
      if (entries.has(key)) {
        throw new ManifestError("manifest-invalid", `\`${inner}\` is given twice; give each field once.`, name);
      }
      const read = literal(property.initializer, source, `\`${inner}\``);
      entries.set(key, read);
      value[key] = read.value;
    }
    return new Read(value, node, entries);
  }
  throw notLiteral(node, source, describe(node), where);
}

function describe(node) {
  if (ts.isIdentifier(node)) {
    return node.text === "undefined" ? "`undefined`" : `the name \`${node.text}\` (a value defined or imported elsewhere)`;
  }
  if (ts.isCallExpression(node) || ts.isNewExpression(node)) return "a call";
  if (ts.isTemplateExpression(node)) return "a template with a substitution (${...})";
  if (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) return "a value read from another object";
  if (ts.isArrowFunction(node) || ts.isFunctionExpression(node)) return "a function";
  if (ts.isConditionalExpression(node) || ts.isBinaryExpression(node) || ts.isPrefixUnaryExpression(node)) {
    return "a computed expression";
  }
  if (ts.isSatisfiesExpression(node)) return "a `satisfies` expression";
  return `an expression that is not a literal (${ts.SyntaxKind[node.kind]})`;
}

// ---------------------------------------------------------------- the shape

const invalid = (message, node) => new ManifestError("manifest-invalid", message, node);

function validate(read, argument) {
  if (read.entries === null) {
    throw invalid("The Manifest must be an object: { name, description, resources }.", argument);
  }
  const fields = read.entries;
  for (const [key, field] of fields) {
    if (!["name", "description", "resources"].includes(key)) {
      throw invalid(
        `The Manifest has no field \`${key}\`: it has \`name\`, \`description\` and \`resources\`.`,
        field.node,
      );
    }
  }
  const name = fields.get("name");
  if (name === undefined) {
    throw invalid('The Manifest has no `name`: give the Application\'s name, as name: "incident-board".', read.node);
  }
  if (typeof name.value !== "string") throw invalid("`name` must be a string: the Application's name.", name.node);
  const problem = nameProblem(name.value);
  if (problem !== null) throw invalid(`\`name\` ${JSON.stringify(name.value)} ${problem}`, name.node);
  const description = fields.get("description");
  if (description === undefined) {
    throw invalid(
      "The Manifest has no `description`: give one sentence saying what the Application does, and for whom.",
      read.node,
    );
  }
  if (typeof description.value !== "string") {
    throw invalid("`description` must be a string: one sentence saying what the Application does.", description.node);
  }
  const resources = fields.get("resources");
  if (resources === undefined) {
    throw invalid(
      'The Manifest has no `resources`: list the shared databases it uses, as resources: { devops: { kind: "postgres", access: "read" } }, or resources: {} for none.',
      read.node,
    );
  }
  if (resources.entries === null) {
    throw invalid(
      '`resources` must be an object: { <Resource name>: { kind: "postgres", access: "read" | "write" } }.',
      resources.node,
    );
  }
  const declared = {};
  for (const [resource, declaration] of resources.entries) {
    const where = `\`resources.${resource}\``;
    if (resource.length === 0) throw invalid("A Resource name in `resources` is empty.", declaration.node);
    if (declaration.entries === null) {
      throw invalid(`${where} must be { kind: "postgres", access: "read" | "write" }.`, declaration.node);
    }
    for (const [key, field] of declaration.entries) {
      if (key !== "kind" && key !== "access") {
        throw invalid(`${where} has no field \`${key}\`: it has \`kind\` and \`access\`.`, field.node);
      }
    }
    const kind = declaration.entries.get("kind");
    if (kind === undefined) throw invalid(`${where} has no \`kind\`: give kind: "postgres".`, declaration.node);
    if (kind.value !== "postgres") {
      throw invalid(`${where}.kind is ${JSON.stringify(kind.value)}: the only kind is "postgres".`, kind.node);
    }
    const access = declaration.entries.get("access");
    if (access === undefined) {
      throw invalid(`${where} has no \`access\`: give access: "read" or access: "write".`, declaration.node);
    }
    if (access.value === "structure") {
      throw invalid(
        `${where}.access is "structure", which is not something an Application declares (it is for building only): declare "read" or "write".`,
        access.node,
      );
    }
    if (access.value !== "read" && access.value !== "write") {
      throw invalid(`${where}.access is ${JSON.stringify(access.value)}: it must be "read" or "write".`, access.node);
    }
    declared[resource] = { kind: "postgres", access: access.value };
  }
  return { name: name.value, description: description.value, declared_access: { resources: declared } };
}

/** Why the platform's API would not take `name` as an Application's name, or null. */
function nameProblem(name) {
  if (name.length < 1 || name.length > 63) return "must be 1 to 63 characters long.";
  if (!/^[a-z0-9-]+$/.test(name)) return "may hold only lower-case letters, digits and hyphens (a DNS label).";
  if (name.startsWith("-") || name.endsWith("-")) return "must not start or end with a hyphen.";
  if (name.startsWith("xn--")) return 'must not start with "xn--".';
  if (RESERVED.has(name)) return `is one of the platform's own names (${[...RESERVED].join(", ")}): choose another.`;
  return null;
}
