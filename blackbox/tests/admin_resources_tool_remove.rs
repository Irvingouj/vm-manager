//! The tool's `vibe admin resource location` and `vibe admin resource
//! remove` (CONTRACT.md, "Resources and Grants in full: The tool"): what
//! they check before they ask (exit code `2`, nothing asked), their words
//! and `--json`, their refusals (exit `3`), a `503` (exit `1`), the sign-in
//! (`4`), and `vibe admin --help` naming them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin::request_records;
use blackbox::postgres::{Platform, PrivatePostgres};
use blackbox::project::help_commands;
use blackbox::resources::{admin_resource, delete_resource};
use blackbox::scenario::{Scenario, administrator};
use blackbox::shared_database::token;
use blackbox::sql::Sql;
use blackbox::storage::{MARKETING, Storage, set_mode};
use blackbox::tool::{Machine, Run};
use blackbox::{User, Vm, VmOptions, assert_api_error};
use serde_json::{Value, json};

fn signed_in(vm: &Vm, user: &User) -> Machine {
    let machine = Machine::new();
    machine.sign_in(vm, user);
    machine
}

fn admin_run(machine: &Machine, args: &[&str], json: bool) -> Run {
    let mut all = vec!["admin"];
    all.extend_from_slice(args);
    if json {
        all.push("--json");
    }
    machine.run(&all)
}

#[test]
fn vm_admin_help_names_location_and_remove_and_remove_needs_yes() {
    let resource = Machine::new().run(&["admin", "resource", "--help"]);
    resource.expect_code(0);
    let subcommands = help_commands(&resource.stdout);
    for command in ["location", "remove"] {
        assert!(
            subcommands.iter().any(|c| c == command),
            "expected `vibe admin resource --help` to name `{command}`, got {subcommands:?}\n{}",
            resource.describe()
        );
    }
    for args in [
        vec!["admin", "resource", "location", "--help"],
        vec!["admin", "resource", "remove", "--help"],
    ] {
        let run = Machine::new().run(&args);
        run.expect_code(0);
        assert!(run.stdout.contains("--json"), "{}", run.describe());
    }
    let remove = Machine::new().run(&["admin", "resource", "remove", "--help"]);
    assert!(remove.stdout.contains("--yes"), "{}", remove.describe());
    let location = Machine::new().run(&["admin", "resource", "location", "--help"]);
    assert!(
        location.stdout.contains("--directory"),
        "{}",
        location.describe()
    );
    let admin = Machine::new().run(&["admin", "--help"]);
    admin.expect_code(0);
    assert!(
        admin.stdout.contains("resource"),
        "expected `vibe admin --help` to name the Resource commands\n{}",
        admin.describe()
    );
}

#[test]
fn each_needs_a_sign_in() {
    let machine = Machine::new();
    for args in [
        vec!["resource", "location", "marketing", "--directory", "/srv/x"],
        vec!["resource", "remove", "marketing", "--yes"],
    ] {
        admin_run(&machine, &args, true).json_error(4, "not-signed-in");
    }
}

