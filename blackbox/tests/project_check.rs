//! What `vm check` finds (CONTRACT.md, "A project: the template, creating
//! one and checking one: Checking a project"; ticket "The project
//! template, and creating and checking a project": the check fails, each
//! with a message naming the fix, for a Manifest that is not a plain
//! literal, a declared Resource that is not registered or not pulled, a
//! schema changed without a migration, and a type error; it warns, and does
//! not fail, when the Builder's own Grant is below a declared level, saying
//! who will be refused and whom to ask; it mentions pulled Resources the
//! Manifest does not declare; it has a JSON form and the tool's exit
//! codes).
//!
//! Each test checks a project of the harness's own (`checked_project`),
//! written under the repository's `target/` so that its packages resolve
//! from the root's `node_modules`, changed in the one way the test is
//! about.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::Path;

use blackbox::project::{
    NOTES_AND_TAGS_SCHEMA_TS, NOTES_RENAMED_SCHEMA_TS, NOTES_SCHEMA_TS, Report, SCHEMA_FIRST_LINE,
    Tree, check, check_in_words, checked_project, generate_migrations, holds_number, manifest_ts,
    pulled_by_hand, sql_files, stack_line_in, type_errors,
};
use blackbox::scenario::{CONTACT, Scenario};
use blackbox::sdk::{TempProject, run_tooling};
use blackbox::tool::Machine;
use blackbox::{User, Vm, VmOptions};
use serde_json::{Value, json};

fn bea() -> User {
    User::new("bea").in_groups(&["builders"])
}

/// A program, and a machine signed in to it as [`bea`].
fn signed_in() -> (Vm, Machine) {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let machine = Machine::new();
    machine.sign_in(&vm, &bea());
    (vm, machine)
}

/// A program with `devops` and `inventory` registered, `builders` granted
/// `read` on both, and a machine signed in as [`bea`].
fn with_resources() -> (Scenario, Machine) {
    let scenario = Scenario::start();
    for name in ["devops", "inventory"] {
        scenario.register_writable(name);
        scenario.grant(name, "builders", "read");
    }
    let machine = Machine::new();
    machine.sign_in(&scenario.vm, &bea());
    (scenario, machine)
}

/// The checked project declaring `devops` at `read`.
fn declaring_devops(label: &str) -> TempProject {
    let project = checked_project(label);
    project.write(
        "manifest.ts",
        &manifest_ts(
            "checked",
            "{ devops: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    project
}

/// A Manifest whose `name` is an identifier, at line 5, column 9.
const NOT_LITERAL: &str = r#"import { defineManifest } from "@vm/sdk";

const n = "checked";
export default defineManifest({
  name: n,
  description: "A project for the tests of vm check.",
  resources: {},
});
"#;

/// A Manifest that declares a Resource at `structure`, at line 5, column
/// 52.
const WRONG_SHAPE: &str = r#"import { defineManifest } from "@vm/sdk";
export default defineManifest({
  name: "checked",
  description: "A project for the tests of vm check.",
  resources: { devops: { kind: "postgres", access: "structure" } },
});
"#;

/// A `manifest.ts` with no `defineManifest` call.
const NO_CALL: &str = "// No Manifest here.\nexport default {};\n";

/// A file with one type error, at line 1, column 14.
const ONE_TYPE_ERROR: &str = "export const wrong: number = \"text\";\n";

/// The compiler's message for [`ONE_TYPE_ERROR`].
const TYPE_ERROR_MESSAGE: &str = "Type 'string' is not assignable to type 'number'.";

#[test]
fn the_checked_project_passes_with_nothing_to_say() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-clean");
    Report::clean(&check(&machine, &project.dir));
}

/// Fails unless `problem` points, as the SDK's tooling does, at the line
/// and column `tooling` gave, in `manifest.ts`.
fn assert_points_as_the_tooling(
    problem: &Value,
    dir: &Path,
    line: Option<u64>,
    column: Option<u64>,
) {
    let (line, column) = (json!(line), json!(column));
    let tooling = run_tooling("manifest", dir).json().unwrap();
    assert_eq!(
        tooling["error"]["reason"], problem["reason"],
        "the harness's own check: the tooling gives the reason {tooling}"
    );
    assert_eq!(tooling["error"]["line"], line, "the harness's own check");
    assert_eq!(
        tooling["error"]["column"], column,
        "the harness's own check"
    );
    assert!(
        problem["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("manifest.ts")),
        "expected `file` the path of manifest.ts, got {problem}"
    );
    assert_eq!(problem["line"], line, "{problem}");
    assert_eq!(problem["column"], column, "{problem}");
}

#[test]
fn a_manifest_that_is_not_a_plain_literal_fails_and_says_why() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-not-literal");
    project.write("manifest.ts", NOT_LITERAL);
    let report = Report::failed(&check(&machine, &project.dir));
    let problem = report.problem("manifest", "manifest-not-literal");
    assert_points_as_the_tooling(&problem, &project.dir, Some(5), Some(9));
    let message = problem["message"].as_str().unwrap();
    let lower = message.to_lowercase();
    assert!(
        lower.contains("plain literal") && lower.contains("without running"),
        "expected the message to say the Manifest must be a plain literal because the platform reads it without running code, got {message:?}"
    );
    assert!(
        message.contains("name"),
        "expected the message to say what was found and where (the field `name`), got {message:?}"
    );
    assert_eq!(report.problems().len(), 1, "{}", report.run.describe());
}

#[test]
fn a_manifest_of_the_wrong_shape_fails_and_names_the_field() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-wrong-shape");
    project.write("manifest.ts", WRONG_SHAPE);
    let report = Report::failed(&check(&machine, &project.dir));
    let problem = report.problem("manifest", "manifest-invalid");
    assert_points_as_the_tooling(&problem, &project.dir, Some(5), Some(52));
    let message = problem["message"].as_str().unwrap();
    assert!(
        message.contains("devops") && message.contains("access"),
        "expected the message to name the field, got {message:?}"
    );
}

