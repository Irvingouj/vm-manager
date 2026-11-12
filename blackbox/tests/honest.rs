//! The honest path: a signed-in User reaches a fixture Application, which
//! sees exactly that User, through ctx.user() and through the raw host
//! interface (CONTRACT.md, "What an Application is handed", "The raw host
//! interface", "What the log records per request").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::net::TcpListener;
use std::time::Duration;

use blackbox::expect::{assert_refused, assert_user, assert_whoami};
use blackbox::records::{application_of, event, user_of};
use blackbox::{FIXTURE_VERSION, User, Vm};

fn vm() -> Vm {
    Vm::with_stand_in(&[("honest", "honest.js"), ("second", "honest.js")])
}

#[test]
fn user_a_gets_an_answer_naming_a_and_user_b_one_naming_b() {
    let vm = vm();
    let alice = User::new("alice").in_groups(&["staff"]);
    let bob = User::new("bob").in_groups(&["finance", "staff"]);
    let for_alice = vm.get_as(&alice, "honest", "/hello");
    for_alice.expect_status(200);
    assert_eq!(for_alice.text(), "Hello, alice! Your Groups: staff.");
    let for_bob = vm.get_as(&bob, "honest", "/hello");
    for_bob.expect_status(200);
    assert!(
        for_bob.text() == "Hello, bob! Your Groups: finance,staff."
            || for_bob.text() == "Hello, bob! Your Groups: staff,finance.",
        "expected the answer to name bob and his Groups, got {:?}",
        for_bob.text()
    );
}

#[test]
fn whoami_through_the_raw_host_interface_agrees_with_ctx_user() {
    let vm = vm();
    for user in [
        User::new("alice").in_groups(&["staff"]),
        User::new("bob").in_groups(&["finance", "staff"]),
        User::new("carol"),
    ] {
        let answer = vm.get_as(&user, "honest", "/");
        answer.expect_status(200);
        let body = answer.json();
        assert_user(&body["user"], &user, "ctx.user()");
        assert_whoami(
            &body["whoami"],
            &user,
            "honest",
            FIXTURE_VERSION,
            &answer.request_id(),
            "whoami",
        );
    }
}

#[test]
fn whoami_names_the_application_the_request_went_to() {
    let vm = vm();
    let alice = User::new("alice");
    let answer = vm.get_as(&alice, "second", "/");
    answer.expect_status(200);
    assert_whoami(
        &answer.json()["whoami"],
        &alice,
        "second",
        FIXTURE_VERSION,
        &answer.request_id(),
        "whoami in `second`",
    );
}

#[test]
fn ctx_and_the_user_it_returns_are_frozen() {
    let vm = vm();
    let answer = vm.get_as(&User::new("alice"), "honest", "/");
    answer.expect_status(200);
    let body = answer.json();
    assert_eq!(body["ctxFrozen"], true, "expected ctx to be frozen: {body}");
    assert_eq!(
        body["userFrozen"], true,
        "expected ctx.user() to be frozen: {body}"
    );
}

#[test]
fn the_application_is_handed_the_method_url_headers_and_body() {
    let vm = vm();
    let answer = vm
        .request("honest", "/some/path?x=1&y=two")
        .method("POST")
        .as_user(&User::new("alice"))
        .header("content-type", "text/plain")
        .header("x-custom", "custom value")
        .body("the request body")
        .send();
    answer.expect_status(200);
    let body = answer.json();
    assert_eq!(body["method"], "POST", "method in {body}");
    assert_eq!(
        body["url"], "http://honest.vm.test/some/path?x=1&y=two",
        "url in {body}"
    );
    assert_eq!(
        body["headers"]["x-custom"], "custom value",
        "headers in {body}"
    );
    assert_eq!(body["body"], "the request body", "body in {body}");
}