#[test]
fn what_the_tool_checks_before_it_asks_is_exit_code_2_and_asks_nothing() {
    let s = Storage::ready();
    let machine = signed_in(&s.vm, &administrator());
    let dir = s.root().join("elsewhere");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.to_string_lossy().into_owned();
    // The commands well formed are asked and answered.
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "location without --directory",
            vec!["resource", "location", MARKETING],
        ),
        (
            "location with a relative --directory",
            vec![
                "resource",
                "location",
                MARKETING,
                "--directory",
                "relative/dir",
            ],
        ),
        (
            "location with a directory that is only a dot",
            vec!["resource", "location", MARKETING, "--directory", "."],
        ),
        (
            "location with a name no Resource could have",
            vec!["resource", "location", "Not_A_Name", "--directory", &dir],
        ),
        (
            "location with --directory twice",
            vec![
                "resource",
                "location",
                MARKETING,
                "--directory",
                &dir,
                "--directory",
                &dir,
            ],
        ),
        (
            "location with an option it does not have",
            vec![
                "resource",
                "location",
                MARKETING,
                "--directory",
                &dir,
                "--host",
                "db",
            ],
        ),
        (
            "location without a name",
            vec!["resource", "location", "--directory", &dir],
        ),
        (
            "remove without --yes",
            vec!["resource", "remove", MARKETING],
        ),
        (
            "remove with a name no Resource could have",
            vec!["resource", "remove", "Not_A_Name", "--yes"],
        ),
        (
            "remove with --yes twice",
            vec!["resource", "remove", MARKETING, "--yes", "--yes"],
        ),
        (
            "remove with an option it does not have",
            vec!["resource", "remove", MARKETING, "--yes", "--force"],
        ),
        ("remove without a name", vec!["resource", "remove", "--yes"]),
        (
            "remove with two names",
            vec!["resource", "remove", MARKETING, "other", "--yes"],
        ),
    ];
    for json in [false, true] {
        for (what, args) in &cases {
            let before = request_records(&s.vm);
            let run = admin_run(&machine, args, json);
            assert_eq!(
                run.code,
                Some(2),
                "expected exit code 2 for {what}: `vibe admin {}`{}\n{}",
                args.join(" "),
                if json { " --json" } else { "" },
                run.describe()
            );
            assert!(
                !run.stderr.trim().is_empty() && run.stdout.trim().is_empty(),
                "{what}: expected the usage text on standard error and nothing on standard output\n{}",
                run.describe()
            );
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(
                request_records(&s.vm),
                before,
                "{what}: expected nothing asked of the installation"
            );
        }
    }
    // Nothing happened to the Resource.
    admin_resource(&s.admin(), MARKETING).expect_status(200);
    assert_eq!(
        admin_resource(&s.admin(), MARKETING).json()["location"]["directory"],
        json!(s.dir(MARKETING).to_string_lossy())
    );
}

#[test]
fn location_prints_the_apis_answer_and_in_words_the_new_directory_and_that_files_stay() {
    let s = Storage::ready();
    let machine = signed_in(&s.vm, &administrator());
    let first = s.root().join("first");
    let second = s.root().join("second");
    for dir in [&first, &second] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let printed = admin_run(
        &machine,
        &[
            "resource",
            "location",
            MARKETING,
            "--directory",
            &first.to_string_lossy(),
        ],
        true,
    )
    .ok_json();
    assert_eq!(printed["resource"], json!(MARKETING), "{printed}");
    assert_eq!(printed["changed"], json!(true), "{printed}");
    assert_eq!(printed["files_moved"], json!(false), "{printed}");
    assert_eq!(
        printed["location"],
        json!({ "backend": "directory", "directory": first.to_string_lossy() }),
        "{printed}"
    );
    assert_eq!(
        admin_resource(&s.admin(), MARKETING).json()["location"],
        printed["location"],
        "the answer is the API's"
    );
    // The same again: the API's `changed: false`.
    let again = admin_run(
        &machine,
        &[
            "resource",
            "location",
            MARKETING,
            "--directory",
            &first.to_string_lossy(),
        ],
        true,
    )
    .ok_json();
    assert_eq!(again["changed"], json!(false), "{again}");
    // In words.
    let words = admin_run(
        &machine,
        &[
            "resource",
            "location",
            MARKETING,
            "--directory",
            &second.to_string_lossy(),
        ],
        false,
    )
    .ok_text();
    let lower = words
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(
        words.contains(&*second.to_string_lossy()),
        "expected the new directory in the words\n{words}"
    );
    assert!(
        lower.contains("not moved")
            || lower.contains("were not")
            || lower.contains("not been moved"),
        "expected the words to say that files were not moved\n{words}"
    );
    assert!(
        lower.contains("old director") && lower.contains("stay"),
        "expected the words to say that the old directory's files stay where they are\n{words}"
    );
}

