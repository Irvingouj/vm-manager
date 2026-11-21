//! A disable or an enable whose answer is lost (CONTRACT.md, "What
//! Platform administrators see and stop: A write whose answer is lost",
//! and "A write whose answer is lost: The rule"), on a platform database on
//! a Postgres of each test's own whose commits the harness holds
//! ([`HeldPostgres`]): the disable is answered `503` `unavailable`;
//! meanwhile every request to that Application's address is `503` with
//! `retry-after`, at once, and none of its code runs; once the commit goes
//! through, requests are answered as the database says (disabled, or
//! served). Nobody is served, and nobody told it is disabled, on a guess.
//!
//! Each test takes about half a minute (the program's limit for a write,
//! then a few seconds for the write to be settled).

use std::thread;
use std::time::{Duration, Instant};

use blackbox::admin::{self, administrator, assert_disabled_plain, disable_ok};
use blackbox::lost_answer::{HeldPostgres, QUICK, SETTLE, assert_503_retry};
use blackbox::sharing::TRACED;
use blackbox::{Response, User, Vm, assert_api_error, fixture_bundle};
use serde_json::json;

fn bob() -> User {
    User::new("bob")
}

/// A held Postgres, and the program on a platform database there with
/// `board` (`fixtures/honest.js`) shared with everyone.
fn with_board(name: &str) -> (HeldPostgres, Vm) {
    let postgres = HeldPostgres::start();
    let platform = postgres.platform(name);
    let vm = Vm::start(&admin::options().platform(&platform));
    vm.api_as(&User::new("alice"))
        .create_with_bundle("board", &fixture_bundle(TRACED));
    (postgres, vm)
}

fn open(vm: &Vm) -> Response {
    vm.request("board", "/hello")
        .as_user(&bob())
        .header("accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send()
}

/// `open`, which must be answered within [`QUICK`].
fn open_quickly(vm: &Vm, context: &str) -> Response {
    let began = Instant::now();
    let response = open(vm);
    assert!(
        began.elapsed() < QUICK,
        "{context}: expected an answer at once, it took {:?}\n{}",
        began.elapsed(),
        response.describe()
    );
    response
}

/// Sends `action` as the administrator while commits are held: answered
/// `503` `unavailable`.
fn lost(vm: &Vm, action: &str) {
    let answer = vm
        .api_as(&administrator())
        .request(&format!("/v1/applications/board/{action}"))
        .method("POST")
        .json(&json!({}))
        .timeout(Duration::from_secs(60))
        .send();
    assert_api_error(&answer, 503, "unavailable");
}

/// For a few seconds: every request is the `503` with `retry-after`, at
/// once, and none of the Application's code runs.
fn meanwhile(vm: &Vm, context: &str) {
    for round in 0..5 {
        let response = open_quickly(vm, &format!("{context}, round {round}"));
        assert_503_retry(&response, &format!("{context}, round {round}"));
        vm.records_of(&response).assert_no_application_code_ran();
        thread::sleep(Duration::from_millis(300));
    }
}

#[test]
fn a_disable_whose_answer_is_lost_serves_nobody_on_a_guess_and_settles_disabled() {
    let (postgres, vm) = with_board("vm_platform_lost_disable");
    open(&vm).expect_status(200);
    postgres.hold();
    lost(&vm, "disable");
    assert!(
        postgres.waiting() > 0,
        "the disable's commit is still on its way"
    );
    meanwhile(&vm, "while the disable's commit waits");
    postgres.let_through();
    let deadline = Instant::now() + SETTLE;
    loop {
        let response = open_quickly(&vm, "once commits go through");
        assert!(
            response.status == 503,
            "expected `503` until settled and then the disabled answer; never served\n{}",
            response.describe()
        );
        if response.header("retry-after").is_none() {
            assert_disabled_plain(&response, "board", "settled");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected the disable settled within {SETTLE:?}\n{}",
            response.describe()
        );
        thread::sleep(Duration::from_millis(200));
    }
    let read = vm.api_as(&administrator()).application("board");
    assert_eq!(read["disabled"]["by"], administrator().id, "{read}");
}

#[test]
fn an_enable_whose_answer_is_lost_is_not_refused_for_ever_and_settles_served() {
    let (postgres, vm) = with_board("vm_platform_lost_enable");
    disable_ok(&vm, "board");
    postgres.hold();
    lost(&vm, "enable");
    meanwhile(&vm, "while the enable's commit waits");
    postgres.let_through();
    let deadline = Instant::now() + SETTLE;
    loop {
        let response = open_quickly(&vm, "once commits go through");
        if response.status == 200 {
            assert!(
                response.text().contains("Hello, bob!"),
                "{}",
                response.describe()
            );
            break;
        }
        assert_503_retry(&response, "until the enable is settled");
        assert!(
            Instant::now() < deadline,
            "expected the enable settled within {SETTLE:?}\n{}",
            response.describe()
        );
        thread::sleep(Duration::from_millis(200));
    }
    assert!(vm.api_as(&administrator()).application("board")["disabled"].is_null());
}
