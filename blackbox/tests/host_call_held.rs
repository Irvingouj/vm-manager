//! What a request and the program hold at once of host calls not yet
//! answered (CONTRACT.md, "A host call's arguments are bounded": "The
//! bounds"): 16 MiB and 1,024 calls for one request, 256 MiB for the
//! program. A call that would pass a bound is answered at once, never
//! waited for; calls the host answers at once never count; a call counts
//! until it is answered and then no longer.
//!
//! Calls that wait are queries of the Application's own database that sleep
//! (`select pg_sleep(..)`) with a parameter of the code's own making.
//!
//! Pins that hold before the ticket too: `a_request_may_make_...` (twenty
//! calls of 8 MiB one after another), `calls_the_host_answers_at_once_...`.
//! The rest is red until the bounds are there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use blackbox::host_call::{
    CALL_BOUND, Calls, MIB, REQUEST_BOUND, REQUEST_CALLS, assert_limit, assert_taken,
    assert_unavailable, at, call, plain, query_of, quick, short, sized, sleeper, times, unknown,
};
use blackbox::records::{event, request_of};
use blackbox::scenario::lines;
use serde_json::{Value, json};

/// How long the calls that wait sleep in the database: long enough for a
/// loaded machine to make the calls that must come back at once, well
/// below the 10 seconds a statement may run.
const SLEEP: u32 = 6;

/// The refused call has to come back this long before the first sleeper.
const AT_ONCE_BY: i64 = 2_000;

fn epoch_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// Fails unless every held call of `body` (`heldNow`) was still waiting
/// after the settling time: the test's own precondition.
fn assert_all_waiting(body: &Value, what: &str) {
    let settled: Vec<&Value> = body["heldNow"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| !r.is_null())
        .collect();
    assert!(
        settled.is_empty(),
        "the test's own precondition: {what} must still be waiting when the next call is made, but {} had been answered, the first {}",
        settled.len(),
        short(settled[0])
    );
}

/// The first `at` of these results, in ms since the request began.
fn first_at(results: &[Value], started: &Value) -> i64 {
    results.iter().map(|r| at(r, started)).min().unwrap_or(0)
}

#[test]
fn a_call_that_would_pass_16_mib_not_yet_answered_is_limit_arguments_at_once_and_served_when_they_end()
 {
    let h = Calls::start();
    let (response, body) = h.post(
        "/hold",
        &json!({
            "held": [times(sleeper(SLEEP, 4 * MIB), 4)],
            "extra": [
                // Arguments not given are of size 0: they fit at the bound.
                call("whoami", json!({ "undefined": true })),
                sized("nonesuch", 4 * MIB),
                unknown(plain(&json!(1))),
            ],
            "awaitHeld": true,
            "after": [quick(4 * MIB), quick(4 * MIB)],
        }),
    );
    assert_all_waiting(&body, "the four calls of 4 MiB that sleep");
    let started = &body["startedAt"];
    let extra = h.results(&body, "extra");
    let held = h.results(&body, "heldAll");
    let after = h.results(&body, "after");
    for (n, result) in held.iter().enumerate() {
        assert_eq!(
            result["bytes"],
            json!(4 * MIB),
            "the fixture's own check, held call {n}: {}",
            short(result)
        );
        assert!(
            result["answer"]["ok"].is_object(),
            "expected the four calls that fit in 16 MiB to be served, call {n} got {}",
            short(result)
        );
    }
    assert_taken(
        &extra[0],
        "whoami",
        "a call with no arguments at 16 MiB held",
    );
    assert_limit(
        &extra[1],
        "arguments",
        "a call of 4 MiB with 16 MiB of calls not yet answered",
    );
    assert_limit(
        &extra[2],
        "arguments",
        "a call of 32 bytes (one value) with 16 MiB of calls not yet answered",
    );
    let limit_at = at(&extra[1], started);
    let sleepers_at = first_at(&held, started);
    assert!(
        limit_at + AT_ONCE_BY < sleepers_at,
        "expected the call past 16 MiB to be answered at once, not waited for: it came back {limit_at} ms into the request, the first sleeper {sleepers_at} ms"
    );
    // When the sleepers have ended the same calls are served again.
    for (n, result) in after.iter().enumerate() {
        assert_taken(
            result,
            "query",
            &format!("a call of 4 MiB once the sleepers have ended, call {n}"),
        );
    }
    // The refusals made no entry; the four sleepers and the two after did.
    let entries = h.own.entries_of(&response);
    assert_eq!(
        entries.len(),
        4 + 2,
        "expected an Operation log entry for the four sleepers and the two calls after them, none for the two refused, got\n{}",
        lines(&entries)
    );
    assert_eq!(
        h.refused_records(&response)
            .iter()
            .filter(|r| r["reason"] == "limit")
            .count(),
        2,
        "expected two `host-call` records `refused` with reason `limit`"
    );
}