#[test]
fn remove_prints_the_apis_answer_and_in_words_what_went_with_it() {
    let s = Scenario::writable();
    let machine = signed_in(&s.vm, &administrator());
    let printed = admin_run(&machine, &["resource", "remove", "devops", "--yes"], true).ok_json();
    assert_eq!(printed["name"], json!("devops"), "{printed}");
    assert_eq!(printed["grants_removed"], json!(2), "{printed}");
    assert_eq!(
        printed["declared_by"][0],
        json!({ "application": "board", "version": 1, "access": "write" }),
        "{printed}"
    );
    assert_eq!(printed["directory_cleaned"], Value::Null);
    assert_api_error(&delete_resource(&s.admin(), "devops"), 404, "not-found");

    // In words, for another Resource.
    s.register("second");
    s.grant("second", "readers", "read");
    s.install("watcher", "query.js", &[("second", "read")]);
    let words = admin_run(&machine, &["resource", "remove", "second", "--yes"], false).ok_text();
    let lower = words.to_lowercase();
    assert!(
        lower.contains("removed") && words.contains("second"),
        "expected the words to say that it is removed\n{words}"
    );
    assert!(
        words.contains('1') && lower.contains("grant"),
        "expected the words to say how many Grants went with it\n{words}"
    );
    assert!(
        words.contains("watcher"),
        "expected the words to name the Application that declares it\n{words}"
    );
    assert!(
        lower.contains("stopped") || lower.contains("not work") || lower.contains("refused"),
        "expected the words to say that the Applications that declare it are now stopped\n{words}"
    );
}

#[test]
fn remove_of_a_storage_resource_says_its_files_were_not_touched() {
    let s = Storage::ready();
    let machine = signed_in(&s.vm, &administrator());
    let words = admin_run(&machine, &["resource", "remove", MARKETING, "--yes"], false).ok_text();
    let lower = words.to_lowercase();
    assert!(
        lower.contains("files") && lower.contains("not touched"),
        "expected the words to say that a Storage Resource's files were not touched\n{words}"
    );
    assert!(
        words.contains("files") && words.contains("viewer"),
        "expected the Applications that declare it, by name\n{words}"
    );
    assert_api_error(&delete_resource(&s.admin(), MARKETING), 404, "not-found");
}

/// The removal of a Storage Resource whose directory the program cannot
/// clean (a directory it may not write to): `directory_cleaned` is false.
fn removed_with_an_uncleanable_directory(json: bool) -> Option<Run> {
    if blackbox::storage::my_uid() == 0 {
        return None;
    }
    let s = Storage::start();
    let dir = s.dir(MARKETING);
    std::fs::create_dir_all(&dir).unwrap();
    s.register_on(MARKETING, &dir);
    let machine = signed_in(&s.vm, &administrator());
    set_mode(&dir, 0o555);
    let run = admin_run(&machine, &["resource", "remove", MARKETING, "--yes"], json);
    set_mode(&dir, 0o755);
    assert!(
        dir.join(".vibe-resource").exists(),
        "the harness's own check: the marker could not be removed"
    );
    Some(run)
}

#[test]
fn remove_of_a_storage_resource_whose_directory_could_not_be_cleaned_says_so_in_words() {
    let Some(run) = removed_with_an_uncleanable_directory(false) else {
        return;
    };
    let words = run.ok_text();
    let lower = words.to_lowercase();
    assert!(
        lower.contains("files") && lower.contains("not touched"),
        "expected the words still to say that the files were not touched\n{words}"
    );
    assert!(
        words.contains(".vibe-resource")
            && lower.contains("left")
            && lower.contains("in use")
            && lower.contains("by hand"),
        "expected the words to say that `.vibe-resource` and the staging folder were left, and that the directory is in use for another name until `.vibe-resource` is removed by hand\n{words}"
    );
    for lie in [
        "marker was removed",
        "marker is removed",
        "directory was cleaned",
        "directory is cleaned",
        "can be registered again under any name",
    ] {
        assert!(
            !lower.contains(lie),
            "expected the words not to say that the directory was cleaned ({lie:?})\n{words}"
        );
    }
}

