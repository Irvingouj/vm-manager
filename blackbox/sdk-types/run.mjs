// The type tests of @vm/sdk (blackbox/CONTRACT.md, "The types"). A test
// below the black-box seam that the spec names: the compiler's words are
// what a Coding agent reads, so they are asserted.
//
// Each case is a file under projects/<project>/cases/, compiled with the
// TypeScript compiler API together with its project (manifest.ts, src/,
// resources/, as tsconfig.json includes them). A case says what it expects
// with comment lines just above the line where the error must be:
//
//   // @expect TS2345 is not assignable to parameter of type '"devops" | "hr"'
//   ctx.user().db("payroll");
//
// `TS<code>` is the error code, `TS*` any code; the rest of the line is a
// substring the message must contain (the whole chain of the message,
// flattened), empty for any message. A case passes when every expectation
// is met by an error of that code, holding that text, at that line, and no
// other error is reported anywhere in the program. So it fails when an
// expected error is missing, when the code or the wording differs, and when
// an unexpected error appears. A case with no expectation must compile with
// no diagnostics at all.
//
// Also compiled, each with no diagnostics allowed: every fixture project
// under blackbox/fixtures/sdk/ (Applications the black-box tests run).
//
// Before all that, the runner checks itself on selftest/cases/, plain
// TypeScript without the SDK: each case's first line says whether the
// runner must pass it (`// @selftest pass`) or fail it (`// @selftest fail`).
//
// node run.mjs [<substring of a case's path>...]   runs every case, or those matching

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const here = path.dirname(fileURLToPath(import.meta.url));
const fixtures = path.resolve(here, "..", "fixtures", "sdk");
const filters = process.argv.slice(2);

const EXPECT = /^\s*\/\/\s*@expect\s+(TS\d+|TS\*)(?:\s+(.*))?$/;
const SELFTEST = /^\s*\/\/\s*@selftest\s+(pass|fail)\s*$/;

/** The expectations a case file states, each with the line it is about (1-based). */
export function expectationsOf(text, file) {
  const lines = text.split(/\r?\n/);
  const expectations = [];
  let pending = [];
  lines.forEach((line, index) => {
    const found = EXPECT.exec(line);
    if (found) {
      const code = found[1] === "TS*" ? "*" : Number(found[1].slice(2));
      pending.push({ code, text: (found[2] ?? "").trim(), written: index + 1 });
      return;
    }
    const trimmed = line.trim();
    if (pending.length > 0 && trimmed !== "" && !trimmed.startsWith("//")) {
      for (const p of pending) expectations.push({ ...p, line: index + 1 });
      pending = [];
    }
  });
  if (pending.length > 0) {
    throw new Error(`${file}:${pending[0].written}: an @expect with no line of code after it`);
  }
  return expectations;
}

function readProject(dir) {
  const configPath = path.join(dir, "tsconfig.json");
  const read = ts.readConfigFile(configPath, ts.sys.readFile);
  if (read.error) {
    throw new Error(`${configPath}: ${ts.flattenDiagnosticMessageText(read.error.messageText, "\n")}`);
  }
  const parsed = ts.parseJsonConfigFileContent(read.config, ts.sys, dir, undefined, configPath);
  const problems = parsed.errors.filter((d) => d.code !== 18003); // "no inputs": a project of cases only
  if (problems.length > 0) {
    throw new Error(`${configPath}: ${problems.map((d) => ts.flattenDiagnosticMessageText(d.messageText, "\n")).join("; ")}`);
  }
  return parsed;
}

function described(diagnostic) {
  const message = ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n");
  if (!diagnostic.file || diagnostic.start === undefined) {
    return { file: null, line: 0, code: diagnostic.code, message };
  }
  const { line } = diagnostic.file.getLineAndCharacterOfPosition(diagnostic.start);
  return { file: path.resolve(diagnostic.file.fileName), line: line + 1, code: diagnostic.code, message };
}

function show(d) {
  const where = d.file ? `${path.relative(process.cwd(), d.file)}:${d.line}` : "(no file)";
  return `${where}: TS${d.code} ${JSON.stringify(d.message)}`;
}

/**
 * Compares what the compiler said with what the case expects. Returns the
 * problems, empty when the case passes. `caseFile` is null for a project
 * that must compile with no diagnostics.
 */
