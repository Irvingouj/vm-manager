//! The SDK's `manifest` command (CONTRACT.md, "The SDK: The Manifest" and
//! "The tooling"): `node sdk/tooling/main.js manifest <project directory>`
//! reads `manifest.ts` without running it or anything it imports.
//!
//! - A plain literal gives `{ name, description, declared_access }` on
//!   standard output, exit code 0, and that `declared_access` is accepted by
//!   the platform's API unchanged.
//! - Each kind of expression that is not a plain literal (an identifier, a
//!   call, a spread, a computed key, a shorthand property, a template with
//!   a substitution, an imported value) is `manifest-not-literal`, exit code
//!   2, with the file, and the line and column of the offending expression.
//! - A plain literal of the wrong shape is `manifest-invalid`, naming the
//!   field; no file, no call and two calls are `manifest-missing`.
//! - Nothing is run: a `manifest.ts` whose top level, and whose import,
//!   write a file and throw still yields its Manifest, and no file is
//!   written.
//!
//! The projects are written by each test into a directory of its own under
//! `target/` ([`TempProject`]); a marker comment `/*@*/` in a case's text
//! stands just before the offending expression, and is removed before the
//! file is written (`/*@alt*/` marks a second place the contract's words
//! also allow, for a spread and a computed key, where "the offending
//! expression" may be read as the element or as what it holds).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::api::SOME_GIT_COMMIT;
use blackbox::sdk::{TempProject, Tooled, built, fixture_project, run_tooling};
use blackbox::vm::{INSTALLER, Vm};
use serde_json::{Value, json};

const HEAD: &str = "import { defineManifest } from \"@vm/sdk\";\n";

/// A position in a file: line and column, from 1.
type Position = (u64, u64);

/// Removes every `marker` from `text`, returning the text without them and
/// where each one stood (line and column from 1), in order.
fn take_markers(text: &str, marker: &str) -> (String, Vec<Position>) {
    let mut text = text.to_owned();
    let mut found = Vec::new();
    while let Some(at) = text.find(marker) {
        let before = text.get(..at).unwrap();
        let line = u64::try_from(before.matches('\n').count()).unwrap() + 1;
        let line_start = before.rfind('\n').map_or(0, |n| n + 1);
        let column = u64::try_from(before.get(line_start..).unwrap().chars().count()).unwrap() + 1;
        found.push((line, column));
        text.replace_range(at..at + marker.len(), "");
    }
    (text, found)
}

/// The text of a case with its markers taken out, and the positions the
/// error may point at: each `/*@*/`, then each `/*@alt*/`.
fn case(text: &str) -> (String, Vec<Position>) {
    // Take the alternatives out first, then find the primary marker in the
    // text without them, so both positions are positions in the final text.
    let (without_alt, _) = take_markers(text, "/*@alt*/");
    let (final_text, primary) = take_markers(&without_alt, "/*@*/");
    let (without_primary, _) = take_markers(text, "/*@*/");
    let (_, alternatives) = take_markers(&without_primary, "/*@alt*/");
    let mut positions = primary;
    positions.extend(alternatives);
    (final_text, positions)
}

/// Writes `manifest.ts` (and the other files given) into a fresh project
/// and runs `manifest` on it.
fn run_manifest(manifest: &str, others: &[(&str, &str)]) -> (TempProject, Tooled) {
    let project = TempProject::new("manifest");
    project.write("manifest.ts", manifest);
    for (path, text) in others {
        project.write(path, text);
    }
    let run = project.manifest();
    (project, run)
}

/// Fails unless the run exited 2 with exactly `{ error: { reason, message,
/// file, line, column } }` on standard output, this reason, a message, and
/// (when `at` is not empty) the file `manifest.ts` and one of these
/// positions. Returns the error.
fn assert_manifest_error(run: &Tooled, reason: &str, at: &[Position], context: &str) -> Value {
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
        vec!["column", "file", "line", "message", "reason"],
        "{context}: expected {{ error: {{ reason, message, file, line, column }} }}, got {answer}"
    );
    assert_eq!(
        error["reason"], reason,
        "{context}: expected the reason {reason:?}, got {answer}"
    );
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|m| m.trim().len() >= 10),
        "{context}: expected a message a Coding agent can act on, got {answer}"
    );
    if !at.is_empty() {
        assert!(
            error["file"]
                .as_str()
                .is_some_and(|f| f.ends_with("manifest.ts")),
            "{context}: expected `file` to name manifest.ts, got {answer}"
        );
        let got = (error["line"].as_u64(), error["column"].as_u64());
        assert!(
            at.iter().any(|(l, c)| got == (Some(*l), Some(*c))),
            "{context}: expected line and column (from 1) at the offending expression, one of {at:?}, got {got:?} in {answer}"
        );
    }
    error.clone()
}

