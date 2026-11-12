//! A host call with no request in flight is refused with `no-request`: at
//! the top level while loading, and from code left running after its
//! request. Such code never gets an allowed host call and never sees
//! another request's User (CONTRACT.md, "The raw host interface").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::Duration;

use blackbox::expect::{assert_refused, assert_user, assert_whoami};
use blackbox::records::{application_of, describe, event, request_of, user_of};
use blackbox::{Response, User, Vm};
use serde_json::Value;

/// Long enough for every timer the leftover code arms (the last at 700 ms)
/// to have fired, if it ever runs.
const LEFTOVER_TIME: Duration = Duration::from_millis(1200);

#[test]
fn a_host_call_at_the_top_level_while_loading_is_refused_with_no_request() {
    let vm = Vm::with_stand_in(&[("calls-at-load", "calls-at-load.js")]);
    let alice = User::new("alice");
    let response = vm.get_as(&alice, "calls-at-load", "/");
    response.expect_status(200);
    let body = response.json();
    assert_user(&body["user"], &alice, "ctx.user() in the handler");
    let at_load = &body["atLoad"]["answered"];
    assert!(
        !at_load.is_null(),
        "expected the call at load to resolve with a refusal (not reject or throw), got {body}"
    );
    assert_refused(at_load, "no-request", "whoami at the top level");
    let record = vm.wait_for_record(
        "a refused `host-call` record with no request, from calls-at-load",
        |r| {
            event(r) == "host-call"
                && application_of(r) == Some("calls-at-load")
                && r["outcome"] == "refused"
        },
    );
    assert!(
        request_of(&record).is_none(),
        "expected `request: null` in {record}"
    );
    assert!(
        user_of(&record).is_none(),
        "expected `user: null` in {record}"
    );
    assert_eq!(record["reason"], "no-request", "reason in {record}");
    assert_eq!(record["operation"], "whoami", "operation in {record}");
    vm.assert_log_keeps_each_request_to_itself();
}