#[test]
fn a_manifest_ts_with_no_define_manifest_call_is_manifest_missing() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-no-call");
    project.write("manifest.ts", NO_CALL);
    let report = Report::failed(&check(&machine, &project.dir));
    let problem = report.problem("manifest", "manifest-missing");
    assert_points_as_the_tooling(&problem, &project.dir, None, None);
    assert!(
        problem["message"]
            .as_str()
            .unwrap()
            .contains("defineManifest"),
        "{problem}"
    );
}

#[test]
fn when_the_manifest_cannot_be_read_the_resources_check_is_left_out_and_the_others_are_made() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-unread");
    project.write("manifest.ts", NOT_LITERAL);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/broken.ts", ONE_TYPE_ERROR);
    // Pulled and declared nowhere: with no Manifest to go by, nothing is
    // said of it.
    pulled_by_hand(&project, "inventory");
    let report = Report::failed(&check(&machine, &project.dir));
    report.problem("manifest", "manifest-not-literal");
    report.problem("migrations", "migrations-behind");
    report.problem("types", "type-error");
    assert!(
        report.of_check("resources").is_empty(),
        "expected the resources check left out, got {}",
        report.run.describe()
    );
    assert_eq!(report.problems().len(), 3, "{}", report.run.describe());
}

#[test]
fn a_declared_resource_that_is_not_registered_fails_and_names_those_that_are() {
    let (_scenario, machine) = with_resources();
    let project = checked_project("check-not-registered");
    project.write(
        "manifest.ts",
        &manifest_ts(
            "checked",
            "{ payroll: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    let report = Report::failed(&check(&machine, &project.dir));
    let problem = report.problem("resources", "resource-not-registered");
    let message = problem["message"].as_str().unwrap();
    for named in ["payroll", "devops", "inventory", "Platform administrator"] {
        assert!(
            message.contains(named),
            "expected the message to name {named:?} (the Resource declared, the names registered, and whom to ask), got {message:?}"
        );
    }
    assert!(
        report.problems_for("resource-not-pulled").is_empty(),
        "a Resource that is not registered cannot be pulled: one problem is enough\n{}",
        report.run.describe()
    );
}

#[test]
fn a_declared_resource_that_is_not_pulled_fails_and_gives_the_command() {
    let (_scenario, machine) = with_resources();
    let generated_schema = |p: &TempProject| {
        p.write(
            "resources/devops/schema.ts",
            &format!(
                "{SCHEMA_FIRST_LINE}\n// Resource: devops  Fingerprint: sha256:{}\nexport {{}};\n",
                "0".repeat(64)
            ),
        );
    };
    let generated_index = |p: &TempProject| {
        p.write(
            "resources/devops/index.ts",
            &blackbox::structure::link("devops", false),
        );
    };
    for case in ["never pulled", "index.ts missing", "schema.ts missing"] {
        let project = declaring_devops("check-not-pulled");
        match case {
            "index.ts missing" => generated_schema(&project),
            "schema.ts missing" => generated_index(&project),
            _ => {}
        }
        let report = Report::failed(&check(&machine, &project.dir));
        let problem = report.problem("resources", "resource-not-pulled");
        let message = problem["message"].as_str().unwrap();
        assert!(
            message.contains("vm pull devops"),
            "{case}: expected the message to give the command `vm pull devops`, got {message:?}"
        );
    }
}

#[test]
fn a_resource_file_the_tool_did_not_write_is_named_and_to_be_moved_away() {
    let (_scenario, machine) = with_resources();
    for file in ["schema.ts", "index.ts"] {
        let project = declaring_devops("check-hand-written");
        pulled_by_hand(&project, "devops");
        project.write(
            &format!("resources/devops/{file}"),
            "// Written by hand.\nexport const mine = 1;\n",
        );
        let report = Report::failed(&check(&machine, &project.dir));
        let problem = report.problem("resources", "resource-not-pulled");
        let message = problem["message"].as_str().unwrap();
        assert!(
            message.contains("vm pull devops")
                && message.contains(&format!("resources/devops/{file}"))
                && message.to_lowercase().contains("move"),
            "expected the message to name resources/devops/{file}, say it is to be moved away first, and give `vm pull devops`, got {message:?}"
        );
    }
}

#[test]
fn a_resource_pulled_by_the_tool_passes() {
    let (scenario, machine) = with_resources();
    let project = declaring_devops("check-pulled");
    machine
        .run(&[
            "pull",
            "devops",
            "--project",
            project.dir.to_str().unwrap(),
            "--json",
        ])
        .ok_json();
    Report::clean(&check(&machine, &project.dir));
    drop(scenario);
}

#[test]
fn a_schema_changed_without_a_migration_fails_and_says_how_to_generate_one() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-behind");
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let report = Report::failed(&check(&machine, &project.dir));
    let problem = report.problem("migrations", "migrations-behind");
    let message = problem["message"].as_str().unwrap();
    assert!(
        message.contains("src/db/schema.ts") && message.contains("drizzle-kit generate"),
        "expected the message to say src/db/schema.ts changed since the last migration and to generate one with drizzle-kit generate, got {message:?}"
    );
}

#[test]
fn migrations_generated_as_the_contract_says_are_in_step_until_the_schema_changes() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-in-step");
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let generated = generate_migrations(&project.dir);
    assert_eq!(generated.code, Some(0), "{}", generated.describe());
    assert_eq!(sql_files(&project.dir).len(), 1, "{}", generated.describe());
    Report::clean(&check(&machine, &project.dir));

    // A table added: behind.
    project.write("src/db/schema.ts", NOTES_AND_TAGS_SCHEMA_TS);
    Report::failed(&check(&machine, &project.dir)).problem("migrations", "migrations-behind");
    let generated = generate_migrations(&project.dir);
    assert_eq!(sql_files(&project.dir).len(), 2, "{}", generated.describe());
    Report::clean(&check(&machine, &project.dir));
}

