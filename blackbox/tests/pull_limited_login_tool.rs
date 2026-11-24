//! What a login limited to some tables does to the tool (CONTRACT.md,
//! "Resources and Grants in full: A login limited to some tables"): `vibe
//! pull` prints `hidden` with `--json` and, without it, a line in words when
//! it is not 0 (that `n` relations are not shown, that code written against
//! one will not type-check, whom to ask); and what follows for a project
//! pulled before: a deploy pulls again and the definition changes
//! (`structure-updated`), the check ends a deploy whose code uses a relation
//! no login reaches any more (`check-failed`), a Version that serves keeps
//! its recorded structures.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::path::Path;

use blackbox::deploy::{
    BUILDERS, Deployed, READS_INCIDENTS_TS, commit_all, dana, deploy, fingerprint_in,
    init_repository, signed_in,
};
use blackbox::project::{Report, checked_project, manifest_ts};
use blackbox::resources::{login_with_privileges, payroll_shape};
use blackbox::scenario::{CONTACT, READERS, Scenario};
use blackbox::sdk::TempProject;
use blackbox::structure::pull;
use blackbox::tool::Machine;
use blackbox::{User, assert_api_error};
use serde_json::{Value, json};

const DEVOPS: &str = "devops";

fn rita() -> User {
    User::new("rita").in_groups(&[READERS])
}

/// A program, a shared database of `incidents`, `audit` and `payroll`, and
/// `devops` registered with a read login that holds `SELECT` on `incidents`
/// and on each of `also`. [`READERS`] and [`BUILDERS`] hold `read` on it.
fn world(also: &[&str]) -> (Scenario, blackbox::shared_database::Login) {
    let s = Scenario::start();
    payroll_shape(&s.db);
    let mut grants = vec!["SELECT ON incidents".to_owned()];
    grants.extend(also.iter().map(|t| format!("SELECT ON {t}")));
    let grants: Vec<&str> = grants.iter().map(String::as_str).collect();
    let read = login_with_privileges(&s.db, &grants);
    s.register_body(
        DEVOPS,
        &blackbox::resources::body(
            DEVOPS,
            &blackbox::resources::connection(&s.db, &read, None, None),
        ),
    );
    s.grant(DEVOPS, READERS, "read");
    s.grant(DEVOPS, BUILDERS, "read");
    (s, read)
}

fn pull_args(dir: &str, json: bool) -> Vec<&str> {
    let mut args = vec!["pull", DEVOPS, "--project", dir];
    if json {
        args.push("--json");
    }
    args
}

fn project_of(machine: &Machine, name: &str) -> std::path::PathBuf {
    let dir = machine.root().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.ts"),
        manifest_ts(name, "{ devops: { kind: \"postgres\", access: \"read\" } }"),
    )
    .unwrap();
    dir
}

#[test]
fn pull_json_prints_hidden_as_the_api_answers_it() {
    let (s, _read) = world(&[]);
    let api = pull(&s.vm.api_as(&rita()), DEVOPS);
    api.expect_status(200);
    let api = api.json();
    assert_eq!(api["hidden"], json!(2), "{api}");
    let machine = Machine::new();
    machine.sign_in(&s.vm, &rita());
    let dir = project_of(&machine, "board");
    let printed = machine
        .run(&pull_args(dir.to_str().unwrap(), true))
        .ok_json();
    assert_eq!(printed["hidden"], json!(2), "{printed}");
    assert_eq!(printed["fingerprint"], api["fingerprint"]);
}