#[test]
fn remove_of_a_storage_resource_whose_directory_could_not_be_cleaned_prints_directory_cleaned_false()
 {
    let Some(run) = removed_with_an_uncleanable_directory(true) else {
        return;
    };
    let printed = run.ok_json();
    assert_eq!(printed["directory_cleaned"], json!(false), "{printed}");
}

#[test]
fn remove_of_a_database_resource_says_nothing_of_a_directory() {
    let s = Scenario::writable();
    let machine = signed_in(&s.vm, &administrator());
    let words = admin_run(&machine, &["resource", "remove", "devops", "--yes"], false).ok_text();
    let lower = words.to_lowercase();
    assert!(
        !lower.contains("director") && !lower.contains(".vibe") && !lower.contains("files"),
        "expected the words of a database's removal to say nothing of a directory or files\n{words}"
    );
}

#[test]
fn the_apis_refusals_keep_their_reasons_and_end_with_3() {
    let s = Storage::ready();
    s.register_postgres("devops");
    let machine = signed_in(&s.vm, &administrator());
    let t = token();
    let outside = format!("/etc/{t}");
    admin_run(
        &machine,
        &["resource", "location", MARKETING, "--directory", &outside],
        true,
    )
    .json_error(3, "outside-storage-root");
    admin_run(
        &machine,
        &["resource", "location", "ghost", "--directory", &outside],
        true,
    )
    .json_error(3, "not-found");
    let missing = s.root().join(format!("missing-{t}"));
    let message = admin_run(
        &machine,
        &[
            "resource",
            "location",
            MARKETING,
            "--directory",
            &missing.to_string_lossy(),
        ],
        true,
    )
    .json_error(3, "directory-missing");
    assert!(!message.contains(&t), "no path in a message: {message:?}");
    let dir = s.root().join("some");
    std::fs::create_dir_all(&dir).unwrap();
    admin_run(
        &machine,
        &[
            "resource",
            "location",
            "devops",
            "--directory",
            &dir.to_string_lossy(),
        ],
        true,
    )
    .json_error(3, "wrong-kind");
    admin_run(&machine, &["resource", "remove", "ghost", "--yes"], true).json_error(3, "not-found");
    // In words, a refusal is on standard error.
    admin_run(&machine, &["resource", "remove", "ghost", "--yes"], false).text_error(3);
    // Not an administrator.
    let alice = signed_in(&s.vm, &User::new("alice").in_groups(&["staff"]));
    admin_run(&alice, &["resource", "remove", MARKETING, "--yes"], true)
        .json_error(3, "not-an-administrator");
    admin_run(
        &alice,
        &[
            "resource",
            "location",
            MARKETING,
            "--directory",
            &dir.to_string_lossy(),
        ],
        true,
    )
    .json_error(3, "not-an-administrator");
    admin_resource(&s.admin(), MARKETING).expect_status(200);
}

/// Takes about 10 s: the write waits for the row's lock.
#[test]
fn a_503_is_exit_code_1() {
    let postgres = PrivatePostgres::start().expect("a Postgres of the test's own");
    let name = "vm_platform_tool_remove_503";
    let platform = Platform::on(&postgres, name);
    let s = Scenario::start_with(&VmOptions::new().platform(&platform));
    s.register("devops");
    let machine = signed_in(&s.vm, &administrator());
    let holder = Sql::connect(&postgres.url(name)).expect("the harness's own connection");
    holder.batch("BEGIN");
    holder.batch("SELECT id FROM resources WHERE name = 'devops' FOR UPDATE");
    let began = Instant::now();
    let run = admin_run(&machine, &["resource", "remove", "devops", "--yes"], true);
    holder.batch("ROLLBACK");
    run.json_error(1, "unavailable");
    assert!(
        began.elapsed() >= Duration::from_secs(9),
        "expected the write to wait 10 s for the row's lock, took {:?}",
        began.elapsed()
    );
    // Nothing was removed.
    admin_resource(&s.admin(), "devops").expect_status(200);
}