#[test]
fn a_renamed_column_that_drizzle_kit_asks_about_is_not_in_step() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-renamed");
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    generate_migrations(&project.dir);
    Report::clean(&check(&machine, &project.dir));
    project.write("src/db/schema.ts", NOTES_RENAMED_SCHEMA_TS);
    // The harness's own check: drizzle-kit writes nothing for it, and ends
    // with 0.
    let asked = generate_migrations(&project.dir);
    assert_eq!(asked.code, Some(0), "{}", asked.describe());
    assert_eq!(sql_files(&project.dir).len(), 1, "{}", asked.describe());
    Report::failed(&check(&machine, &project.dir)).problem("migrations", "migrations-behind");
}

#[test]
fn a_type_error_fails_with_the_file_the_line_the_column_and_the_compilers_words() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-type-error");
    project.write("src/broken.ts", ONE_TYPE_ERROR);
    let report = Report::failed(&check(&machine, &project.dir));
    let problem = report.problem("types", "type-error");
    assert!(
        problem["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("src/broken.ts")),
        "expected `file` src/broken.ts, got {problem}"
    );
    assert_eq!(problem["line"], json!(1), "{problem}");
    assert_eq!(problem["column"], json!(14), "{problem}");
    let message = problem["message"].as_str().unwrap();
    assert!(
        message.contains(TYPE_ERROR_MESSAGE),
        "expected the compiler's message in the problem's, got {message:?}"
    );
    assert!(
        message.contains("src/broken.ts") && holds_number(message, 1),
        "expected the message to name the file and the line, got {message:?}"
    );
}