/// What the leftover code printed, with where it stands in the log.
fn leftover_prints(vm: &Vm) -> Vec<(usize, Value)> {
    vm.records()
        .into_iter()
        .enumerate()
        .filter(|(_, r)| {
            event(r) == "printed"
                && r["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("LEFTOVER"))
        })
        .collect()
}

/// Fails if leftover code, once its request was over, got a User from
/// ctx.user() or an allowed whoami. Prints made while the request was still
/// in flight (they carry its id and come before its `request` record) may
/// show its own User.
fn assert_leftover_code_got_no_one(vm: &Vm, leave_behind: &Response) {
    let id = leave_behind.request_id();
    let records = vm.records();
    let finished_at = records
        .iter()
        .position(|r| event(r) == "request" && request_of(r) == Some(id.as_str()))
        .unwrap_or_else(|| vm.fail(&format!("no `request` record for {id}")));
    let mut wrong = Vec::new();
    for (at, record) in leftover_prints(vm) {
        let in_flight = at < finished_at && request_of(&record) == Some(id.as_str());
        if in_flight {
            continue;
        }
        let message = record["message"].as_str().unwrap_or_default().to_owned();
        let got_a_user = message.contains("ctx.user() returned") || message.contains("\"ok\"");
        let threw_otherwise =
            message.contains("ctx.user() threw") && !message.contains("no request in flight");
        if got_a_user || threw_otherwise {
            wrong.push(record);
        }
    }
    let wrong_refs: Vec<&Value> = wrong.iter().collect();
    assert!(
        wrong.is_empty(),
        "expected leftover code, after its request, to get no User: ctx.user() throwing \"no request in flight...\" and whoami refused. It printed:\n{}",
        describe(&wrong_refs)
    );
    let refused_wrongly: Vec<&Value> = records
        .iter()
        .filter(|r| {
            event(r) == "host-call"
                && request_of(r).is_none()
                && r["outcome"] == "refused"
                && r["reason"] != "no-request"
        })
        .collect();
    assert!(
        refused_wrongly.is_empty(),
        "expected host calls with no request in flight to be refused with reason no-request, got\n{}",
        describe(&refused_wrongly)
    );
}

#[test]
fn code_left_running_after_its_request_never_gets_an_allowed_host_call() {
    let vm = Vm::with_stand_in(&[("outlives-request", "outlives-request.js")]);
    let alice = User::new("alice");
    let left = vm.get_as(&alice, "outlives-request", "/leave-behind");
    left.expect_status(200);
    assert_user(&left.json()["user"], &alice, "ctx.user()");
    thread::sleep(LEFTOVER_TIME);
    // A request after the wait, so that a program that only runs leftover
    // code when it next serves has done so.
    let later = vm.get_as(&User::new("bob"), "outlives-request", "/victim?ms=0");
    later.expect_status(200);
    vm.records_of(&later);
    thread::sleep(Duration::from_millis(300));
    vm.assert_log_keeps_each_request_to_itself();
    assert_leftover_code_got_no_one(&vm, &left);
}

/// Checks a victim's answer and records: it saw only its own User, and the
/// host recorded exactly its one whoami, as its own.
fn assert_victim_saw_only_itself(vm: &Vm, response: &Response, user: &User) {
    response.expect_status(200);
    let body = response.json();
    let context = format!("victim {}", user.id);
    assert_user(
        &body["before"],
        user,
        &format!("{context}, ctx.user() before waiting"),
    );
    assert_user(
        &body["after"],
        user,
        &format!("{context}, ctx.user() after waiting"),
    );
    assert_whoami(
        &body["whoami"],
        user,
        "outlives-request",
        &response.request_id(),
        &context,
    );
    let records = vm.records_of(response);
    let calls = records.host_calls();
    assert_eq!(
        calls.len(),
        1,
        "{context}: expected exactly the victim's own one host call under its request id, got\n{}",
        describe(&calls)
    );
    records.assert_host_saw_only(&user.id, "outlives-request");
    assert!(
        records.printed().is_empty(),
        "{context}: the victim prints nothing, so no printed record may carry its id, got\n{}",
        describe(&records.printed())
    );
}

#[test]
fn code_left_running_after_its_request_never_sees_the_next_users_request() {
    let vm = Vm::with_stand_in(&[("outlives-request", "outlives-request.js")]);
    let left = vm.get_as(&User::new("alice"), "outlives-request", "/leave-behind");
    left.expect_status(200);
    // bob's request starts at once and stays in flight while alice's
    // leftover timers fire.
    let bob = User::new("bob").in_groups(&["finance"]);
    let victim = vm.get_as(&bob, "outlives-request", "/victim?ms=900");
    assert_victim_saw_only_itself(&vm, &victim, &bob);
    thread::sleep(Duration::from_millis(300));
    vm.assert_log_keeps_each_request_to_itself();
    assert_leftover_code_got_no_one(&vm, &left);
}

#[test]
fn code_left_running_never_sees_requests_of_many_users_in_flight_at_once() {
    let vm = Vm::with_stand_in(&[("outlives-request", "outlives-request.js")]);
    let left: Vec<Response> = thread::scope(|scope| {
        let leavers: Vec<_> = (0..4)
            .map(|i| {
                let vm = &vm;
                scope.spawn(move || {
                    vm.get_as(
                        &User::new(&format!("leaver-{i}")),
                        "outlives-request",
                        "/leave-behind",
                    )
                })
            })
            .collect();
        for i in 0..8 {
            let vm = &vm;
            scope.spawn(move || {
                let user = User::new(&format!("victim-{i}"));
                let response = vm.get_as(
                    &user,
                    "outlives-request",
                    &format!("/victim?ms={}", 100 * (i % 4) + 50),
                );
                assert_victim_saw_only_itself(vm, &response, &user);
            });
        }
        leavers.into_iter().map(|h| h.join().unwrap()).collect()
    });
    thread::sleep(LEFTOVER_TIME);
    vm.assert_log_keeps_each_request_to_itself();
    for response in &left {
        assert_leftover_code_got_no_one(&vm, response);
    }
}

#[test]
fn host_calls_a_request_does_not_await_are_never_attributed_to_another_request() {
    let vm = Vm::with_stand_in(&[("outlives-request", "outlives-request.js")]);
    for round in 0..5 {
        thread::scope(|scope| {
            let vm = &vm;
            scope.spawn(move || {
                let flood = vm.get_as(&User::new("alice"), "outlives-request", "/flood?n=300");
                flood.expect_status(200);
                vm.records_of(&flood)
                    .assert_host_saw_only("alice", "outlives-request");
            });
            for i in 0..4 {
                scope.spawn(move || {
                    let user = User::new(&format!("victim-{round}-{i}"));
                    let response = vm.get_as(&user, "outlives-request", "/victim?ms=20");
                    assert_victim_saw_only_itself(vm, &response, &user);
                });
            }
        });
    }
    thread::sleep(Duration::from_millis(500));
    vm.assert_log_keeps_each_request_to_itself();
}