/// A manifest whose resources are `resources`, its name `board`.
fn manifest_with(resources: &str) -> String {
    format!(
        "{HEAD}export default defineManifest({{\n  name: \"board\",\n  description: \"Open incidents.\",\n  resources: {resources},\n}});\n"
    )
}

#[test]
fn a_plain_literal_gives_the_manifest_as_json() {
    let (_project, run) = run_manifest(
        &format!(
            "{HEAD}// Other code in the file is allowed: it is never run.\nconst unused = Math.random();\nexport function helper() {{ return unused; }}\n\nexport default defineManifest({{\n  \"name\": 'quoted-keys',\n  description: \"Single and double quotes, quoted keys, and as const.\",\n  resources: {{\n    \"hr\": {{ kind: \"postgres\", access: \"write\" }},\n    devops: {{ \"kind\": \"postgres\", \"access\": \"read\" }} as const,\n  }},\n}} as const);\n"
        ),
        &[],
    );
    assert_eq!(
        run.code,
        Some(0),
        "expected exit code 0\n{}",
        run.describe()
    );
    assert_eq!(
        run.json(),
        Some(json!({
            "name": "quoted-keys",
            "description": "Single and double quotes, quoted keys, and as const.",
            "declared_access": { "resources": {
                "hr": { "kind": "postgres", "access": "write" },
                "devops": { "kind": "postgres", "access": "read" },
            } },
        })),
        "expected exactly the Manifest as JSON on standard output\n{}",
        run.describe()
    );
}

#[test]
fn the_fixture_projects_manifests_are_read() {
    let board = run_tooling("manifest", &fixture_project("board"));
    assert_eq!(board.code, Some(0), "{}", board.describe());
    assert_eq!(
        board.json(),
        Some(json!({
            "name": "sdk-board",
            "description": "Incidents, services and notes, read and written through the SDK.",
            "declared_access": { "resources": {
                "devops": { "kind": "postgres", "access": "read" },
                "ops": { "kind": "postgres", "access": "write" },
            } },
        })),
        "{}",
        board.describe()
    );
    let leftover = run_tooling("manifest", &fixture_project("leftover"));
    assert_eq!(leftover.code, Some(0), "{}", leftover.describe());
    assert_eq!(
        leftover.json().map(|m| m["declared_access"].clone()),
        Some(json!({ "resources": {} })),
        "an empty `resources` is an empty Declared access\n{}",
        leftover.describe()
    );
}

#[test]
fn the_declared_access_the_manifest_command_prints_is_accepted_by_the_api_unchanged() {
    let printed = run_tooling("manifest", &fixture_project("board"));
    assert_eq!(printed.code, Some(0), "{}", printed.describe());
    let manifest = printed.json().expect("the manifest as JSON");
    let declared_access = manifest["declared_access"].clone();
    let vm = Vm::with_stand_in(&[]);
    let api = vm.api_as(&User::new(INSTALLER));
    api.create_application(manifest["name"].as_str().unwrap())
        .expect_status(201);
    let sent = api.send_version(
        manifest["name"].as_str().unwrap(),
        &json!({
            "bundle": "globalThis.default = { fetch() { return new Response('not the SDK'); } };",
            "declared_access": declared_access,
            "git_commit": SOME_GIT_COMMIT,
        }),
    );
    assert_eq!(
        sent.status,
        201,
        "expected the API to take the declared_access the `manifest` command printed\n{}",
        sent.describe()
    );
    assert_eq!(
        sent.json()["declared_access"],
        declared_access,
        "expected the Version to record the declared_access unchanged"
    );
}

#[test]
fn an_identifier_is_not_a_plain_literal() {
    let (text, at) = case(&format!(
        "{HEAD}const level = \"read\";\nexport default defineManifest({{\n  name: \"board\",\n  description: \"Open incidents.\",\n  resources: {{ devops: {{ kind: \"postgres\", access: /*@*/level }} }},\n}});\n"
    ));
    let (_p, run) = run_manifest(&text, &[]);
    let error = assert_manifest_error(&run, "manifest-not-literal", &at, "an identifier");
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("plain literal"),
        "expected the message to say the Manifest must be a plain literal, got {message:?}"
    );
}

