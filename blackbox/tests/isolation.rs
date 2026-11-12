//! Nothing in memory survives from one request to another (CONTRACT.md:
//! "Every request gets a fresh context. The script's top level runs again
//! for each request ... Nothing in memory survives from one request to
//! another.")
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;

use blackbox::expect::assert_user;
use blackbox::{User, Vm};
use serde_json::Value;

fn vm() -> Vm {
    Vm::with_stand_in(&[("leaves-state", "leaves-state.js")])
}

/// alice leaves `value` everywhere she can.
fn leave(vm: &Vm, user: &User, value: &str) {
    let response = vm.get_as(user, "leaves-state", &format!("/leave?value={value}"));
    response.expect_status(200);
    let tried = &response.json()["tried"];
    assert!(
        tried["globalThis"] == "done" && tried["module let"] == "done",
        "expected the Application to manage to leave values in its own memory, got {tried}"
    );
}

/// Fails unless `user` finds nothing left anywhere, and finds a context
/// whose top level ran once and whose handler ran for no one before.
fn assert_finds_nothing(vm: &Vm, user: &User, left: &str) {
    let response = vm.get_as(user, "leaves-state", "/look");
    response.expect_status(200);
    let body = response.json();
    assert_user(&body["user"], user, "ctx.user()");
    assert!(
        !response.text().contains(left),
        "expected {} to find nothing of {left:?} left by an earlier request, got {body}",
        user.id
    );
    let found = body["found"].as_object().expect("found");
    let leftovers: Vec<(&String, &Value)> = found.iter().filter(|(_, v)| !v.is_null()).collect();
    assert!(
        leftovers.is_empty(),
        "expected every place to be fresh for {}, found {leftovers:?}",
        user.id
    );
    assert_eq!(
        body["handledBefore"], 0,
        "expected a fresh context, but its handler had already run: {body}"
    );
    assert_eq!(
        body["loadCount"], 1,
        "expected the top level to have run once in this context: {body}"
    );
}

#[test]
fn what_a_request_leaves_in_memory_is_gone_for_a_later_request_by_another_user() {
    let vm = vm();
    leave(&vm, &User::new("alice"), "alice-secret-1");
    assert_finds_nothing(&vm, &User::new("bob"), "alice-secret-1");
}

#[test]
fn what_a_request_leaves_in_memory_is_gone_for_a_later_request_by_the_same_user() {
    let vm = vm();
    let alice = User::new("alice");
    leave(&vm, &alice, "alice-secret-2");
    assert_finds_nothing(&vm, &alice, "alice-secret-2");
}

#[test]
fn the_top_level_runs_afresh_for_every_request() {
    let vm = vm();
    for user in ["alice", "alice", "bob", "carol", "alice"] {
        assert_finds_nothing(&vm, &User::new(user), "nothing-was-left");
    }
}

#[test]
fn nothing_left_by_many_users_at_once_is_found_by_any_other() {
    let vm = vm();
    thread::scope(|scope| {
        for i in 0..8 {
            let vm = &vm;
            scope.spawn(move || {
                let writer = User::new(&format!("writer-{i}"));
                let reader = User::new(&format!("reader-{i}"));
                for round in 0..3 {
                    leave(vm, &writer, &format!("left-by-writer-{i}-{round}"));
                    let response = vm.get_as(&reader, "leaves-state", "/look");
                    response.expect_status(200);
                    assert!(
                        !response.text().contains("left-by-writer"),
                        "expected {} to find nothing left by any writer, got {}",
                        reader.id,
                        response.text()
                    );
                }
            });
        }
    });
}
