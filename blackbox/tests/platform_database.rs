//! The program keeps what it must remember in its platform database
//! (CONTRACT.md, "The platform database"): it creates the database when it
//! does not exist, a restart against the same one loses nothing, and two
//! programs on one Postgres with different platform databases do not see
//! each other.
//!
//! "A newer program upgrades an older platform database in place" cannot be
//! shown from outside with one build of the program; what can be shown is
//! that starting again on a database the program itself wrote, which runs
//! the same start-up path, changes nothing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::api::{assert_served_by, marked_bundle};
use blackbox::{Platform, User, Vm, VmOptions, assert_api_error};
use serde_json::Value;

fn alice() -> User {
    User::new("alice")
}

fn bob() -> User {
    User::new("bob")
}

/// Everything the API says about an Application: its record, its list of
/// Versions, and each Version's record.
fn everything_about(vm: &Vm, name: &str) -> (Value, Vec<Value>, Vec<Value>) {
    let api = vm.api_as(&bob());
    let application = api.application(name);
    let listed = api.versions(name);
    let each = (1..=u64::try_from(listed.len()).unwrap())
        .map(|n| api.get_version(name, n).expect_status(200).json())
        .collect();
    (application, listed, each)
}

#[test]
fn against_a_platform_database_that_does_not_exist_the_program_creates_it_and_serves() {
    let platform = Platform::fresh();
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in().platform(&platform));
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("first"));
    let response = vm.get_as(&bob(), "board", "/");
    assert_served_by(&vm, &response, "board", "first", 1);
}

#[test]
fn a_restart_keeps_applications_contributors_versions_and_the_serving_version() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    let api = vm.api_as(&alice());
    api.create_with_bundle("board", &marked_bundle("board-1"));
    api.send_bundle("board", &marked_bundle("board-2"))
        .expect_status(201);
    vm.api_as(&bob())
        .create_with_bundle("ledger", &marked_bundle("ledger-1"));
    api.create_application("empty").expect_status(201);
    let before: Vec<_> = ["board", "ledger", "empty"]
        .iter()
        .map(|name| everything_about(&vm, name))
        .collect();

    let vm = vm.restart();
    let after: Vec<_> = ["board", "ledger", "empty"]
        .iter()
        .map(|name| everything_about(&vm, name))
        .collect();
    for (name, (before, after)) in ["board", "ledger", "empty"]
        .iter()
        .zip(before.iter().zip(after.iter()))
    {
        assert_eq!(
            before, after,
            "expected {name:?} (its record, its list of Versions and each Version) to be as it was before the restart"
        );
    }
    // Which Version serves is kept, and serves.
    assert_served_by(&vm, &vm.get_as(&bob(), "board", "/"), "board", "board-2", 2);
    assert_served_by(
        &vm,
        &vm.get_as(&alice(), "ledger", "/"),
        "ledger",
        "ledger-1",
        1,
    );
    vm.get_as(&alice(), "empty", "/").expect_status(404);
    // Contributors are kept: alice still may send to board, and only she.
    assert_api_error(
        &vm.api_as(&bob())
            .send_bundle("board", &marked_bundle("bob")),
        403,
        "not-a-contributor",
    );
    let third = vm
        .api_as(&alice())
        .send_bundle("board", &marked_bundle("board-3"));
    third.expect_status(201);
    assert_eq!(
        third.json()["number"],
        3,
        "expected numbering to go on from where it was before the restart, got {}",
        third.json()
    );
    assert_served_by(&vm, &vm.get_as(&bob(), "board", "/"), "board", "board-3", 3);
    // And names stay taken.
    assert_api_error(
        &vm.api_as(&bob()).create_application("board"),
        409,
        "name-taken",
    );
}

#[test]
fn starting_again_and_again_on_the_same_platform_database_changes_nothing() {
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in());
    vm.api_as(&alice())
        .create_with_bundle("board", &marked_bundle("only"));
    let before = everything_about(&vm, "board");
    let mut vm = vm;
    for _ in 0..3 {
        vm = vm.restart();
        assert_eq!(
            everything_about(&vm, "board"),
            before,
            "expected another start on the same platform database to change nothing"
        );
    }
    assert_served_by(&vm, &vm.get_as(&bob(), "board", "/"), "board", "only", 1);
}

#[test]
fn a_platform_database_whose_name_needs_quoting_is_created_and_kept() {
    let platform = Platform::named(&format!("Vm-Platform-{}", Platform::fresh().name()));
    let vm = Vm::start(&VmOptions::new().stand_in_sign_in().platform(&platform));
    vm.api_as(&alice())
        .create_with_bundle("board", &marked_bundle("quoted"));
    let vm = vm.restart();
    assert_served_by(&vm, &vm.get_as(&bob(), "board", "/"), "board", "quoted", 1);
}

#[test]
fn two_programs_with_different_platform_databases_on_one_postgres_do_not_see_each_other() {
    let first = Vm::start(&VmOptions::new().stand_in_sign_in());
    let second = Vm::start(&VmOptions::new().stand_in_sign_in());
    assert_ne!(first.platform().name(), second.platform().name());
    first
        .api_as(&alice())
        .create_with_bundle("board", &marked_bundle("in-first"));
    first
        .api_as(&alice())
        .create_application("only-first")
        .expect_status(201);

    assert_api_error(
        &second.api_as(&alice()).get_application("board"),
        404,
        "not-found",
    );
    assert_api_error(
        &second.api_as(&alice()).list_versions("board"),
        404,
        "not-found",
    );
    assert_api_error(
        &second.api_as(&alice()).get_application("only-first"),
        404,
        "not-found",
    );
    second.get_as(&alice(), "board", "/").expect_status(404);
    // The same name is free in the other installation, and each serves its own.
    second
        .api_as(&bob())
        .create_with_bundle("board", &marked_bundle("in-second"));
    assert_served_by(
        &first,
        &first.get_as(&bob(), "board", "/"),
        "board",
        "in-first",
        1,
    );
    assert_served_by(
        &second,
        &second.get_as(&alice(), "board", "/"),
        "board",
        "in-second",
        1,
    );
    assert_eq!(
        first.api_as(&bob()).application("board")["created_by"],
        "alice"
    );
    assert_eq!(
        second.api_as(&alice()).application("board")["created_by"],
        "bob"
    );
}