export function compare(diagnostics, caseFile, expectations) {
  const problems = [];
  const inCase = [];
  for (const d of diagnostics) {
    if (caseFile !== null && d.file === caseFile) inCase.push({ ...d, used: false });
    else problems.push(`unexpected error: ${show(d)}`);
  }
  for (const e of expectations) {
    const wanted = `TS${e.code === "*" ? "*" : e.code}${e.text ? ` containing ${JSON.stringify(e.text)}` : ""}`;
    const met = inCase.find(
      (d) => !d.used && d.line === e.line && (e.code === "*" || d.code === e.code) && d.message.includes(e.text),
    );
    if (met) {
      met.used = true;
      continue;
    }
    const near = inCase.filter((d) => !d.used && d.line === e.line);
    if (near.length > 0) {
      for (const d of near) d.used = true;
      problems.push(
        `line ${e.line}: expected ${wanted}, but the error there differs: ${near.map(show).join(" | ")}`,
      );
    } else {
      problems.push(`line ${e.line}: expected ${wanted}, but there is no error on that line`);
    }
  }
  for (const d of inCase) if (!d.used) problems.push(`unexpected error: ${show(d)}`);
  return problems;
}

function compile(rootNames, options, oldProgram) {
  const program = ts.createProgram({ rootNames, options, oldProgram });
  const diagnostics = ts.getPreEmitDiagnostics(program).map(described);
  return { program, diagnostics };
}

function casesIn(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => name.endsWith(".ts"))
    .sort()
    .map((name) => path.join(dir, name));
}

function wanted(file) {
  return filters.length === 0 || filters.some((f) => file.includes(f));
}

let failures = 0;
let passes = 0;

function report(name, problems) {
  if (problems.length === 0) {
    passes += 1;
    console.log(`ok    ${name}`);
  } else {
    failures += 1;
    console.log(`FAIL  ${name}`);
    for (const p of problems) console.log(`      ${p}`);
  }
}

/** Runs every case of the project in `dir`. With `selftest`, checks the runner instead. */
function runProject(dir, { selftest = false } = {}) {
  const project = readProject(dir);
  let old;
  for (const caseFile of casesIn(path.join(dir, "cases"))) {
    const name = path.relative(here, caseFile);
    if (!wanted(caseFile)) continue;
    const text = readFileSync(caseFile, "utf8");
    let expectations;
    try {
      expectations = expectationsOf(text, name);
    } catch (e) {
      report(name, [String(e.message)]);
      continue;
    }
    const rootNames = [...project.fileNames.filter((f) => path.resolve(f) !== caseFile), caseFile];
    const { program, diagnostics } = compile(rootNames, project.options, old);
    old = program;
    const problems = compare(diagnostics, path.resolve(caseFile), expectations);
    if (selftest) {
      const outcome = SELFTEST.exec(text.split(/\r?\n/)[0] ?? "");
      if (!outcome) {
        report(`selftest ${name}`, ["the first line must be // @selftest pass or // @selftest fail"]);
        continue;
      }
      const shouldPass = outcome[1] === "pass";
      const passed = problems.length === 0;
      report(
        `selftest ${name} (the runner must ${outcome[1]} it)`,
        passed === shouldPass
          ? []
          : [
              `the runner ${passed ? "passed" : "failed"} it${passed ? "" : ":"}`,
              ...problems.map((p) => `  ${p}`),
            ],
      );
    } else {
      report(name, problems);
    }
  }
}

/** Compiles the project in `dir` as it is, requiring no diagnostics. */
function runWholeProject(dir) {
  const name = path.relative(here, dir);
  if (!wanted(dir)) return;
  const project = readProject(dir);
  const { diagnostics } = compile(project.fileNames, project.options);
  report(`${name} (compiles with no diagnostics)`, compare(diagnostics, null, []));
}

function directoriesIn(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .map((name) => path.join(dir, name))
    .filter((p) => statSync(p).isDirectory() && existsSync(path.join(p, "tsconfig.json")))
    .sort();
}

console.log(`TypeScript ${ts.version}`);
runProject(path.join(here, "selftest"), { selftest: true });
for (const dir of directoriesIn(path.join(here, "projects"))) runProject(dir);
for (const dir of directoriesIn(fixtures)) runWholeProject(dir);

console.log(`\n${passes} passed, ${failures} failed`);
process.exitCode = failures === 0 ? 0 : 1;
