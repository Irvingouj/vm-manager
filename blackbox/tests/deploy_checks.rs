//! The checks of `vibe check` at deploy, the bundle, and the size of what is
//! sent (CONTRACT.md, "Deploying: The command", step 6 and what follows
//! step 8; "What the installation may answer"). A problem of any of the
//! four checks ends the deploy exactly as it ends the check: exit code `1`,
//! `check-failed`, the same report, and nothing sent. A project that does
//! not bundle is `build-failed` with the tooling's `errors`; a Version over
//! 16 MiB is `too-large`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;

use blackbox::deploy::{
    BUILDERS, IMPORTS_NOWHERE_TS, NOWHERE_DTS, commit_all, dana, deploy, deploy_in_words,
    deploy_project, init_repository, project_tree, signed_in,
};
use blackbox::generate::{Generated, generate};
use blackbox::project::{NOTES_SCHEMA_TS, Report, check, manifest_ts, type_errors};
use blackbox::scenario::Scenario;
use blackbox::sdk::TempProject;
use blackbox::tool::Machine;
use serde_json::{Value, json};

/// Deploys `project` (named `name`), which must fail its checks: fails
/// unless the deploy ends as `vibe check` on the same project ends, with the
/// same problems, nothing written in the project, and nothing sent.
fn assert_fails_as_the_check(
    scenario: &Scenario,
    machine: &Machine,
    project: &TempProject,
    name: &str,
    check_name: &str,
    reason: &str,
) -> Report {
    let before = project_tree(&project.dir);
    let run = deploy(machine, &project.dir, &[]);
    let report = Report::failed(&run);
    let checked = Report::failed(&check(machine, &project.dir));
    assert!(
        !checked.of_check(check_name).is_empty(),
        "the harness's own check: the check finds a problem of `{check_name}`\n{}",
        checked.run.describe()
    );
    assert!(
        report
            .problems_for(reason)
            .iter()
            .all(|p| p["check"] == check_name)
            && !report.problems_for(reason).is_empty(),
        "expected a problem `{reason}` of the check `{check_name}`\n{}",
        run.describe()
    );
    assert_eq!(
        report.problems(),
        checked.problems(),
        "expected the deploy's report to be the check's: the same problems"
    );
    deploy_in_words(machine, &project.dir, &[]).text_error(1);
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new(),
        "expected nothing written in the project"
    );
    let answer = scenario.vm.api_as(&dana()).get_application(name);
    assert_eq!(
        answer.status,
        404,
        "expected nothing sent (no Application {name:?} created)\n{}",
        answer.describe()
    );
    report
}

fn ready() -> (Scenario, Machine) {
    let scenario = Scenario::start();
    let machine = signed_in(&scenario.vm);
    (scenario, machine)
}

/// A Manifest whose `name` is an identifier.
const NOT_LITERAL: &str = r#"import { defineManifest } from "@vm/sdk";

const n = "not-literal-board";
export default defineManifest({
  name: n,
  description: "A project for the tests of vibe check.",
  resources: {},
});
"#;

#[test]
fn a_manifest_that_is_not_a_plain_literal_fails_the_deploy_as_it_fails_the_check() {
    let (scenario, machine) = ready();
    let project = deploy_project("deploy-check-manifest", "not-literal-board");
    project.write("manifest.ts", NOT_LITERAL);
    commit_all(&project.dir, "not a literal");
    assert_fails_as_the_check(
        &scenario,
        &machine,
        &project,
        "not-literal-board",
        "manifest",
        "manifest-not-literal",
    );
}