#[test]
fn the_16_mib_of_a_request_are_counted_to_the_byte() {
    let h = Calls::start();
    // 16 MiB less 32 bytes held: a call of 32 bytes (the least a call with
    // an argument is) fits exactly; with 31 held, one byte of it does not.
    for (held_less, fits) in [(32_usize, true), (31, false)] {
        let (_, body) = h.post(
            "/hold",
            &json!({
                "held": [
                    times(sleeper(SLEEP, 4 * MIB), 3),
                    sleeper(SLEEP, 4 * MIB - held_less),
                ],
                "extra": [unknown(plain(&json!(1)))],
                "awaitHeld": false,
            }),
        );
        assert_all_waiting(&body, "the four calls that sleep");
        let extra = h.results(&body, "extra");
        let context = format!(
            "a call of 32 bytes with {} bytes of calls not yet answered",
            REQUEST_BOUND - held_less
        );
        if fits {
            assert_taken(
                &extra[0],
                "nonesuch",
                &format!("{context} (exactly 16 MiB)"),
            );
        } else {
            assert_limit(
                &extra[0],
                "arguments",
                &format!("{context} (one byte over)"),
            );
        }
    }
}

#[test]
fn a_call_past_the_1024_calls_not_yet_answered_of_a_request_is_limit_calls_at_once() {
    let h = Calls::start();
    let marker = json!({ "own": true, "as": "user", "sql": "select 7 as marker" });
    let (response, body) = h.post(
        "/hold",
        &json!({
            "held": [times(query_of(&format!("select pg_sleep({SLEEP}) as s, length($1::text) as n"), 512, false), REQUEST_CALLS)],
            "extra": [
                times(unknown(plain(&json!(1))), 2),
                call("query", plain(&marker)),
                times(unknown(plain(&json!(1))), 2),
            ],
            "settleMs": 200,
            "awaitHeld": false,
        }),
    );
    assert_all_waiting(&body, "the 1,024 calls that sleep");
    let extra = h.results(&body, "extra");
    assert_eq!(extra.len(), 5);
    for (n, result) in extra.iter().enumerate() {
        assert_limit(
            result,
            "calls",
            &format!(
                "call {} of a request with 1,024 calls not yet answered",
                REQUEST_CALLS + 1 + n
            ),
        );
    }
    let entries = h.own.entries_of(&response);
    assert!(
        entries
            .iter()
            .all(|e| !e["statement"].as_str().unwrap_or("").contains("marker")),
        "expected no Operation log entry for a call refused `limit` `calls`, got\n{}",
        lines(&entries[entries.len().saturating_sub(5)..])
    );
    let refused = h.refused_records(&response);
    assert_eq!(
        refused.iter().filter(|r| r["reason"] == "limit").count(),
        5,
        "expected five `host-call` records `refused` with reason `limit`, got {} refused: {refused:?}",
        refused.len()
    );
    // The calls of a request that ended no longer count: the next request
    // is served as if nothing had been held.
    let (_, results) = h.seq(&[sized("nonesuch", 1024), sized("nonesuch", 4 * MIB)]);
    assert_taken(&results[0], "nonesuch", "a call of the next request");
    assert_taken(
        &results[1],
        "nonesuch",
        "a call of 4 MiB of the next request",
    );
}

#[test]
fn calls_the_host_answers_at_once_are_never_limit_however_many_are_made_together() {
    let h = Calls::start();
    // 5,000 calls of an operation the host does not know, 2,000 malformed.
    let (_, body) = h.post(
        "/together",
        &json!({ "calls": [
            times(unknown(plain(&json!({ "a": 1 }))), 5000),
            times(call("query", plain(&json!(5))), 2000),
        ] }),
    );
    let results = h.results(&body, "results");
    assert_eq!(results.len(), 7000);
    for (n, result) in results.iter().enumerate() {
        let answer = &result["answer"];
        let ok = if n < 5000 {
            answer["refused"]["refusal"]["reason"] == "unknown-operation"
        } else {
            answer["error"]["kind"] == "malformed"
        };
        assert!(
            ok,
            "expected call {n} (of {}) answered as the host answers it at once, never `limit`, got {}",
            results.len(),
            short(result)
        );
    }
}

#[test]
fn a_request_may_make_any_number_of_calls_of_8_mib_one_after_another() {
    let h = Calls::start();
    let (_, results) = h.seq(&[times(quick(CALL_BOUND), 20)]);
    assert_eq!(results.len(), 20);
    for (n, result) in results.iter().enumerate() {
        assert_taken(
            result,
            "query",
            &format!(
                "call {} of 8 MiB, each made after the one before was answered",
                n + 1
            ),
        );
    }
}

/// The sleepers a holder request makes: 4 calls, 15.75 MiB in all (under
/// the request's 16 MiB). Sixteen holders hold 252 MiB, which leaves 4 MiB
/// of the program's 256 MiB.
const HOLD_SLEEP: u32 = 8;

fn holder_body() -> Value {
    json!({
        "held": [
            times(sleeper(HOLD_SLEEP, 4 * MIB), 3),
            sleeper(HOLD_SLEEP, 4 * MIB - 256 * 1024),
        ],
        "awaitHeld": true,
        "settleMs": 100,
    })
}