#[test]
fn more_than_fifty_type_errors_give_fifty_and_how_many_more() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-many-errors");
    project.write("src/many.ts", &type_errors(60));
    let run = check(&machine, &project.dir);
    run.json_error(1, "check-failed");
    let report = Report {
        body: run.json(),
        run: run.clone(),
    };
    report.assert_shape();
    let types = report.of_check("types");
    assert_eq!(
        types.len(),
        51,
        "expected 50 type errors and one last problem saying how many more\n{}",
        run.describe()
    );
    for (index, problem) in types.iter().take(50).enumerate() {
        assert_eq!(problem["reason"], json!("type-error"), "{problem}");
        assert!(
            problem["file"]
                .as_str()
                .is_some_and(|f| f.ends_with("src/many.ts")),
            "{problem}"
        );
        assert_eq!(problem["line"], json!(index + 1), "{problem}");
        assert_eq!(problem["column"], json!(14), "{problem}");
    }
    let last = &types[50];
    assert_eq!(last["reason"], json!("type-error"), "{last}");
    assert_eq!(last["more"], json!(10), "expected `more`: 10, got {last}");
    for field in ["file", "line", "column"] {
        assert!(
            last.get(field).is_none(),
            "expected the last problem to have no `{field}`, got {last}"
        );
    }
    let message = last["message"].as_str().unwrap();
    assert!(
        holds_number(message, 10) && message.contains("more"),
        "expected the last problem's message to say how many more (10), got {message:?}"
    );
}

