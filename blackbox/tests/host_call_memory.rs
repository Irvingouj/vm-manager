//! What the program holds for host calls (CONTRACT.md, "A host call's
//! arguments are bounded": "Memory"), as its resident memory (`VmRSS`,
//! sampled every 20 ms while the requests run) rises over what it was
//! after a first small request.
//!
//! - what the host keeps for a request's calls is what is counted, not a
//!   multiple of it (one string of 1 MiB given to four hundred calls that
//!   wait: at most 128 MiB, where before the ticket more than 1 GiB);
//! - a call too large is not copied (calls of 20 MiB as fast as the code
//!   can make them, in several requests at once: nothing beyond what the
//!   Applications' own memory is allowed, 64 MiB for each request in
//!   flight, and a margin of 128 MiB for the program's own buffers);
//! - many small values are not a way round (two million `0`s the same).
//!
//! The own database's pool holds a few connections and a call waits for one
//! at most 5 seconds, so what waits is held for 5 seconds or the sleep.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::User;
use blackbox::body::memory_of;
use blackbox::host_call::{Calls, MIB, assert_limit, call, short, sleeper, times, unmeasured};
use blackbox::storage::sampled;
use serde_json::{Value, json};

/// The margin over what the Applications' own memory may hold.
const MARGIN: u64 = 128 * MIB as u64;

/// What the program's memory rose by while `work` ran, in bytes, over its
/// size when `work` began.
fn rise<T>(h: &Calls, work: impl FnOnce() -> T) -> (T, u64) {
    let pid = h.own.vm.pid();
    let before = memory_of(pid, "VmRSS");
    let (outcome, peak) = sampled(pid, work);
    (outcome, peak.saturating_sub(before))
}

/// Once small first, so that what the program allocates the first time is
/// in the baseline.
fn warm(h: &Calls) {
    h.post(
        "/hold",
        &json!({ "held": [times(sleeper(1, 4096), 4)], "awaitHeld": true }),
    );
}

/// `requests` requests at once of this body, each as a User of its own.
/// Returns each response's status and JSON (null when it is none).
fn together(h: &Calls, requests: usize, body: &Value) -> Vec<(u16, Value)> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..requests)
            .map(|i| {
                scope.spawn(move || {
                    let user = User::new(&format!("hostile-{i}"));
                    let response = h.send(&user, "/hold", body);
                    (
                        response.status,
                        serde_json::from_str(&response.text()).unwrap_or(Value::Null),
                    )
                })
            })
            .collect();
        handles.into_iter().map(|t| t.join().unwrap()).collect()
    })
}

/// One request giving one string of 1 MiB to `calls` calls that wait.
fn one_string_to(calls: usize) {
    let h = Calls::start();
    warm(&h);
    let (body, rose) = rise(&h, || {
        h.post(
            "/hold",
            &json!({
                // A query that sleeps, its parameter the same string of 1 MiB
                // (the call's whole arguments are 1 MiB).
                "held": [times(unmeasured(sleeper(6, MIB)), calls)],
                "settleMs": 2500,
                "awaitHeld": false,
            }),
        )
        .1
    });
    eprintln!(
        "MEASURED one string of 1 MiB, {calls} calls: rose {} MiB",
        rose / MIB as u64
    );
    let now = h.results(&body, "heldNow");
    let limited = now
        .iter()
        .filter(|r| r["answer"]["error"]["limit"] == "arguments")
        .count();
    assert_eq!(
        limited,
        calls - 16,
        "expected {calls} calls of 1 MiB to have 16 of them under way (16 MiB) and the other {} answered `limit` `arguments`, {limited} were; the first answered was {}",
        calls - 16,
        now.iter()
            .find(|r| !r.is_null())
            .map_or(String::from("none"), short)
    );
    for r in now.iter().filter(|r| !r.is_null()) {
        assert_limit(
            r,
            "arguments",
            "a call that was answered while the others waited",
        );
    }
    assert!(
        rose <= 128 * MIB as u64,
        "expected the program's resident memory to rise by no more than 128 MiB for one request giving one string of 1 MiB to {calls} calls that wait, it rose by {} MiB",
        rose / MIB as u64
    );
}

#[test]
fn one_string_of_1_mib_given_to_400_waiting_calls_is_kept_once_for_those_that_fit() {
    one_string_to(400);
}

#[test]
fn calls_of_20_mib_made_as_fast_as_can_be_in_several_requests_make_the_program_hold_nothing_for_them()
 {
    let h = Calls::start();
    warm(&h);
    let requests = 4;
    let (answers, rose) = rise(&h, || {
        together(
            &h,
            requests,
            &json!({
                // Each waits (a query that sleeps) with 20 MiB of its own.
                "held": [times(unmeasured(call_20_mib()), 8)],
                "settleMs": 2000,
                "awaitHeld": false,
            }),
        )
    });
    eprintln!(
        "MEASURED 20 MiB calls, {requests} requests x N: rose {} MiB, statuses {:?}",
        rose / MIB as u64,
        answers.iter().map(|a| a.0).collect::<Vec<_>>()
    );
    let allowed = (requests as u64) * 64 * MIB as u64 + MARGIN;
    assert!(
        rose <= allowed,
        "expected the program's memory to rise by no more than {} MiB ({requests} requests in flight at 64 MiB of Application heap each, and a margin of 128 MiB) for calls of 20 MiB it refuses, it rose by {} MiB",
        allowed / MIB as u64,
        rose / MIB as u64
    );
    // The program is still there and serves.
    let (_, results) = h.seq(&[call("whoami", json!({ "undefined": true }))]);
    assert!(
        results[0]["answer"]["ok"].is_object(),
        "{}",
        short(&results[0])
    );
}

/// A query that sleeps whose parameter makes its arguments 20 MiB.
fn call_20_mib() -> Value {
    sleeper(6, 20 * MIB)
}

#[test]
fn calls_of_two_million_zeros_made_as_fast_as_can_be_in_several_requests_make_the_program_hold_nothing_for_them()
 {
    let h = Calls::start();
    warm(&h);
    let requests = 4;
    let zeros = json!({ "sleepZeros": 2_000_000, "seconds": 6 });
    let (answers, rose) = rise(&h, || {
        together(
            &h,
            requests,
            &json!({
                "held": [times(unmeasured(call("query", zeros)), 2)],
                "settleMs": 2000,
                "awaitHeld": false,
            }),
        )
    });
    eprintln!(
        "MEASURED two million zeros, {requests} requests x N: rose {} MiB, statuses {:?}",
        rose / MIB as u64,
        answers.iter().map(|a| a.0).collect::<Vec<_>>()
    );
    for (status, body) in &answers {
        if *status == 200 {
            for r in h.results(body, "heldNow").iter().filter(|r| !r.is_null()) {
                assert_limit(r, "arguments", "a call of two million `0`s as parameters");
            }
        }
    }
    let allowed = (requests as u64) * 64 * MIB as u64 + MARGIN;
    assert!(
        rose <= allowed,
        "expected the program's memory to rise by no more than {} MiB for calls of two million `0`s it refuses without building them, it rose by {} MiB",
        allowed / MIB as u64,
        rose / MIB as u64
    );
}