#[test]
fn a_declared_resource_that_is_not_registered_fails_the_deploy_as_it_fails_the_check() {
    let (scenario, machine) = ready();
    let project = deploy_project("deploy-check-unregistered", "unregistered-board");
    project.write(
        "manifest.ts",
        &manifest_ts(
            "unregistered-board",
            "{ nosuch: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    commit_all(&project.dir, "declares nosuch");
    assert_fails_as_the_check(
        &scenario,
        &machine,
        &project,
        "unregistered-board",
        "resources",
        "resource-not-registered",
    );
}

#[test]
fn a_declared_resource_that_is_not_pulled_is_left_to_the_check_and_not_pulled() {
    let (scenario, machine) = ready();
    scenario.register_writable("devops");
    scenario.grant("devops", BUILDERS, "read");
    let project = deploy_project("deploy-check-unpulled", "unpulled-board");
    project.write(
        "manifest.ts",
        &manifest_ts(
            "unpulled-board",
            "{ devops: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    commit_all(&project.dir, "declares devops");
    let report = assert_fails_as_the_check(
        &scenario,
        &machine,
        &project,
        "unpulled-board",
        "resources",
        "resource-not-pulled",
    );
    assert!(
        report.problem("resources", "resource-not-pulled")["message"]
            .as_str()
            .unwrap()
            .contains("vibe pull devops")
    );
    assert!(
        !project.dir.join("resources").exists(),
        "expected the deploy not to pull a Resource that was never pulled"
    );
}

#[test]
fn migrations_behind_the_schema_fail_the_deploy_as_they_fail_the_check() {
    let (scenario, machine) = ready();
    let project = deploy_project("deploy-check-behind", "behind-board");
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    commit_all(&project.dir, "a table and no migration");
    assert_fails_as_the_check(
        &scenario,
        &machine,
        &project,
        "behind-board",
        "migrations",
        "migrations-behind",
    );
}

#[test]
fn a_type_error_fails_the_deploy_as_it_fails_the_check() {
    let (scenario, machine) = ready();
    let project = deploy_project("deploy-check-types", "typed-board");
    project.write("src/wrong.ts", &type_errors(2));
    commit_all(&project.dir, "two type errors");
    let report = assert_fails_as_the_check(
        &scenario,
        &machine,
        &project,
        "typed-board",
        "types",
        "type-error",
    );
    assert_eq!(report.problems_for("type-error").len(), 2);
}

#[test]
fn a_project_that_type_checks_and_does_not_bundle_is_build_failed() {
    let (scenario, machine) = ready();
    let project = deploy_project("deploy-build-failed", "unbuilt-board");
    project.write("src/nowhere.d.ts", NOWHERE_DTS);
    project.write("src/index.ts", IMPORTS_NOWHERE_TS);
    commit_all(&project.dir, "imports what is nowhere");
    let before = project_tree(&project.dir);
    let run = deploy(&machine, &project.dir, &[]);
    let message = run.json_error(1, "build-failed");
    let errors = run.json()["error"]["errors"].clone();
    assert!(
        errors.as_array().is_some_and(|e| !e.is_empty()),
        "expected the tooling's `errors` beside the reason\n{}",
        run.describe()
    );
    assert!(
        errors.to_string().contains("vm-harness-nowhere") || message.contains("vm-harness-nowhere"),
        "expected the tooling's error naming what it could not resolve\n{}",
        run.describe()
    );
    deploy_in_words(&machine, &project.dir, &[]).text_error(1);
    assert_eq!(
        before.differences(&project_tree(&project.dir)),
        Vec::<String>::new()
    );
    assert_eq!(
        scenario
            .vm
            .api_as(&dana())
            .get_application("unbuilt-board")
            .status,
        404,
        "expected nothing sent: the bundle comes before the Application is created"
    );
}

#[test]
fn a_version_over_16_mib_is_too_large_and_nothing_is_kept() {
    let (scenario, machine) = ready();
    let project = blackbox::project::checked_project("deploy-too-large");
    project.write("manifest.ts", &manifest_ts("large-board", "{}"));
    let made = Generated::made(
        &generate(&machine, &project.dir, &["--empty", "--name", "big"]),
        &project.dir,
    );
    let mut sql = String::with_capacity(17 * 1024 * 1024 + 64);
    let line = format!("-- {}\n", "x".repeat(1020));
    while sql.len() <= 17 * 1024 * 1024 {
        sql.push_str(&line);
    }
    sql.push_str("SELECT 1;\n");
    fs::write(project.dir.join(&made.file), sql).unwrap();
    init_repository(&project.dir);
    commit_all(&project.dir, "a large migration");
    let run = deploy(&machine, &project.dir, &[]);
    run.json_error(1, "too-large");
    let api = scenario.vm.api_as(&dana());
    let answer = api.get_application("large-board");
    if answer.status == 200 {
        assert_eq!(
            answer.json()["serving_version"],
            Value::Null,
            "expected no Version"
        );
        assert_eq!(api.versions("large-board"), Vec::<Value>::new());
    } else {
        assert_eq!(answer.status, 404, "{}", answer.describe());
    }
    assert_eq!(json!(made.statements), json!(0));
}
