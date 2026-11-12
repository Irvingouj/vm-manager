//! When Application code fails, the User gets the program's fixed 500 with
//! no message and no stack, the `uncaught` record holds both, and the
//! program goes on serving (CONTRACT.md, "Responses the program itself
//! gives", "What the log records per request").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::records::{application_of, user_of};
use blackbox::{Response, User, Vm};

fn vm() -> Vm {
    Vm::with_stand_in(&[
        ("errors", "errors.js"),
        ("broken", "throws-at-load.js"),
        ("honest", "honest.js"),
    ])
}

fn alice() -> User {
    User::new("alice")
}

/// The words of a failure that must never reach the User: the marker in
/// the message, the function in the stack, and stack-like text.
fn assert_body_reveals_nothing(response: &Response, secrets: &[&str]) {
    let body = response.text();
    for secret in secrets
        .iter()
        .chain(["the secret message", "\n    at ", ".js:"].iter())
    {
        assert!(
            !body.contains(secret),
            "expected the 500 body to hold neither the error's message nor its stack, but it contains {secret:?}:\n{}",
            response.describe()
        );
    }
}

/// Requests `path` of `application` as alice, and fails unless the answer is
/// the 500 that reveals nothing and the request has an `uncaught` record
/// holding all of `in_record`. Returns the 500 body.
fn assert_failure_is_caught(
    vm: &Vm,
    application: &str,
    path: &str,
    secrets: &[&str],
    in_record: &[&str],
) -> Vec<u8> {
    let response = vm.get_as(&alice(), application, path);
    response.expect_status(500);
    assert_body_reveals_nothing(&response, secrets);
    let records = vm.records_of(&response);
    let uncaught = records.uncaught();
    assert_eq!(
        uncaught.len(),
        1,
        "expected one `uncaught` record for {application}{path}, got {:?}",
        records.all
    );
    let record = uncaught[0];
    assert_eq!(user_of(record), Some("alice"), "user in {record}");
    assert_eq!(
        application_of(record),
        Some(application),
        "application in {record}"
    );
    let error = record["error"].to_string();
    for wanted in in_record {
        assert!(
            error.contains(wanted),
            "expected the `uncaught` record's error to hold {wanted:?}, got {record}"
        );
    }
    assert_eq!(
        records.request["status"].as_u64(),
        Some(500),
        "status in {}",
        records.request
    );
    response.body().to_vec()
}

fn assert_still_serving(vm: &Vm) {
    let ok = vm.get_as(&User::new("bob"), "errors", "/ok");
    ok.expect_status(200);
    assert_eq!(ok.text(), "ok");
    let honest = vm.get_as(&User::new("bob"), "honest", "/hello");
    honest.expect_status(200);
}

#[test]
fn a_synchronous_throw_gives_the_fixed_500_and_an_uncaught_record_with_message_and_stack() {
    let vm = vm();
    assert_failure_is_caught(
        &vm,
        "errors",
        "/throw",
        &["SECRET-SYNC-7f3a", "failInsideTheNamedFunctionForTheStack"],
        &["SECRET-SYNC-7f3a", "failInsideTheNamedFunctionForTheStack"],
    );
    assert_still_serving(&vm);
}

#[test]
fn a_throw_after_an_await_gives_the_fixed_500_and_an_uncaught_record_with_message_and_stack() {
    let vm = vm();
    assert_failure_is_caught(
        &vm,
        "errors",
        "/async-throw",
        &["SECRET-ASYNC-91c2", "failInsideTheNamedFunctionForTheStack"],
        &["SECRET-ASYNC-91c2", "failInsideTheNamedFunctionForTheStack"],
    );
    assert_still_serving(&vm);
}

#[test]
fn a_rejected_promise_gives_the_fixed_500_and_an_uncaught_record_with_the_message() {
    let vm = vm();
    assert_failure_is_caught(
        &vm,
        "errors",
        "/reject",
        &["SECRET-REJECT-4d0e"],
        &["SECRET-REJECT-4d0e"],
    );
    assert_still_serving(&vm);
}

#[test]
fn a_thrown_string_gives_the_fixed_500_and_an_uncaught_record_with_the_string() {
    let vm = vm();
    assert_failure_is_caught(
        &vm,
        "errors",
        "/throw-string",
        &["SECRET-STRING-55ab"],
        &["SECRET-STRING-55ab"],
    );
    assert_still_serving(&vm);
}

#[test]
fn an_answer_that_is_not_a_response_gives_the_fixed_500_and_an_uncaught_record() {
    let vm = vm();
    for (path, secret) in [
        ("/not-response", "SECRET-OBJECT-2b8f"),
        ("/string", "SECRET-TEXT-a17c"),
        ("/nothing", "the secret message"),
    ] {
        assert_failure_is_caught(&vm, "errors", path, &[secret], &[]);
    }
    assert_still_serving(&vm);
}

#[test]
fn a_throw_while_loading_gives_the_fixed_500_and_an_uncaught_record_with_message_and_stack() {
    let vm = vm();
    assert_failure_is_caught(
        &vm,
        "broken",
        "/",
        &["SECRET-LOAD-c3d9", "failWhileLoadingInTheNamedFunction"],
        &["SECRET-LOAD-c3d9", "failWhileLoadingInTheNamedFunction"],
    );
    // It fails the same way every time, and the others are served.
    assert_failure_is_caught(
        &vm,
        "broken",
        "/again",
        &["SECRET-LOAD-c3d9"],
        &["SECRET-LOAD-c3d9"],
    );
    assert_still_serving(&vm);
}

#[test]
fn the_500_body_is_the_same_whatever_failed() {
    let vm = vm();
    let bodies: Vec<(&str, Vec<u8>)> = [
        ("errors", "/throw"),
        ("errors", "/async-throw"),
        ("errors", "/reject"),
        ("errors", "/throw-string"),
        ("errors", "/not-response"),
        ("broken", "/"),
    ]
    .iter()
    .map(|(application, path)| {
        let response = vm.get_as(&alice(), application, path);
        response.expect_status(500);
        (*path, response.body().to_vec())
    })
    .collect();
    let (first_path, first) = &bodies[0];
    for (path, body) in &bodies {
        assert_eq!(
            body,
            first,
            "expected the same fixed 500 body for {path} as for {first_path}: {:?} vs {:?}",
            String::from_utf8_lossy(body),
            String::from_utf8_lossy(first)
        );
    }
}