#[test]
fn a_call_that_would_pass_the_256_mib_of_the_program_is_unavailable_at_once_until_the_holders_end()
{
    // The own database's pool holds a few connections and a call waits for
    // one at most 5 seconds, so the holders' calls are waiting for the
    // probes' time only: the test checks that none was answered before the
    // probes were made (its own precondition).
    let h = Calls::start();
    let body = holder_body();
    let before = h.own.vm.records().len();
    let (holders, probed_to, probe_results, probe_response) = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let body = &body;
                let h = &h;
                scope.spawn(move || {
                    let user = blackbox::User::new(&format!("holder-{i}"));
                    h.post_as(&user, "/hold", body).1
                })
            })
            .collect();
        // Wait until the program has decided all 64 calls of the holders
        // (each has its `host-call` record), then make the probes.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            let decided = h
                .own
                .vm
                .records()
                .iter()
                .skip(before)
                .filter(|r| event(r) == "host-call" && r["operation"] == "query")
                .count();
            if decided >= 64 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let (response, results) = h.seq(&[
            // Small: fits. 4 MiB exactly: fits (252 + 4 = 256). One byte
            // more: does not. An unknown operation too, and `whoami`.
            sized("nonesuch", 1024),
            sized("nonesuch", 4 * MIB),
            sized("nonesuch", 4 * MIB + 1),
            sized("nonesuch", CALL_BOUND),
            call("whoami", json!({ "undefined": true })),
        ]);
        let to = epoch_ms();
        let holders: Vec<Value> = handles.into_iter().map(|t| t.join().unwrap()).collect();
        (holders, to, results, response)
    });
    for (n, holder) in holders.iter().enumerate() {
        for result in h.results(holder, "heldAll") {
            assert!(
                result["at"].as_i64().unwrap_or(0) > probed_to,
                "the test's own precondition: the calls of holder {n} were to be waiting while the probes were made (until {probed_to}), one was answered at {}: {}",
                result["at"],
                short(&result)
            );
        }
    }
    assert_taken(
        &probe_results[0],
        "nonesuch",
        "a small call with 252 MiB of the program's held",
    );
    assert_taken(
        &probe_results[1],
        "nonesuch",
        "a call of 4 MiB with 252 MiB held: exactly the 256 MiB",
    );
    assert_unavailable(
        &probe_results[2],
        "a call of 4 MiB and one byte with 252 MiB held: it would pass the program's 256 MiB",
    );
    assert_unavailable(
        &probe_results[3],
        "a call of 8 MiB of an unknown operation with 252 MiB held",
    );
    assert_taken(
        &probe_results[4],
        "whoami",
        "a call with no arguments (size 0) with 252 MiB held",
    );
    let refused = h.refused_records(&probe_response);
    assert_eq!(
        refused
            .iter()
            .filter(|r| r["reason"] == "unavailable")
            .count(),
        2,
        "expected two `host-call` records `refused` with reason `unavailable`, got {refused:?}"
    );
    // The holders ended: the call that was refused is served, and an Operation
    // log entry is made for a call of a known operation that is.
    let (response, results) = h.seq(&[sized("nonesuch", 4 * MIB + 1), quick(CALL_BOUND)]);
    assert_taken(
        &results[0],
        "nonesuch",
        "the call of 4 MiB and a byte once the holders ended",
    );
    assert_taken(
        &results[1],
        "query",
        "a query of 8 MiB once the holders ended",
    );
    assert_eq!(h.own.entries_of(&response).len(), 1);
}

#[test]
fn a_call_made_with_no_request_in_flight_past_a_bound_is_limit_not_no_request() {
    // "Decided before anything else about the call: ... with or without a
    // request in flight." A call at the top level while the code loads:
    // within the bound it is refused `no-request`; past it, `limit`, and the
    // program's log has a `host-call` record refused `limit`, with no
    // request and no User.
    let vm = blackbox::Vm::with_stand_in(&[("host-call-at-load", "host-call-at-load.js")]);
    let response = vm.get_as(&blackbox::User::new("alice"), "host-call-at-load", "/");
    response.expect_status(200);
    let body = response.json();
    assert_eq!(
        body["within"]["answered"]["refused"]["refusal"]["reason"].as_str(),
        Some("no-request"),
        "control: a call of 1 KiB with no request in flight is refused `no-request`, got {}",
        short(&body)
    );
    for what in ["over", "many"] {
        assert_eq!(
            (
                body[what]["answered"]["error"]["kind"].as_str(),
                body[what]["answered"]["error"]["limit"].as_str()
            ),
            (Some("limit"), Some("arguments")),
            "expected the call `{what}` (8 MiB and a byte, or 262,145 values) at load to be answered {{ error: {{ kind: \"limit\", limit: \"arguments\" }} }}, got {}",
            short(&body)
        );
    }
    let limited = vm
        .records()
        .into_iter()
        .filter(|r| {
            event(r) == "host-call"
                && request_of(r).is_none()
                && r["outcome"] == "refused"
                && r["reason"] == "limit"
        })
        .count();
    assert_eq!(
        limited,
        2,
        "expected two `host-call` records with no request, `refused` with reason `limit`; the records were\n{}",
        vm.records()
            .iter()
            .filter(|r| event(r) == "host-call")
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}