#[test]
fn a_call_is_not_a_plain_literal() {
    let (text, at) = case(&format!(
        "{HEAD}function pick(level: \"read\" | \"write\") {{ return level; }}\nexport default defineManifest({{\n  name: \"board\",\n  description: \"Open incidents.\",\n  resources: {{ devops: {{ kind: \"postgres\", access: /*@*/pick(\"read\") }} }},\n}});\n"
    ));
    let (_p, run) = run_manifest(&text, &[]);
    assert_manifest_error(&run, "manifest-not-literal", &at, "a call");
}

#[test]
fn a_spread_is_not_a_plain_literal() {
    let (text, at) = case(&format!(
        "{HEAD}const shared = {{ devops: {{ kind: \"postgres\", access: \"read\" }} }} as const;\nexport default defineManifest({{\n  name: \"board\",\n  description: \"Open incidents.\",\n  resources: {{ /*@*/.../*@alt*/shared }},\n}});\n"
    ));
    let (_p, run) = run_manifest(&text, &[]);
    assert_manifest_error(&run, "manifest-not-literal", &at, "a spread");
}

#[test]
fn a_computed_key_is_not_a_plain_literal() {
    let (text, at) = case(&format!(
        "{HEAD}export default defineManifest({{\n  name: \"board\",\n  description: \"Open incidents.\",\n  resources: {{ /*@*/[/*@alt*/\"devops\"]: {{ kind: \"postgres\", access: \"read\" }} }},\n}});\n"
    ));
    let (_p, run) = run_manifest(&text, &[]);
    assert_manifest_error(&run, "manifest-not-literal", &at, "a computed key");
}

#[test]
fn a_shorthand_property_is_not_a_plain_literal() {
    let (text, at) = case(&format!(
        "{HEAD}const name = \"board\";\nexport default defineManifest({{\n  /*@*/name,\n  description: \"Open incidents.\",\n  resources: {{}},\n}});\n"
    ));
    let (_p, run) = run_manifest(&text, &[]);
    assert_manifest_error(&run, "manifest-not-literal", &at, "a shorthand property");
}

#[test]
fn a_template_with_a_substitution_is_not_a_plain_literal() {
    let (text, at) = case(&format!(
        "{HEAD}const team = \"on call\";\nexport default defineManifest({{\n  name: \"board\",\n  description: /*@*/`For the people ${{team}}.`,\n  resources: {{}},\n}});\n"
    ));
    let (_p, run) = run_manifest(&text, &[]);
    assert_manifest_error(
        &run,
        "manifest-not-literal",
        &at,
        "a template with a substitution",
    );
}

#[test]
fn an_imported_value_is_not_a_plain_literal() {
    let (text, at) = case(&format!(
        "{HEAD}import {{ level }} from \"./levels.ts\";\nexport default defineManifest({{\n  name: \"board\",\n  description: \"Open incidents.\",\n  resources: {{ devops: {{ kind: \"postgres\", access: /*@*/level }} }},\n}});\n"
    ));
    let (_p, run) = run_manifest(
        &text,
        &[("levels.ts", "export const level = \"read\" as const;\n")],
    );
    assert_manifest_error(&run, "manifest-not-literal", &at, "an imported value");
}

#[test]
fn an_imported_manifest_is_not_a_plain_literal() {
    let (text, at) = case(&format!(
        "{HEAD}import shared from \"./shared.ts\";\nexport default defineManifest(/*@*/shared);\n"
    ));
    let (_p, run) = run_manifest(
        &text,
        &[(
            "shared.ts",
            "export default { name: \"board\", description: \"d\", resources: {} } as const;\n",
        )],
    );
    assert_manifest_error(&run, "manifest-not-literal", &at, "an imported Manifest");
}

#[test]
fn structure_is_not_a_level_an_application_declares() {
    let (text, at) = case(&manifest_with(
        "{ devops: { kind: \"postgres\", access: /*@*/\"structure\" } }",
    ));
    let (_p, run) = run_manifest(&text, &[]);
    let error = assert_manifest_error(&run, "manifest-invalid", &at, "access: \"structure\"");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("access"),
        "expected the message to name the field `access`, got {error}"
    );
}

#[test]
fn an_unknown_kind_is_invalid() {
    let (text, at) = case(&manifest_with(
        "{ devops: { kind: /*@*/\"mysql\", access: \"read\" } }",
    ));
    let (_p, run) = run_manifest(&text, &[]);
    let error = assert_manifest_error(&run, "manifest-invalid", &at, "kind: \"mysql\"");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("kind"),
        "expected the message to name the field `kind`, got {error}"
    );
}