#[test]
fn several_problems_are_all_reported_and_the_error_says_how_many_and_the_first() {
    let (_scenario, machine) = with_resources();
    let project = checked_project("check-several");
    project.write(
        "manifest.ts",
        &manifest_ts(
            "checked",
            "{ payroll: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    project.write("src/broken.ts", ONE_TYPE_ERROR);
    let report = Report::failed(&check(&machine, &project.dir));
    report.problem("resources", "resource-not-registered");
    report.problem("migrations", "migrations-behind");
    report.problem("types", "type-error");
    assert_eq!(report.problems().len(), 3, "{}", report.run.describe());
    let message = report.error_message();
    let first = &report.problems()[0];
    assert!(
        holds_number(&message, 3)
            && (message.contains(first["message"].as_str().unwrap())
                || message.contains(first["reason"].as_str().unwrap())),
        "expected the error's message to say how many problems (3) and the first ({first}), got {message:?}"
    );

    // In words: on standard error, and nothing on standard output.
    let words = check_in_words(&machine, &project.dir).text_error(1);
    for said in ["payroll", "src/db/schema.ts", "src/broken.ts"] {
        assert!(
            words.contains(said),
            "expected the words to say {said:?}, got\n{words}"
        );
    }
}

/// A program with `devops` registered (contact [`CONTACT`]) and Grants on
/// it: `viewers` at `structure`, `readers` at `read`, `writers` at
/// `write`.
fn graded() -> Scenario {
    let scenario = Scenario::start();
    scenario.register_writable("devops");
    scenario.grant("devops", "viewers", "structure");
    scenario.grant("devops", "readers", "read");
    scenario.grant("devops", "writers", "write");
    scenario
}

/// The checked project declaring `devops` at `access`, with its files
/// pulled by hand.
fn declaring_at(access: &str) -> TempProject {
    let project = checked_project(&format!("check-declared-{access}"));
    project.write(
        "manifest.ts",
        &manifest_ts(
            "checked",
            &format!("{{ devops: {{ kind: \"postgres\", access: \"{access}\" }} }}"),
        ),
    );
    pulled_by_hand(&project, "devops");
    project
}

#[test]
fn an_own_grant_below_the_declared_level_warns_and_does_not_fail() {
    let scenario = graded();
    let read = declaring_at("read");
    let write = declaring_at("write");
    for (group, project, declared, held) in [
        ("elsewhere", &read, "read", Value::Null),
        ("viewers", &read, "read", json!("structure")),
        ("readers", &write, "write", json!("read")),
    ] {
        let machine = Machine::new();
        machine.sign_in(&scenario.vm, &User::new("bea").in_groups(&[group]));
        let report = Report::passed(&check(&machine, &project.dir));
        let warnings = report.warnings_for("own-grant-below-declared");
        assert_eq!(
            warnings.len(),
            1,
            "{group}: expected one warning\n{}",
            report.run.describe()
        );
        let warning = &warnings[0];
        assert_eq!(warning["resource"], json!("devops"), "{warning}");
        assert_eq!(warning["declared"], json!(declared), "{warning}");
        assert_eq!(warning["held"], held, "{warning}");
        assert_eq!(warning["contact"], json!(CONTACT), "{warning}");
        let message = warning["message"].as_str().unwrap();
        let lower = message.to_lowercase();
        assert!(
            message.contains("devops")
                && lower.contains("refused")
                && lower.contains("user")
                && message.contains(CONTACT),
            "{group}: expected the message to say that the Builder and every User whose Groups hold no Grant at that level will be refused the Resource's operations, and whom to ask, got {message:?}"
        );
        assert_eq!(report.warnings().len(), 1, "{}", report.run.describe());

        // In words, on standard output: the check passed.
        let words = check_in_words(&machine, &project.dir).ok_text();
        assert!(
            words.contains(CONTACT),
            "expected the words to say whom to ask, got\n{words}"
        );
    }
}

#[test]
fn an_own_grant_at_the_declared_level_or_above_says_nothing() {
    let scenario = graded();
    let read = declaring_at("read");
    let write = declaring_at("write");
    for (group, project) in [("readers", &read), ("writers", &read), ("writers", &write)] {
        let machine = Machine::new();
        machine.sign_in(&scenario.vm, &User::new("bea").in_groups(&[group]));
        Report::clean(&check(&machine, &project.dir));
    }
}

#[test]
fn a_pulled_resource_the_manifest_does_not_declare_is_a_note() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-undeclared");
    pulled_by_hand(&project, "inventory");
    // A folder whose schema.ts was not written by `vm pull` is no pulled
    // Resource.
    project.write("resources/mine/schema.ts", "export const mine = 1;\n");
    let report = Report::passed(&check(&machine, &project.dir));
    assert!(report.warnings().is_empty(), "{}", report.run.describe());
    let notes = report.notes_for("pulled-not-declared");
    assert_eq!(notes.len(), 1, "{}", report.run.describe());
    assert_eq!(notes[0]["resource"], json!("inventory"), "{}", notes[0]);
    let message = notes[0]["message"].as_str().unwrap();
    assert!(
        message.contains("inventory") && message.to_lowercase().contains("declare"),
        "expected the note to say to declare it or remove the folder, got {message:?}"
    );
    assert_eq!(report.notes().len(), 1, "{}", report.run.describe());
    // In words, on standard output.
    let words = check_in_words(&machine, &project.dir).ok_text();
    assert!(words.contains("inventory"), "{words}");
}

#[test]
fn the_check_reads_no_drizzle_config() {
    let (_vm, machine) = signed_in();
    let elsewhere_config = "export default { dialect: \"postgresql\", schema: \"./elsewhere/schema.ts\", out: \"./elsewhere/migrations\" };\n";

    // In step by src/db/schema.ts and migrations/; the configuration points
    // at a schema with a table and no migration.
    let in_step = checked_project("check-config-in-step");
    in_step.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    generate_migrations(&in_step.dir);
    in_step.write("elsewhere/schema.ts", NOTES_AND_TAGS_SCHEMA_TS);
    in_step.write("drizzle.config.ts", elsewhere_config);
    in_step.write("drizzle.config.json", "{ \"dialect\": \"postgresql\", \"schema\": \"./elsewhere/schema.ts\", \"out\": \"./elsewhere/migrations\" }\n");
    Report::clean(&check(&machine, &in_step.dir));

    // Behind by src/db/schema.ts; the configuration points at an empty
    // schema, which is in step with its empty folder.
    let behind = checked_project("check-config-behind");
    behind.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    behind.write("elsewhere/schema.ts", "export {};\n");
    behind.write("elsewhere/migrations/.gitkeep", "");
    behind.write("drizzle.config.ts", elsewhere_config);
    Report::failed(&check(&machine, &behind.dir)).problem("migrations", "migrations-behind");
}

/// Fails unless a message with a tool's words holds its error and not its
/// stack: no line of a stack trace and no path of the check's own folder.
fn assert_words_not_stack(message: &str) {
    assert_eq!(
        stack_line_in(message),
        None,
        "expected the tool's error without its stack, got {message:?}"
    );
    assert!(
        !message.contains("vm-check-"),
        "expected no path of the check's own folder, got {message:?}"
    );
}

/// Fails unless the report has one problem of the check `migrations`,
/// `own-schema-failed`, whose message has `words` (the tool's own) and
/// says to correct `src/db/schema.ts`; and the types check was made too.
fn assert_own_schema_failed(report: &Report, words: &str) {
    assert_eq!(
        report.of_check("migrations").len(),
        1,
        "expected one problem of the migrations check\n{}",
        report.run.describe()
    );
    let problem = report.problem("migrations", "own-schema-failed");
    let message = problem["message"].as_str().unwrap();
    assert!(
        message.contains(words),
        "expected the message to have drizzle-kit's own words {words:?}, got {message:?}"
    );
    assert!(
        message.contains("src/db/schema.ts") && message.to_lowercase().contains("correct"),
        "expected the message to say to correct src/db/schema.ts, got {message:?}"
    );
    assert_words_not_stack(message);
    assert!(
        report.problems_for("type-error").iter().any(|p| p["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("src/broken.ts"))),
        "expected the other checks still made (the type error in src/broken.ts)\n{}",
        report.run.describe()
    );
}

#[test]
fn an_own_schema_that_is_not_there_is_own_schema_failed() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-schema-missing");
    std::fs::remove_file(project.path("src/db/schema.ts")).unwrap();
    project.write("src/broken.ts", ONE_TYPE_ERROR);
    let report = Report::failed(&check(&machine, &project.dir));
    assert_own_schema_failed(&report, "No schema files found");
}

#[test]
fn an_own_schema_that_throws_when_loaded_is_own_schema_failed() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-schema-throws");
    project.write(
        "src/db/schema.ts",
        "throw new Error(\"the schema refuses to load\");\nexport {};\n",
    );
    project.write("src/broken.ts", ONE_TYPE_ERROR);
    let report = Report::failed(&check(&machine, &project.dir));
    assert_own_schema_failed(&report, "the schema refuses to load");
}

#[test]
fn an_own_schema_that_imports_what_is_not_there_is_own_schema_failed() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-schema-import");
    project.write(
        "src/db/schema.ts",
        "import { nothing } from \"./not-there.ts\";\nexport const x = nothing;\n",
    );
    project.write("src/broken.ts", ONE_TYPE_ERROR);
    let report = Report::failed(&check(&machine, &project.dir));
    assert_own_schema_failed(&report, "Cannot find module './not-there.ts'");
    // tsc says so too, with its place.
    assert!(
        report.problems_for("type-error").iter().any(|p| p["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("src/db/schema.ts"))
            && p["line"] == json!(1)),
        "{}",
        report.run.describe()
    );
}

#[test]
fn a_compiler_error_with_no_place_is_a_type_error_with_null_file_line_and_column() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-no-tsconfig");
    std::fs::remove_file(project.path("tsconfig.json")).unwrap();
    let report = Report::failed(&check(&machine, &project.dir));
    let problem = report.problem("types", "type-error");
    for field in ["file", "line", "column"] {
        assert!(
            problem.get(field).is_some_and(Value::is_null),
            "expected `{field}` null for an error the compiler gives with no place, got {problem}"
        );
    }
    let message = problem["message"].as_str().unwrap();
    assert!(
        message.contains("tsconfig.json"),
        "expected the compiler's message (it names tsconfig.json), got {message:?}"
    );
}

