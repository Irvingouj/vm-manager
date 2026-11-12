//! Requests in flight at once, as different Users, each see only their own
//! User throughout, however their awaits interleave. Tested from outside in
//! this ticket by the product owner's decision.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::Barrier;
use std::thread;

use blackbox::expect::{assert_user, assert_whoami};
use blackbox::records::user_of;
use blackbox::{Response, User, Vm};

const USERS: usize = 24;
const ROUNDS: usize = 3;

fn user(i: usize) -> User {
    User::new(&format!("user-{i}")).in_groups(&[&format!("group-{i}"), "everyone"])
}

fn assert_saw_only_itself(vm: &Vm, response: &Response, user: &User) {
    response.expect_status(200);
    let body = response.json();
    let readings = body["readings"].as_array().expect("readings");
    assert_eq!(
        readings.len(),
        4,
        "expected four readings of ctx.user(), got {body}"
    );
    for (n, reading) in readings.iter().enumerate() {
        assert_user(
            reading,
            user,
            &format!("{}'s reading {n} of ctx.user()", user.id),
        );
    }
    let whoami = body["whoami"].as_array().expect("whoami");
    assert_eq!(whoami.len(), 2, "expected two whoami answers, got {body}");
    for (n, answer) in whoami.iter().enumerate() {
        assert_whoami(
            answer,
            user,
            "interleaves",
            &response.request_id(),
            &format!("{}'s whoami {n}", user.id),
        );
    }
    let records = vm.records_of(response);
    assert_eq!(
        records.host_calls().len(),
        2,
        "expected two host calls for {}, got {:?}",
        user.id,
        records.all
    );
    records.assert_host_saw_only(&user.id, "interleaves");
    let printed = records.printed();
    assert_eq!(
        printed.len(),
        1,
        "expected one printed record for {}, got {printed:?}",
        user.id
    );
    assert_eq!(
        printed[0]["message"],
        format!("interleaves: I am {}", user.id),
        "message in {}",
        printed[0]
    );
    assert_eq!(
        user_of(printed[0]),
        Some(user.id.as_str()),
        "user in {}",
        printed[0]
    );
    assert_eq!(
        user_of(&records.request),
        Some(user.id.as_str()),
        "user in {}",
        records.request
    );
}

#[test]
fn many_requests_in_flight_at_once_each_see_only_their_own_user() {
    let vm = Vm::with_stand_in(&[("interleaves", "interleaves.js")]);
    for round in 0..ROUNDS {
        let start = Barrier::new(USERS);
        thread::scope(|scope| {
            for i in 0..USERS {
                let (vm, start) = (&vm, &start);
                scope.spawn(move || {
                    let user = user(i);
                    // Pauses differ, so that the requests' awaits interleave
                    // in a different order each round.
                    let pause = ((i * 37 + round * 53) % 150) + 10;
                    let request = vm
                        .request("interleaves", &format!("/?pause={pause}"))
                        .as_user(&user);
                    start.wait();
                    let response = request.send();
                    assert_saw_only_itself(vm, &response, &user);
                });
            }
        });
    }
    vm.assert_log_keeps_each_request_to_itself();
}

#[test]
fn the_same_user_in_many_requests_at_once_gets_each_request_its_own_id() {
    let vm = Vm::with_stand_in(&[("interleaves", "interleaves.js")]);
    let alice = User::new("alice");
    let start = Barrier::new(8);
    let ids: Vec<String> = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (vm, start, alice) = (&vm, &start, &alice);
                scope.spawn(move || {
                    let request = vm
                        .request("interleaves", &format!("/?pause={}", 20 * i + 10))
                        .as_user(alice);
                    start.wait();
                    let response = request.send();
                    assert_saw_only_itself(vm, &response, alice);
                    response.request_id()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        ids.len(),
        "expected every request its own id, got {ids:?}"
    );
    vm.assert_log_keeps_each_request_to_itself();
}