#[test]
fn pull_in_words_says_how_many_relations_are_not_shown_and_whom_to_ask() {
    let (s, _read) = world(&[]);
    let machine = Machine::new();
    machine.sign_in(&s.vm, &rita());
    let dir = project_of(&machine, "board");
    let words = machine
        .run(&pull_args(dir.to_str().unwrap(), false))
        .ok_text();
    let flat = words.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = flat.to_lowercase();
    assert!(
        words
            .lines()
            .any(|l| l.to_lowercase().contains("not shown") && l.contains('2')),
        "expected a line saying that 2 relations are not shown\n{words}"
    );
    assert!(
        lower.contains("type-check") || lower.contains("type check"),
        "expected the words to say that code written against one will not type-check\n{words}"
    );
    assert!(
        flat.contains(CONTACT),
        "expected the words to name the Resource's contact, {CONTACT:?}\n{words}"
    );
    assert!(
        lower.contains("logins"),
        "expected the words to say that none of the Resource's logins can reach them\n{words}"
    );
    for hidden in ["payroll", "audit"] {
        assert!(
            !words.contains(hidden),
            "expected no name of a hidden relation in the words, found {hidden:?}\n{words}"
        );
    }
}

#[test]
fn with_nothing_hidden_the_words_have_no_such_line_and_json_says_0() {
    let (s, read) = world(&["audit", "payroll"]);
    let _ = read;
    let machine = Machine::new();
    machine.sign_in(&s.vm, &rita());
    let dir = project_of(&machine, "board");
    let words = machine
        .run(&pull_args(dir.to_str().unwrap(), false))
        .ok_text();
    assert!(
        !words.to_lowercase().contains("not shown"),
        "expected no line about relations not shown when none is hidden\n{words}"
    );
    let printed = machine
        .run(&pull_args(dir.to_str().unwrap(), true))
        .ok_json();
    assert_eq!(printed["hidden"], json!(0), "{printed}");
}

#[test]
fn a_refused_pull_says_nothing_of_hidden() {
    let (s, _read) = world(&[]);
    let nora = User::new("nora").in_groups(&["elsewhere"]);
    let machine = Machine::new();
    machine.sign_in(&s.vm, &nora);
    let dir = project_of(&machine, "board");
    let run = machine.run(&pull_args(dir.to_str().unwrap(), true));
    run.expect_code(3);
    assert!(!run.stdout.contains("hidden") && !run.stderr.contains("2 relations"));
    assert_api_error(&pull(&s.vm.api_as(&nora), DEVOPS), 403, "no-grant");
}

/// A handler that reads `payroll.salary` through the pulled definition.
const READS_PAYROLL_TS: &str = r#"import { defineApp } from "@vm/sdk";
import manifest from "../manifest.ts";
import { payroll } from "../resources/devops/schema.ts";

export default defineApp(manifest, {
  async fetch(_request, ctx) {
    const rows = await ctx.user().db("devops").select({ salary: payroll.salary }).from(payroll);
    return Response.json({ user: ctx.user().id, salaries: rows.map((r) => r.salary) });
  },
});
"#;

fn declaring(name: &str) -> String {
    manifest_ts(name, "{ devops: { kind: \"postgres\", access: \"read\" } }")
}

/// A project named `name` whose handler is `code`, pulled by `machine`
/// (while the Resource's logins reach what they reach now), committed.
fn pulled_project(machine: &Machine, label: &str, name: &str, code: &str) -> TempProject {
    let project = checked_project(label);
    project.write("manifest.ts", &declaring(name));
    project.write("src/index.ts", code);
    machine
        .run(&[
            "pull",
            DEVOPS,
            "--project",
            project.dir.to_str().unwrap(),
            "--json",
        ])
        .ok_json();
    init_repository(&project.dir);
    commit_all(&project.dir, "pulled");
    project
}

fn schema_of(dir: &Path) -> String {
    fs::read_to_string(dir.join("resources/devops/schema.ts")).unwrap()
}

fn fingerprint_now(s: &Scenario) -> String {
    let answer = pull(&s.vm.api_as(&dana()), DEVOPS);
    answer.expect_status(200);
    answer.json()["fingerprint"].as_str().unwrap().to_owned()
}

