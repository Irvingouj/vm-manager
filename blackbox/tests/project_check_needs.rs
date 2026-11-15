//! What `vm check` needs, what it leaves, and how it ends (CONTRACT.md, "A
//! project: the template, creating one and checking one: Checking a
//! project"; "The tool: Every command"; ticket "The project template, and
//! creating and checking a project": the check has a JSON form and uses
//! the tool's exit codes). It needs the tool signed in, Node 22 or later,
//! and the project's packages installed; it writes nothing in the project;
//! it has an end, and stops what it started; the project's text is printed
//! as the tool prints any text that is not its own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::fs;
use std::time::Duration;

use blackbox::project::{
    CHECK_TIMEOUT, KillOnDrop, NODE_20, NOTES_SCHEMA_TS, Report, Tree, check, check_args,
    check_in_words, checked_project, escapes_in, generate_migrations, manifest_ts, pids_in,
    pulled_by_hand, schema_starting_a_child, stand_ins, still_running_after, write_checked,
};
use blackbox::scenario::{Scenario, resource_body_with_write};
use blackbox::tool::{Broken, Machine};
use blackbox::{User, Vm, VmOptions};
use serde_json::json;

fn bea() -> User {
    User::new("bea").in_groups(&["builders"])
}

fn signed_in() -> (Vm, Machine) {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let machine = Machine::new();
    machine.sign_in(&vm, &bea());
    (vm, machine)
}

#[test]
fn not_signed_in_is_exit_code_4() {
    let machine = Machine::new();
    let project = checked_project("needs-sign-in");
    let before = Tree::of(&project.dir);
    check(&machine, &project.dir).json_error(4, "not-signed-in");
    let message = check_in_words(&machine, &project.dir).text_error(4);
    assert!(message.contains("vm sign-in"), "{message:?}");
    assert_eq!(
        before.differences(&Tree::of(&project.dir)),
        Vec::<String>::new()
    );
}

#[test]
fn a_directory_without_a_manifest_is_not_a_project() {
    let (_vm, machine) = signed_in();
    let empty = machine.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    check(&machine, &empty).json_error(1, "not-a-project");
    check_in_words(&machine, &empty).text_error(1);
    // Without --project, the current directory, which has no manifest.ts.
    machine
        .run_into_files(&["check", "--json"], &[], CHECK_TIMEOUT)
        .json_error(1, "not-a-project");
    assert!(machine.cwd().read_dir().unwrap().next().is_none());
}

#[test]
fn without_project_the_current_directory_is_checked() {
    let (_vm, machine) = signed_in();
    let project = checked_project("needs-cwd");
    Report::clean(&machine.run_into_files_in(
        &project.dir,
        &["check", "--json"],
        &[],
        CHECK_TIMEOUT,
    ));
    project.write("src/broken.ts", "export const wrong: number = \"text\";\n");
    Report::failed(&machine.run_into_files_in(
        &project.dir,
        &["check", "--json"],
        &[],
        CHECK_TIMEOUT,
    ))
    .problem("types", "type-error");
}

#[test]
fn no_node_on_the_path_or_one_older_than_22_is_node_missing() {
    let (_vm, machine) = signed_in();
    let project = checked_project("needs-node");
    let dir = project.dir.to_str().unwrap();
    let empty = stand_ins(&machine.root().join("no-node"), &[]);
    let old = stand_ins(&machine.root().join("node-20"), &[("node", NODE_20)]);
    for (case, path) in [("no node", &empty), ("Node 20", &old)] {
        let path = path.to_str().unwrap();
        let run = machine.run_into_files(&check_args(dir, &[]), &[("PATH", path)], CHECK_TIMEOUT);
        let message = run.json_error(1, "node-missing");
        assert!(
            message.contains("Node") && message.contains("22"),
            "{case}: expected the message to say Node 22 or later is needed, got {message:?}"
        );
        let words = machine.run_into_files(
            &["check", "--project", dir],
            &[("PATH", path)],
            CHECK_TIMEOUT,
        );
        words.text_error(1);
    }
}

#[test]
fn a_project_whose_packages_are_not_installed_is_not_installed() {
    let (_vm, machine) = signed_in();
    // Outside the repository: nothing resolves.
    let dir = machine.root().join("bare");
    write_checked(&dir);
    let before = Tree::of(&dir);
    let message = check(&machine, &dir).json_error(1, "not-installed");
    assert!(
        message.contains("npm install"),
        "expected the message to say to run `npm install`, got {message:?}"
    );
    check_in_words(&machine, &dir).text_error(1);
    assert_eq!(before.differences(&Tree::of(&dir)), Vec::<String>::new());
}

