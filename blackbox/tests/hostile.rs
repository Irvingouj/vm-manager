//! Hostile fixture Applications try to become another User or another
//! Application. Whatever they do, ctx.user() and the host report the real
//! User, in that request and in later requests by other Users (CONTRACT.md:
//! "The host decides who is asking from the request in flight, read on
//! entry. Nothing the Application passes, sets or overwrites changes that.")
//!
//! Each test checks what Application code saw and what the host says: its
//! `whoami` answers, and its `host-call` and `request` records.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::expect::{assert_user, assert_whoami, assert_whoami_names_no_one_else, is_user};
use blackbox::{Response, User, Vm};
use serde_json::Value;

fn vm() -> Vm {
    Vm::with_stand_in(&[
        ("hostile", "hostile.js"),
        ("hostile-at-load", "hostile-at-load.js"),
        ("payroll", "honest.js"),
    ])
}

fn alice() -> User {
    User::new("alice").in_groups(&["staff"])
}

fn bob() -> User {
    User::new("bob").in_groups(&["finance"])
}

/// Runs an attack route as alice and checks what the host recorded for it:
/// at least one host call, every one attributed to alice in `hostile`.
/// Returns the answer and its JSON body.
fn attack(vm: &Vm, path: &str) -> (Response, Value) {
    let response = vm.get_as(&alice(), "hostile", path);
    response.expect_status(200);
    let records = vm.records_of(&response);
    assert!(
        !records.host_calls().is_empty(),
        "expected the attack {path} to reach the host at least once, got {:?}",
        records.all
    );
    records.assert_host_saw_only("alice", "hostile");
    assert_eq!(
        blackbox::records::user_of(&records.request),
        Some("alice"),
        "user in the `request` record {}",
        records.request
    );
    let body = response.json();
    (response, body)
}

/// After an attack: other Users, and alice again, are still exactly
/// themselves, in this Application and in another, and the log kept every
/// request's records to that request.
fn assert_later_requests_are_still_themselves(vm: &Vm) {
    for (user, application) in [(bob(), "hostile"), (alice(), "hostile"), (bob(), "payroll")] {
        let path = if application == "hostile" {
            "/plain"
        } else {
            "/"
        };
        let response = vm.get_as(&user, application, path);
        response.expect_status(200);
        let body = response.json();
        let context = format!("a later request by {} to {application}", user.id);
        assert_user(&body["user"], &user, &context);
        let whoami = if application == "hostile" {
            &body["whoami"]["answer"]
        } else {
            &body["whoami"]
        };
        assert_whoami(whoami, &user, application, &response.request_id(), &context);
        vm.records_of(&response)
            .assert_host_saw_only(&user.id, application);
    }
    vm.assert_log_keeps_each_request_to_itself();
}

#[test]
fn claiming_another_user_through_globals_and_arguments_changes_nothing() {
    let vm = vm();
    let (response, body) = attack(&vm, "/claim-user");
    let id = response.request_id();
    assert_user(&body["user"], &alice(), "ctx.user() after the claims");
    assert_whoami(
        &body["whoami"]["answer"],
        &alice(),
        "hostile",
        &id,
        "whoami after the claims",
    );
    let answers = body["answers"].as_object().expect("answers");
    assert!(
        answers.len() >= 10,
        "expected every claim to be tried, got {body}"
    );
    for (claim, answer) in answers {
        assert_whoami_names_no_one_else(
            &answer["answer"],
            &alice(),
            "hostile",
            &id,
            &format!("whoami with {claim}"),
        );
    }
    assert_later_requests_are_still_themselves(&vm);
}

#[test]
fn claiming_another_application_changes_nothing() {
    let vm = vm();
    let (response, body) = attack(&vm, "/claim-application");
    let id = response.request_id();
    assert_user(&body["user"], &alice(), "ctx.user() after the claims");
    assert_whoami(
        &body["whoami"]["answer"],
        &alice(),
        "hostile",
        &id,
        "whoami after the claims",
    );
    for (claim, answer) in body["answers"].as_object().expect("answers") {
        assert_whoami_names_no_one_else(
            &answer["answer"],
            &alice(),
            "hostile",
            &id,
            &format!("whoami through {claim}"),
        );
    }
    assert_later_requests_are_still_themselves(&vm);
}