/// The checked project with the `notes` table and its one migration
/// generated, then changed by `spoil` (in `migrations/`), with a type error
/// in `src/broken.ts`.
fn spoiled_migrations(label: &str, spoil: impl Fn(&Path)) -> TempProject {
    let project = checked_project(label);
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    let generated = generate_migrations(&project.dir);
    assert_eq!(sql_files(&project.dir).len(), 1, "{}", generated.describe());
    spoil(&project.path("migrations"));
    project.write("src/broken.ts", ONE_TYPE_ERROR);
    project
}

/// The first `*_snapshot.json` in `migrations/meta`.
fn first_snapshot(migrations: &Path) -> std::path::PathBuf {
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(migrations.join("meta"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with("_snapshot.json"))
        .collect();
    found.sort();
    found.into_iter().next().unwrap()
}

/// Fails unless the report has one problem of the check `migrations`,
/// `migrations-unreadable`, whose message has `words` (drizzle-kit's own,
/// or the file it names),
/// says migrations are never edited by hand and to bring the folder back
/// as it was last committed; and the types check was made too.
fn assert_migrations_unreadable(report: &Report, words: &str) {
    assert_eq!(
        report.of_check("migrations").len(),
        1,
        "expected one problem of the migrations check\n{}",
        report.run.describe()
    );
    let problem = report.problem("migrations", "migrations-unreadable");
    let message = problem["message"].as_str().unwrap();
    let lower = message.to_lowercase();
    assert!(
        message.contains(words),
        "expected the message to have drizzle-kit's own words {words:?}, got {message:?}"
    );
    assert!(
        lower.contains("by hand") && lower.contains("committed"),
        "expected the message to say migrations are never edited by hand and to bring the folder back as it was last committed, got {message:?}"
    );
    assert_words_not_stack(message);
    assert!(
        report.problems_for("type-error").iter().any(|p| p["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("src/broken.ts"))),
        "expected the other checks still made (the type error in src/broken.ts)\n{}",
        report.run.describe()
    );
}

#[test]
fn a_journal_that_is_not_json_is_migrations_unreadable() {
    let (_vm, machine) = signed_in();
    let project = spoiled_migrations("check-journal-not-json", |m| {
        std::fs::write(m.join("meta/_journal.json"), "{ not json\n").unwrap();
    });
    let report = Report::failed(&check(&machine, &project.dir));
    assert_migrations_unreadable(&report, "Expected property name");
}

#[test]
fn a_snapshot_that_is_not_json_is_migrations_unreadable() {
    let (_vm, machine) = signed_in();
    let project = spoiled_migrations("check-snapshot-not-json", |m| {
        std::fs::write(first_snapshot(m), "{ not json\n").unwrap();
    });
    let report = Report::failed(&check(&machine, &project.dir));
    assert_migrations_unreadable(&report, "Expected property name");
}

#[test]
fn a_journal_removed_beside_its_migration_and_snapshot_is_migrations_unreadable() {
    let (_vm, machine) = signed_in();
    let project = spoiled_migrations("check-journal-gone", |m| {
        std::fs::remove_file(m.join("meta/_journal.json")).unwrap();
    });
    let report = Report::failed(&check(&machine, &project.dir));
    assert_migrations_unreadable(&report, "no such file or directory");
    // drizzle-kit names the journal of the copy; the message names the
    // project's.
    let problem = report.problem("migrations", "migrations-unreadable");
    assert!(
        problem["message"]
            .as_str()
            .unwrap()
            .contains("migrations/meta/_journal.json"),
        "expected the message to name migrations/meta/_journal.json, got {problem}"
    );
}

/// drizzle-kit reads the snapshots it finds in `meta/`, not those the
/// journal names: with the snapshot gone it takes the folder for one with
/// no migration and would write one. That is `migrations-behind`.
#[test]
fn a_snapshot_removed_is_migrations_behind() {
    let (_vm, machine) = signed_in();
    let project = spoiled_migrations("check-snapshot-gone", |m| {
        std::fs::remove_file(first_snapshot(m)).unwrap();
    });
    let report = Report::failed(&check(&machine, &project.dir));
    report.problem("migrations", "migrations-behind");
    assert_eq!(report.of_check("migrations").len(), 1);
}

/// drizzle-kit does not read the `.sql` files: with one removed and its
/// journal entry and snapshot still there, it says "No schema changes".
/// The check sees that the journal and the files do not agree.
#[test]
fn a_migration_the_journal_names_whose_sql_file_is_gone_is_migrations_unreadable() {
    let (_vm, machine) = signed_in();
    let project = spoiled_migrations("check-sql-gone", |_| {});
    let gone = sql_files(&project.dir).into_iter().next().unwrap();
    std::fs::remove_file(project.path(&format!("migrations/{gone}"))).unwrap();
    let report = Report::failed(&check(&machine, &project.dir));
    assert_migrations_unreadable(&report, &format!("migrations/{gone}"));
}

#[test]
fn a_sql_file_the_journal_does_not_name_is_migrations_unreadable() {
    let (_vm, machine) = signed_in();
    let project = spoiled_migrations("check-sql-stray", |m| {
        std::fs::write(
            m.join("0001_stray.sql"),
            "ALTER TABLE \"notes\" ADD COLUMN \"extra\" text;\n",
        )
        .unwrap();
    });
    let report = Report::failed(&check(&machine, &project.dir));
    assert_migrations_unreadable(&report, "migrations/0001_stray.sql");
}

/// A new project's `migrations/`: only `.gitkeep`, no journal, no
/// migration, and a schema with no table. In step: the journal the
/// contract asks to agree with the files is not there to disagree.
#[test]
fn a_migrations_folder_with_only_gitkeep_and_no_table_is_in_step() {
    let (_vm, machine) = signed_in();
    let project = checked_project("check-gitkeep-only");
    assert_eq!(
        Tree::of(&project.path("migrations")).paths(),
        [".gitkeep"],
        "the harness's own check"
    );
    Report::clean(&check(&machine, &project.dir));
}

/// drizzle-kit reads `{}` in place of its snapshot, ends with 0, writes
/// nothing, and says on standard output
/// `migrations/meta/0000_snapshot.json data is malformed`.
#[test]
fn a_snapshot_that_is_json_but_no_snapshot_is_migrations_unreadable() {
    let (_vm, machine) = signed_in();
    let project = spoiled_migrations("check-snapshot-not-a-snapshot", |m| {
        std::fs::write(first_snapshot(m), "{}\n").unwrap();
    });
    let report = Report::failed(&check(&machine, &project.dir));
    assert_migrations_unreadable(&report, "data is malformed");
    let problem = report.problem("migrations", "migrations-unreadable");
    assert!(
        problem["message"]
            .as_str()
            .unwrap()
            .contains("migrations/meta/0000_snapshot.json"),
        "expected the message to name the project's migrations/meta/0000_snapshot.json, got {problem}"
    );
}

#[test]
fn of_more_than_ten_files_that_do_not_agree_ten_are_named_and_how_many_more() {
    let (_vm, machine) = signed_in();
    let strays: Vec<String> = (1..=12).map(|i| format!("{i:04}_stray.sql")).collect();
    let project = spoiled_migrations("check-sql-strays", |m| {
        for stray in &strays {
            std::fs::write(m.join(stray), "SELECT 1;\n").unwrap();
        }
    });
    let report = Report::failed(&check(&machine, &project.dir));
    assert_eq!(
        report.problems_for("migrations-unreadable").len(),
        1,
        "expected one problem for all the files\n{}",
        report.run.describe()
    );
    assert_migrations_unreadable(&report, "migrations/");
    let problem = report.problem("migrations", "migrations-unreadable");
    let message = problem["message"].as_str().unwrap();
    let named = strays
        .iter()
        .filter(|s| message.contains(&format!("migrations/{s}")))
        .count();
    assert_eq!(
        named, 10,
        "expected ten of the twelve files named, got {named}: {message:?}"
    );
    assert!(
        holds_number(message, 2) && message.contains("more"),
        "expected the message to say there are 2 more, got {message:?}"
    );
}

#[test]
fn a_journal_larger_than_4_mib_is_migrations_unreadable() {
    let (_vm, machine) = signed_in();
    let project = spoiled_migrations("check-journal-large", |m| {
        let journal = m.join("meta/_journal.json");
        let mut text = std::fs::read_to_string(&journal).unwrap();
        text.push_str(&" ".repeat(4 * 1024 * 1024 + 1));
        text.push('\n');
        std::fs::write(&journal, text).unwrap();
    });
    // The harness's own check: still a valid journal.
    let journal = std::fs::read_to_string(project.path("migrations/meta/_journal.json")).unwrap();
    assert!(serde_json::from_str::<Value>(&journal).is_ok());
    assert!(journal.len() > 4 * 1024 * 1024);
    let report = Report::failed(&check(&machine, &project.dir));
    assert_migrations_unreadable(&report, "migrations/meta/_journal.json");
}