#[test]
fn a_timeout_outside_1_to_3600_is_a_wrong_command_line() {
    let (_vm, machine) = signed_in();
    let project = checked_project("needs-timeout");
    let dir = project.dir.to_str().unwrap();
    for timeout in [
        "0",
        "3601",
        "-1",
        "1.5",
        "abc",
        "",
        "1e3",
        "99999999999999999999",
    ] {
        let run = machine.run_into_files(
            &["check", "--project", dir, "--timeout", timeout, "--json"],
            &[],
            CHECK_TIMEOUT,
        );
        assert_eq!(
            run.code,
            Some(2),
            "expected --timeout {timeout:?} to be a wrong command line\n{}",
            run.describe()
        );
    }
    Report::clean(&machine.run_into_files(
        &check_args(dir, &["--timeout", "3600"]),
        &[],
        CHECK_TIMEOUT,
    ));
    let one = machine.run_into_files(&check_args(dir, &["--timeout", "1"]), &[], CHECK_TIMEOUT);
    assert_ne!(
        one.code,
        Some(2),
        "expected 1 a timeout the check takes\n{}",
        one.describe()
    );
}

#[test]
fn the_check_writes_nothing_in_the_project_passing_or_failing() {
    let (_vm, machine) = signed_in();
    let project = checked_project("needs-nothing-written");
    project.write("src/db/schema.ts", NOTES_SCHEMA_TS);
    generate_migrations(&project.dir);
    pulled_by_hand(&project, "inventory");
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    Report::passed(&check(&machine, &project.dir));
    check_in_words(&machine, &project.dir).ok_text();
    assert_eq!(
        before.differences(&Tree::of(&project.dir)),
        Vec::<String>::new(),
        "expected a passing check to write nothing in the project (migrations/ included)"
    );

    // Failing, in every check: behind, a type error, a Resource not pulled.
    project.write(
        "manifest.ts",
        &manifest_ts(
            "checked",
            "{ devops: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    project.write(
        "src/db/schema.ts",
        blackbox::project::NOTES_AND_TAGS_SCHEMA_TS,
    );
    project.write("src/broken.ts", "export const wrong: number = \"text\";\n");
    std::thread::sleep(Duration::from_millis(50));
    let before = Tree::of(&project.dir);
    Report::failed(&check(&machine, &project.dir));
    check_in_words(&machine, &project.dir).text_error(1);
    assert_eq!(
        before.differences(&Tree::of(&project.dir)),
        Vec::<String>::new(),
        "expected a failing check to write nothing in the project (migrations/ included)"
    );
}

#[test]
fn past_its_timeout_the_check_ends_names_the_check_and_leaves_nothing_running() {
    let (_vm, machine) = signed_in();
    let project = checked_project("needs-timed-out");
    let pids = project.dir.with_extension("pids");
    let _cleanup = KillOnDrop(pids.clone());
    project.write("src/db/schema.ts", &schema_starting_a_child(&pids, true));
    let dir = project.dir.to_str().unwrap();
    let run = machine.run_into_files(
        &check_args(dir, &["--timeout", "3"]),
        &[],
        Duration::from_secs(60),
    );
    let message = run.json_error(1, "check-timed-out");
    assert!(
        message.contains("migrations"),
        "expected the message to name the check it was in (migrations), got {message:?}"
    );
    assert!(
        run.elapsed < Duration::from_secs(15),
        "expected the check to end soon after its 3 s, took {:?}",
        run.elapsed
    );
    let started = pids_in(&pids);
    assert!(
        started.len() >= 2,
        "the harness's own check: expected the schema to have been run by drizzle-kit and to have recorded itself and its child, found {started:?}\n{}",
        run.describe()
    );
    let running = still_running_after(&started, Duration::from_secs(1));
    assert!(
        running.is_empty(),
        "expected nothing the check started, nor what that started, still running when it ended; running: {running:?}"
    );
    drop(fs::remove_file(&pids));
}

#[test]
fn a_passing_check_leaves_no_process_behind() {
    let (_vm, machine) = signed_in();
    let project = checked_project("needs-no-leftover");
    let pids = project.dir.with_extension("pids");
    let _cleanup = KillOnDrop(pids.clone());
    project.write("src/db/schema.ts", &schema_starting_a_child(&pids, false));
    Report::clean(&check(&machine, &project.dir));
    let started = pids_in(&pids);
    assert!(
        started.len() >= 2,
        "the harness's own check: expected the schema run, found {started:?}"
    );
    let running = still_running_after(&started, Duration::from_secs(1));
    assert!(
        running.is_empty(),
        "expected nothing the check started, nor what that started, still running when it ended; running: {running:?}"
    );
    drop(fs::remove_file(&pids));
}

/// Fails unless `text` holds no control character or bidirectional mark
/// raw, but for line ends and tabs.
fn assert_no_raw_control(text: &str, context: &str) {
    let raw: Vec<char> = text
        .chars()
        .filter(|c| {
            (c.is_control() && *c != '\n' && *c != '\t')
                || matches!(*c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect();
    assert!(
        raw.is_empty(),
        "{context}: expected no control character raw, found {raw:?} in\n{}",
        text.escape_debug()
    );
}

#[test]
fn the_installations_text_is_printed_as_any_text_not_the_tools_own() {
    let scenario = Scenario::start();
    let contact = "Dana\u{1b}[2J\u{1b}[31m Ops\u{202e}\nfake line";
    let mut body = resource_body_with_write(
        "devops",
        &scenario.db,
        &scenario.db.reader,
        &scenario.db.writer,
    );
    body["contact"] = json!(contact);
    scenario.register_body("devops", &body);
    let machine = Machine::new();
    machine.sign_in(&scenario.vm, &bea());
    let project = checked_project("needs-escaped-contact");
    project.write(
        "manifest.ts",
        &manifest_ts(
            "checked",
            "{ devops: { kind: \"postgres\", access: \"read\" } }",
        ),
    );
    pulled_by_hand(&project, "devops");

    let run = check(&machine, &project.dir);
    let report = Report::passed(&run);
    let warning = report.warnings_for("own-grant-below-declared");
    assert_eq!(warning.len(), 1, "{}", run.describe());
    assert_eq!(
        warning[0]["contact"],
        json!(contact),
        "with --json the text parses to the original"
    );
    assert_no_raw_control(&run.stdout, "`vm check --json`");

    let words = check_in_words(&machine, &project.dir).ok_text();
    assert_no_raw_control(&words, "`vm check` in words");
    let escaped = escapes_in(&words);
    for c in [0x1b, 0x202e, 0xa] {
        assert!(
            escaped.contains(&c),
            "expected U+{c:04X} of the contact printed as \\u{{<hex>}}, got\n{}",
            words.escape_debug()
        );
    }
}

#[test]
fn the_projects_text_is_printed_as_any_text_not_the_tools_own() {
    let (_vm, machine) = signed_in();
    let project = checked_project("needs-escaped-file");
    let odd = "src/odd\u{1b}[31mname.ts";
    project.write(odd, "export const wrong: number = \"text\";\n");
    let run = check(&machine, &project.dir);
    let report = Report::failed(&run);
    let problem = report.problem("types", "type-error");
    assert!(
        problem["file"].as_str().is_some_and(|f| f.ends_with(odd)),
        "expected the file's name, parsed, as it is: {problem}"
    );
    assert_no_raw_control(&run.stdout, "`vm check --json`");

    let words = check_in_words(&machine, &project.dir).text_error(1);
    assert_no_raw_control(&words, "`vm check` in words");
    assert!(
        escapes_in(&words).contains(&0x1b),
        "expected the escape character of the file's name printed as \\u{{1b}}, got\n{}",
        words.escape_debug()
    );
}

#[test]
fn vm_check_whose_standard_output_or_error_is_closed_ends_without_a_crash() {
    let (_vm, machine) = signed_in();
    let project = checked_project("needs-closed");
    let dir = project.dir.to_str().unwrap();
    let failing = checked_project("needs-closed-failing");
    failing.write("src/broken.ts", "export const wrong: number = \"text\";\n");
    let failing_dir = failing.dir.to_str().unwrap();
    for broken in [
        Broken::StdoutClosed,
        Broken::StdoutReaderGone,
        Broken::StderrClosed,
        Broken::BothClosed,
    ] {
        for (args, want) in [
            (vec!["check", "--project", dir], 0),
            (vec!["check", "--project", dir, "--json"], 0),
            (vec!["check", "--project", failing_dir], 1),
            (vec!["check", "--project", failing_dir, "--json"], 1),
        ] {
            let run = machine.run_broken(&args, broken);
            assert!(
                run.code == Some(want) || run.code == Some(1),
                "expected {want} or 1 with {broken:?}, got\n{}",
                run.describe()
            );
            for crash in ["panicked", "RUST_BACKTRACE", "stack backtrace"] {
                assert!(!run.stderr.contains(crash), "{}", run.describe());
            }
        }
    }
}

#[test]
fn the_command_line_is_looked_at_before_the_project_and_the_sign_in() {
    let machine = Machine::new();
    let empty = machine.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let run = machine.run_into_files(
        &check_args(empty.to_str().unwrap(), &["--timeout", "0"]),
        &[],
        CHECK_TIMEOUT,
    );
    assert_eq!(
        run.code,
        Some(2),
        "expected a wrong --timeout to be 2 before anything else\n{}",
        run.describe()
    );
}

#[test]
fn the_project_is_looked_at_before_the_sign_in() {
    let machine = Machine::new();
    let empty = machine.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    check(&machine, &empty).json_error(1, "not-a-project");
}

#[test]
fn the_sign_in_is_looked_at_before_node_and_the_packages() {
    let machine = Machine::new();
    let project = checked_project("needs-order-node");
    let empty = stand_ins(&machine.root().join("no-node"), &[]);
    machine
        .run_into_files(
            &check_args(project.dir.to_str().unwrap(), &[]),
            &[("PATH", empty.to_str().unwrap())],
            CHECK_TIMEOUT,
        )
        .json_error(4, "not-signed-in");
    // Not installed either: still 4.
    let bare = machine.root().join("bare");
    write_checked(&bare);
    check(&machine, &bare).json_error(4, "not-signed-in");
}