#[test]
fn replacing_redefining_or_detaching_ctx_user_never_yields_another_user() {
    let vm = vm();
    let (response, body) = attack(&vm, "/replace-ctx");
    assert_eq!(
        body["ctxFrozen"], true,
        "expected ctx to stay frozen: {body}"
    );
    assert_user(&body["user"], &alice(), "ctx.user() after the attempts");
    assert_user(
        &body["readings"]["ctx.user()"]["answer"],
        &alice(),
        "ctx.user() while Object.prototype.user is forged",
    );
    for (how, reading) in body["readings"].as_object().expect("readings") {
        if let Some(answer) = reading.get("answer") {
            assert!(
                is_user(answer, &alice()),
                "expected ctx.user() called {how} either to throw or to return alice, got {reading}"
            );
        }
    }
    assert_whoami(
        &body["whoami"]["answer"],
        &alice(),
        "hostile",
        &response.request_id(),
        "whoami after the attempts",
    );
    assert_later_requests_are_still_themselves(&vm);
}

#[test]
fn a_ctx_forged_by_the_application_does_not_fool_the_host() {
    let vm = vm();
    let (response, body) = attack(&vm, "/forge-ctx");
    let id = response.request_id();
    assert_whoami(
        &body["inner"]["whoami"]["answer"],
        &alice(),
        "hostile",
        &id,
        "whoami inside the handler called with a forged ctx",
    );
    if let Some(answer) = body["created"].get("answer") {
        assert!(
            is_user(answer, &alice()),
            "expected user() on an object made from ctx to throw or return alice, got {body}"
        );
    }
    assert_user(&body["user"], &alice(), "the real ctx.user()");
    assert_whoami(
        &body["whoami"]["answer"],
        &alice(),
        "hostile",
        &id,
        "whoami",
    );
    assert_later_requests_are_still_themselves(&vm);
}

#[test]
fn changing_the_returned_user_object_changes_nothing() {
    let vm = vm();
    let (response, body) = attack(&vm, "/change-user");
    assert_eq!(
        body["userFrozen"], true,
        "expected the User object to be frozen: {body}"
    );
    assert_eq!(
        body["changedUserId"], "alice",
        "expected the User's id to stay alice: {body}"
    );
    assert_user(
        &body["again"],
        &alice(),
        "ctx.user() called again after changing the first answer",
    );
    assert_whoami(
        &body["whoami"]["answer"],
        &alice(),
        "hostile",
        &response.request_id(),
        "whoami after the changes",
    );
    assert_later_requests_are_still_themselves(&vm);
}

#[test]
fn overwriting_globals_and_built_ins_during_the_handler_changes_nothing() {
    let vm = vm();
    let (_, body) = attack(&vm, "/tamper");
    assert!(
        body["userError"].is_null(),
        "expected ctx.user() to work while built-ins are overwritten: {body}"
    );
    assert_user(
        &body["user"],
        &alice(),
        "ctx.user() with globals and built-ins overwritten",
    );
    assert_eq!(
        body["ctxFrozen"], true,
        "expected ctx to stay frozen with Object.freeze overwritten: {body}"
    );
    assert_eq!(
        body["userFrozen"], true,
        "expected ctx.user() frozen with Object.freeze overwritten: {body}"
    );
    // What whoami answers is read here through the Application's own
    // overwritten built-ins, so it is not checked; the host's records (in
    // `attack`) are.
    assert_later_requests_are_still_themselves(&vm);
}

#[test]
fn overwriting_globals_and_built_ins_before_the_handler_changes_nothing() {
    let vm = vm();
    for user in [alice(), bob(), User::new("carol")] {
        let response = vm.get_as(&user, "hostile-at-load", "/");
        response.expect_status(200);
        let body = response.json();
        let context = format!("{} with globals and built-ins overwritten at load", user.id);
        assert!(
            body["userError"].is_null(),
            "{context}: expected ctx.user() to work: {body}"
        );
        assert_user(&body["user"], &user, &context);
        assert_eq!(
            body["ctxFrozen"], true,
            "{context}: expected ctx frozen: {body}"
        );
        assert_eq!(
            body["userFrozen"], true,
            "{context}: expected ctx.user() frozen: {body}"
        );
        let records = vm.records_of(&response);
        assert!(
            !records.host_calls().is_empty(),
            "{context}: expected the handler's whoami to reach the host, got {:?}",
            records.all
        );
        records.assert_host_saw_only(&user.id, "hostile-at-load");
    }
    assert_later_requests_are_still_themselves(&vm);
}

#[test]
fn replacing_the_handler_during_a_request_does_not_reach_the_next_request() {
    let vm = vm();
    let (response, body) = attack(&vm, "/replace-default");
    assert_user(&body["user"], &alice(), "ctx.user()");
    assert_whoami(
        &body["whoami"]["answer"],
        &alice(),
        "hostile",
        &response.request_id(),
        "whoami",
    );
    let next = vm.get_as(&bob(), "hostile", "/plain");
    next.expect_status(200);
    assert!(
        !next.text().contains("forged"),
        "expected bob's request to be served by the registered handler, not the replacement: {}",
        next.text()
    );
    assert_later_requests_are_still_themselves(&vm);
}
