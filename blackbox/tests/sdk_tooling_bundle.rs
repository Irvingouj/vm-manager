//! The SDK's `bundle` command (CONTRACT.md, "The SDK: The tooling" and
//! "What an Application is handed"): `node sdk/tooling/main.js bundle
//! <project directory>` prints one classic script, with no `import` and no
//! `export`, that runs on the program; a project that does not build exits
//! 2 with `{ error: { reason: "build-failed", message, errors: [ { file,
//! line, message } ] } }`. `bundle` does not type-check.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::sdk::{Sdk, TSCONFIG, TempProject, Tooled, ann, built, node};
use serde_json::{Value, json};

/// Parses `path` as a classic script (`vm.Script`, as a browser's `<script>`
/// or the program would), and prints `classic` or why not.
const PARSE_AS_SCRIPT: &str = "const fs = require('node:fs'); const vm = require('node:vm'); \
     const code = fs.readFileSync(process.argv[1], 'utf8'); \
     try { new vm.Script(code, { filename: 'bundle.js' }); console.log('classic'); } \
     catch (e) { console.log('not a classic script: ' + e.message); process.exitCode = 3; }";

#[test]
fn the_bundle_is_a_classic_script_with_no_import_or_export() {
    let board = built("board");
    let project = TempProject::new("parse-bundle");
    project.write("bundle.js", &board.bundle);
    let path = project.path("bundle.js");
    let parsed = node(&["-e", PARSE_AS_SCRIPT, &path.to_string_lossy()]);
    assert_eq!(
        (parsed.code, parsed.stdout.trim()),
        (Some(0), "classic"),
        "expected the bundle to parse as a classic script (an `import` or `export` statement does not)\n{}",
        parsed.describe()
    );
    let statements: Vec<&str> = board
        .bundle
        .lines()
        .map(str::trim_start)
        .filter(|l| {
            l.starts_with("import ")
                || l.starts_with("import{")
                || l.starts_with("export ")
                || l.starts_with("export{")
        })
        .collect();
    assert!(
        statements.is_empty(),
        "expected no line of the bundle to begin an import or export statement, found {statements:?}"
    );
    assert!(
        board.bundle.contains("globalThis"),
        "expected the bundle to assign globalThis.default"
    );
}

#[test]
fn the_bundle_runs_on_the_program() {
    let sdk = Sdk::only("leftover");
    let (_, body) = sdk.get_ok(&ann(), "/now");
    assert_eq!(
        body,
        json!({ "asUser": { "ran": true, "rows": 1 }, "asApplication": { "ran": true, "rows": 1 } }),
        "expected the bundled Application to answer, having read its own database as the User and as the Application"
    );
}

/// A small project: the Manifest, the tsconfig, an own schema, and this
/// `src/index.ts`.
fn project_with_index(index: &str) -> TempProject {
    let project = TempProject::new("bundle");
    project
        .write(
            "manifest.ts",
            "import { defineManifest } from \"@vm/sdk\";\nexport default defineManifest({\n  name: \"small\",\n  description: \"A small project.\",\n  resources: {},\n});\n",
        )
        .write("tsconfig.json", TSCONFIG)
        .write(
            "src/db/schema.ts",
            "import { pgTable, serial, text } from \"drizzle-orm/pg-core\";\nexport const notes = pgTable(\"notes\", { id: serial(\"id\").primaryKey(), body: text(\"body\").notNull() });\n",
        )
        .write("src/index.ts", index);
    project
}

/// Fails unless the run exited 2 with exactly `{ error: { reason:
/// "build-failed", message, errors } }`, an error naming `src/index.ts`
/// at `line`. Returns the error.
fn assert_build_failed(run: &Tooled, line: u64, context: &str) -> Value {
    assert_eq!(
        run.code,
        Some(2),
        "{context}: expected exit code 2\n{}",
        run.describe()
    );
    let answer = run.json().unwrap_or_else(|| {
        panic!(
            "{context}: expected one JSON value on standard output\n{}",
            run.describe()
        )
    });
    let error = &answer["error"];
    let mut keys: Vec<&str> = error
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["errors", "message", "reason"],
        "{context}: expected {{ error: {{ reason, message, errors }} }}, got {answer}"
    );
    assert_eq!(error["reason"], "build-failed", "{context}: {answer}");
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|m| !m.trim().is_empty()),
        "{context}: expected a message, got {answer}"
    );
    let errors = error["errors"].as_array().cloned().unwrap_or_default();
    assert!(
        !errors.is_empty(),
        "{context}: expected at least one error, got {answer}"
    );
    for e in &errors {
        assert!(
            e["file"].is_string() && e["line"].is_u64() && e["message"].is_string(),
            "{context}: expected each error as {{ file, line, message }}, got {e}"
        );
    }
    assert!(
        errors.iter().any(|e| {
            e["file"]
                .as_str()
                .is_some_and(|f| f.ends_with("src/index.ts"))
                && e["line"].as_u64() == Some(line)
        }),
        "{context}: expected an error at src/index.ts line {line}, got {answer}"
    );
    answer
}

#[test]
fn a_project_with_a_syntax_error_does_not_build() {
    let project = project_with_index(
        "import { defineApp } from \"@vm/sdk\";\nimport manifest from \"../manifest.ts\";\n\nexport default defineApp(manifest, {\n  async fetch(request, ctx) {\n    const broken = (1 + ;\n    return new Response(String(broken));\n  },\n});\n",
    );
    let run = project.bundle();
    assert_build_failed(&run, 6, "a syntax error");
}

#[test]
fn a_project_importing_a_file_that_is_not_there_does_not_build() {
    let project = project_with_index(
        "import { defineApp } from \"@vm/sdk\";\nimport manifest from \"../manifest.ts\";\nimport { helper } from \"./not-there.ts\";\n\nexport default defineApp(manifest, {\n  async fetch() {\n    return new Response(helper());\n  },\n});\n",
    );
    let run = project.bundle();
    assert_build_failed(&run, 3, "an import of a missing file");
}

#[test]
fn bundle_does_not_type_check() {
    let project = project_with_index(
        "import { defineApp } from \"@vm/sdk\";\nimport manifest from \"../manifest.ts\";\n\nexport default defineApp(manifest, {\n  async fetch() {\n    const n: number = \"not a number\";\n    return new Response(String(n));\n  },\n});\n",
    );
    let run = project.bundle();
    assert_eq!(
        run.code,
        Some(0),
        "expected a project with a type error (but valid syntax) to bundle\n{}",
        run.describe()
    );
    assert!(
        !run.stdout.trim().is_empty(),
        "expected the bundle on standard output\n{}",
        run.describe()
    );
}