#[test]
fn the_request_record_names_the_request_application_version_user_method_path_and_status() {
    let vm = vm();
    let answer = vm.get_as(
        &User::new("alice").in_groups(&["staff"]),
        "honest",
        "/hello",
    );
    answer.expect_status(200);
    let records = vm.records_of(&answer);
    let record = &records.request;
    assert_eq!(
        record["request"].as_str(),
        Some(answer.request_id().as_str()),
        "request in {record}"
    );
    assert_eq!(
        application_of(record),
        Some("honest"),
        "application in {record}"
    );
    assert_eq!(
        record["version"].as_u64(),
        Some(FIXTURE_VERSION),
        "expected the `request` record to name the Version that served it, in {record}"
    );
    assert_eq!(user_of(record), Some("alice"), "user in {record}");
    assert_eq!(record["method"], "GET", "method in {record}");
    assert_eq!(record["path"], "/hello", "path in {record}");
    assert_eq!(record["status"].as_u64(), Some(200), "status in {record}");
}

#[test]
fn the_request_record_is_the_last_record_of_its_request() {
    let vm = vm();
    let answer = vm.get_as(&User::new("alice"), "honest", "/");
    answer.expect_status(200);
    let records = vm.records_of(&answer);
    assert!(
        records.host_calls().len() == 1 && records.printed().len() == 1,
        "expected one host-call and one printed record for the request, got {:?}",
        records.all
    );
    assert_eq!(
        records.all.last().map(event),
        Some("request"),
        "expected the `request` record last among the records of request {}, got {:?}",
        records.id,
        records.all
    );
}

#[test]
fn an_allowed_whoami_leaves_a_host_call_record() {
    let vm = vm();
    let answer = vm.get_as(&User::new("alice"), "honest", "/");
    answer.expect_status(200);
    let records = vm.records_of(&answer);
    let calls = records.host_calls();
    assert_eq!(
        calls.len(),
        1,
        "expected one host-call record, got {calls:?}"
    );
    let call = calls[0];
    assert_eq!(call["operation"], "whoami", "operation in {call}");
    assert_eq!(call["outcome"], "allowed", "outcome in {call}");
    assert_eq!(user_of(call), Some("alice"), "user in {call}");
    assert_eq!(
        application_of(call),
        Some("honest"),
        "application in {call}"
    );
}

#[test]
fn an_operation_the_host_does_not_know_is_refused_with_unknown_operation() {
    let vm = vm();
    let answer = vm.get_as(&User::new("alice"), "honest", "/unknown-operation");
    answer.expect_status(200);
    let body = answer.json();
    let resolved = &body["answer"]["resolved"];
    assert!(
        !resolved.is_null(),
        "expected invoke to resolve (not reject) for a refusal, got {body}"
    );
    assert_refused(
        resolved,
        "unknown-operation",
        "invoke(\"no-such-operation\")",
    );
    let records = vm.records_of(&answer);
    let calls = records.host_calls();
    assert_eq!(
        calls.len(),
        1,
        "expected one host-call record, got {calls:?}"
    );
    let call = calls[0];
    assert_eq!(
        call["operation"], "no-such-operation",
        "operation in {call}"
    );
    assert_eq!(call["outcome"], "refused", "outcome in {call}");
    assert_eq!(call["reason"], "unknown-operation", "reason in {call}");
    assert_eq!(user_of(call), Some("alice"), "user in {call}");
}

#[test]
fn an_outbound_fetch_from_application_code_rejects_and_connects_nowhere() {
    let vm = vm();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let target = format!("http://{}/", listener.local_addr().unwrap());
    let answer = vm.get_as(
        &User::new("alice"),
        "honest",
        &format!("/fetch?to={target}"),
    );
    answer.expect_status(200);
    let body = answer.json();
    assert!(
        body["fetch"]["rejected"].is_string(),
        "expected an outbound fetch to reject, got {body}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        listener.accept().is_err(),
        "expected no connection to {target} from Application code"
    );
}
