//! The Dashboard while the program cannot tell what became of a write
//! (CONTRACT.md, "The Dashboard: The list" on `unknown`, "Sharing from it"
//! on `503` `unavailable`, and "A write whose answer is lost"), with the
//! stand-in sign-in, on a platform database on a Postgres of each test's
//! own whose commits the harness can hold (`blackbox::lost_answer`).
//!
//! These tests wait once each for the program's limit on a write (20 s).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;

use blackbox::dashboard::{Dashboard, Page, assert_refused, create, deploy, stand_in_options};
use blackbox::lost_answer::{HeldPostgres, LOST_WRITE_TIMEOUT, quickly};
use blackbox::scenario::{CONTACT, Scenario, administrator};
use blackbox::{User, Vm, assert_api_error};
use serde_json::json;

const APP: &str = "board";

fn alice() -> User {
    User::new("alice").in_groups(&["crew"])
}

/// (yours, standing, ask) of a Resource of `board` on alice's Dashboard.
fn standing(vm: &Vm, resource: &str) -> (String, String, Option<String>) {
    let page = Dashboard::new(vm, &alice()).list();
    let shown = page.listed(APP).resource(resource).clone();
    (
        shown.yours.unwrap_or_default(),
        shown.standing.unwrap_or_default(),
        shown.ask,
    )
}

#[test]
fn while_a_grant_cannot_be_told_its_standing_is_unknown_and_then_what_the_database_has() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_dashboard_lost_grant");
    let s = Scenario::start_with(&stand_in_options().platform(&platform));
    s.register("crewdb");
    s.register("steady");
    s.grant("crewdb", "crew", "read");
    s.grant("steady", "crew", "read");
    let vm = &s.vm;
    create(vm, &alice(), APP, None);
    deploy(vm, &alice(), APP, &[("crewdb", "read"), ("steady", "read")]);
    assert_eq!(
        standing(vm, "crewdb").1,
        "enough",
        "the harness's own check: crew holds read on crewdb"
    );

    postgres.hold();
    let lower = vm
        .api_as(&administrator())
        .request("/v1/resources/crewdb/grants")
        .method("POST")
        .json(&json!({ "group": "crew", "access": "structure" }))
        .timeout(LOST_WRITE_TIMEOUT);
    let lowered = thread::scope(|scope| {
        let lowered = scope.spawn(move || lower.try_send());
        postgres.wait_for_waiting(1, "the Grant lowered");
        lowered.join().unwrap()
    });
    let lowered =
        lowered.unwrap_or_else(|e| panic!("expected `503` for the Grant lowered, no answer ({e})"));
    assert_api_error(&lowered, 503, "unavailable");
    postgres.assert_waiting(1, "after the Grant's `503`");

    // Answered `200` within the bound of the API's reading, never held
    // until the write is settled.
    let dashboard = Dashboard::new(vm, &alice());
    let answer = quickly(
        vm,
        &dashboard.navigation("GET", "/"),
        "the list while the Grants on crewdb cannot be told",
    );
    assert_eq!(
        answer.status,
        200,
        "expected the list answered `200` all the same while the Grants on crewdb cannot be told\n{}",
        answer.describe()
    );
    let page = Page::read(answer, dashboard.url("/"), "the list meanwhile");
    let board = page.listed(APP);
    let crewdb = board.resource("crewdb");
    assert_eq!(
        (crewdb.yours.as_deref(), crewdb.standing.as_deref()),
        (Some("unknown"), Some("unknown")),
        "expected `data-vm-yours` and `data-vm-standing` `unknown` while the program cannot tell the Grants on crewdb\n{}",
        page.describe()
    );
    assert!(
        crewdb.text.to_lowercase().contains("now"),
        "expected words saying that it cannot be told just now, got {:?}",
        crewdb.text
    );
    let steady = board.resource("steady");
    assert_eq!(
        (steady.yours.as_deref(), steady.standing.as_deref()),
        (Some("read"), Some("enough")),
        "expected another Resource shown as ever meanwhile\n{}",
        page.describe()
    );

    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
    assert_eq!(
        standing(vm, "crewdb"),
        (
            "structure".to_owned(),
            "lower".to_owned(),
            Some(CONTACT.to_owned())
        ),
        "expected the Grant the database has once the write is settled, at the next request"
    );
}

#[test]
fn a_change_of_sharing_whose_answer_is_lost_is_refused_503_unavailable() {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform("vm_platform_dashboard_lost_share");
    let vm = Vm::start(&stand_in_options().platform(&platform));
    create(&vm, &alice(), APP, None);
    let dashboard = Dashboard::new(&vm, &alice());
    let request = dashboard
        .share_request(APP, "user", "bob")
        .timeout(LOST_WRITE_TIMEOUT);

    postgres.hold();
    let answer = thread::scope(|scope| {
        let answer = scope.spawn(move || request.try_send());
        postgres.wait_for_waiting(1, "the share");
        answer.join().unwrap()
    });
    let answer =
        answer.unwrap_or_else(|e| panic!("expected an answer to the share, none came ({e})"));
    assert_refused(
        answer,
        &dashboard.url("/"),
        503,
        "unavailable",
        APP,
        "a share whose commit is still on its way when the program stops waiting",
    );
    postgres.let_through();
    postgres.wait_until_none_waiting("once commits go through");
}