#[test]
fn a_project_whose_code_uses_no_relation_that_became_unreachable_deploys_with_the_definition_updated()
 {
    let (s, read) = world(&["payroll"]);
    let machine = signed_in(&s.vm);
    let project = pulled_project(
        &machine,
        "limited-uses-incidents",
        "uses-incidents-board",
        READS_INCIDENTS_TS,
    );
    assert!(schema_of(&project.dir).contains("pgTable(\"payroll\""));
    let first = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(first.version, 1);
    let api = s.vm.api_as(&dana());
    let recorded = api
        .get_version("uses-incidents-board", 1)
        .expect_status(200)
        .json()["structures"]
        .clone();
    commit_all(&project.dir, "deployed");

    // What no login reaches any more is no longer listed.
    s.db.superuser()
        .batch(&format!("REVOKE SELECT ON payroll FROM {}", read.role));
    let now = fingerprint_now(&s);
    let second = Deployed::of(&deploy(&machine, &project.dir, &[]));
    let pulled = second.pulled_of(DEVOPS);
    assert_eq!(pulled["changed"], json!(true), "{pulled}");
    let note = second.notes_for("structure-updated");
    assert_eq!(note.len(), 1, "{}", second.run.describe());
    assert_eq!(note[0]["resource"], json!(DEVOPS));
    let schema = schema_of(&project.dir);
    assert!(!schema.contains("payroll"), "{schema}");
    assert_eq!(fingerprint_in(&schema), Some(now.clone()));
    assert_eq!(second.structures, json!({ DEVOPS: now }));
    // The first Version keeps what it recorded.
    assert_eq!(
        api.get_version("uses-incidents-board", 1)
            .expect_status(200)
            .json()["structures"],
        recorded,
        "expected a Version that was serving to keep its recorded structures"
    );
}

#[test]
fn a_project_whose_code_uses_a_relation_that_became_unreachable_fails_the_check_at_deploy() {
    let (s, read) = world(&["payroll"]);
    let machine = signed_in(&s.vm);
    let project = pulled_project(
        &machine,
        "limited-uses-payroll",
        "uses-payroll-board",
        READS_PAYROLL_TS,
    );
    Deployed::of(&deploy(&machine, &project.dir, &[]));
    commit_all(&project.dir, "deployed");
    let api = s.vm.api_as(&dana());
    let versions_before = api.versions("uses-payroll-board");

    s.db.superuser()
        .batch(&format!("REVOKE SELECT ON payroll FROM {}", read.role));
    let now = fingerprint_now(&s);
    let run = deploy(&machine, &project.dir, &[]);
    let report = Report::failed(&run);
    let errors = report.problems_for("type-error");
    assert!(
        !errors.is_empty() && errors.iter().all(|p| p["check"] == "types"),
        "expected the type error that code written against a relation no login reaches makes\n{}",
        run.describe()
    );
    assert!(
        report.error_message().contains(DEVOPS),
        "expected the message to name the Resource whose structure changed, got {:?}",
        report.error_message()
    );
    assert_eq!(api.versions("uses-payroll-board"), versions_before);
    assert_eq!(
        api.application("uses-payroll-board")["serving_version"],
        json!(1),
        "expected the serving Version untouched"
    );
    let schema = schema_of(&project.dir);
    assert!(!schema.contains("payroll"), "the pulled files are written");
    assert_eq!(fingerprint_in(&schema), Some(now));
}

#[test]
fn a_project_pulled_before_with_nothing_unreachable_pulls_the_same_bytes_again() {
    // "Nothing changes for a relation that a login of the Resource can
    // reach": the same pull, the same bytes, the same fingerprint.
    let (s, _read) = world(&["payroll", "audit"]);
    let machine = signed_in(&s.vm);
    let project = pulled_project(
        &machine,
        "limited-unchanged",
        "unchanged-board",
        READS_PAYROLL_TS,
    );
    let before = schema_of(&project.dir);
    let deployed = Deployed::of(&deploy(&machine, &project.dir, &[]));
    assert_eq!(
        deployed.pulled,
        vec![json!({ "resource": DEVOPS, "changed": false, "files": [] })],
        "{}",
        deployed.run.describe()
    );
    assert_eq!(schema_of(&project.dir), before);
    let _: Value = deployed.body;
}