#[test]
fn a_missing_name_is_invalid() {
    let (text, at) = case(&format!(
        "{HEAD}export default /*@alt*/defineManifest(/*@*/{{\n  description: \"Open incidents.\",\n  resources: {{}},\n}});\n"
    ));
    let (_p, run) = run_manifest(&text, &[]);
    let error = assert_manifest_error(&run, "manifest-invalid", &at, "no `name`");
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("name"),
        "expected the message to name the field `name`, got {error}"
    );
}

#[test]
fn a_name_the_api_would_not_take_is_invalid() {
    for bad in ["Incident Board", "-board", "api"] {
        let (text, at) = case(&format!(
            "{HEAD}export default defineManifest({{\n  name: /*@*/\"{bad}\",\n  description: \"Open incidents.\",\n  resources: {{}},\n}});\n"
        ));
        let (_p, run) = run_manifest(&text, &[]);
        let error =
            assert_manifest_error(&run, "manifest-invalid", &at, &format!("the name {bad:?}"));
        assert!(
            error["message"]
                .as_str()
                .unwrap_or_default()
                .contains("name"),
            "expected the message to name the field `name`, got {error}"
        );
    }
}

#[test]
fn a_project_without_manifest_ts_is_manifest_missing() {
    let project = TempProject::new("no-manifest");
    project.write("src/index.ts", "export {};\n");
    let run = project.manifest();
    assert_manifest_error(&run, "manifest-missing", &[], "no manifest.ts");
}

#[test]
fn a_manifest_ts_without_define_manifest_is_manifest_missing() {
    let (_p, run) = run_manifest(
        "export default { name: \"board\", description: \"d\", resources: {} };\n",
        &[],
    );
    assert_manifest_error(&run, "manifest-missing", &[], "no defineManifest(...) call");
}

#[test]
fn two_define_manifest_calls_are_manifest_missing() {
    let (_p, run) = run_manifest(
        &format!(
            "{HEAD}export const other = defineManifest({{ name: \"other\", description: \"d\", resources: {{}} }});\nexport default defineManifest({{ name: \"board\", description: \"d\", resources: {{}} }});\n"
        ),
        &[],
    );
    assert_manifest_error(
        &run,
        "manifest-missing",
        &[],
        "two defineManifest(...) calls",
    );
}

#[test]
fn reading_the_manifest_runs_neither_it_nor_what_it_imports() {
    let project = TempProject::new("not-run");
    let ran_manifest = project.path("ran-manifest.txt");
    let ran_import = project.path("ran-import.txt");
    let side = format!(
        "import {{ writeFileSync }} from \"node:fs\";\nwriteFileSync({:?}, \"an import of manifest.ts ran\");\nthrow new Error(\"an import of manifest.ts was run\");\n",
        ran_import.to_string_lossy()
    );
    let manifest = format!(
        "{HEAD}import {{ writeFileSync }} from \"node:fs\";\nimport \"./side-effect.ts\";\nwriteFileSync({:?}, \"manifest.ts ran\");\nthrow new Error(\"manifest.ts was run\");\nexport default defineManifest({{\n  name: \"not-run\",\n  description: \"Never run.\",\n  resources: {{ devops: {{ kind: \"postgres\", access: \"read\" }} }},\n}});\n",
        ran_manifest.to_string_lossy()
    );
    project.write("side-effect.ts", &side);
    project.write("manifest.ts", &manifest);
    let run = project.manifest();
    assert_eq!(
        run.code,
        Some(0),
        "expected the Manifest read without running the file\n{}",
        run.describe()
    );
    assert_eq!(
        run.json(),
        Some(json!({
            "name": "not-run",
            "description": "Never run.",
            "declared_access": { "resources": { "devops": { "kind": "postgres", "access": "read" } } },
        })),
        "{}",
        run.describe()
    );
    assert!(
        !ran_manifest.exists(),
        "manifest.ts was run: it wrote {}",
        ran_manifest.display()
    );
    assert!(
        !ran_import.exists(),
        "an import of manifest.ts was run: it wrote {}",
        ran_import.display()
    );
}

#[test]
fn the_board_fixture_builds() {
    // The other tests' fixture, built as they build it: a failure here says
    // the tooling cannot build it, before any program is started.
    let board = built("board");
    assert_eq!(board.name, "sdk-board");
    assert_eq!(board.migrations.len(), 1);
}
