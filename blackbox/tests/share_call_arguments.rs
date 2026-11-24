//! An Application's share of the program's bytes in the arguments of host
//! calls not yet answered (CONTRACT.md, "Resources and Grants in full: An
//! Application's share of what the program holds"; "A host call's
//! arguments are bounded: The bounds"): 64 MiB of the program's 256 MiB,
//! exactly that taken. A call that would pass the share is answered
//! `unavailable` at once, never waited for, with no Operation log entry and
//! a `host-call` record `refused` with the reason `unavailable`; another
//! Application's call is served; the program's log has one `share-full`
//! record, `call-arguments`.
//!
//! Time: about 15 seconds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use blackbox::User;
use blackbox::host_call::{
    APP, Calls, MIB, assert_taken, assert_unavailable, short, sized, sleeper, times,
};
use blackbox::records::event;
use serde_json::{Value, json};

/// A: [`APP`], which fills its share. B: the same fixture beside it.
const A: &str = APP;
const B: &str = "caller-b";

/// How long the calls that wait sleep in the database.
const SLEEP: u32 = 8;

fn epoch_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// A request that holds 15 MiB in four calls that wait (under the
/// request's 16 MiB): four of them hold 60 MiB.
fn holder_body() -> Value {
    json!({
        "held": [
            times(sleeper(SLEEP, 4 * MIB), 3),
            sleeper(SLEEP, 3 * MIB),
        ],
        "awaitHeld": true,
        "settleMs": 100,
    })
}

fn share_full(h: &Calls, application: &str) -> Vec<Value> {
    h.own
        .vm
        .records()
        .into_iter()
        .filter(|r| event(r) == "share-full" && r["application"] == application)
        .collect()
}

#[test]
fn a_call_that_would_pass_the_64_mib_of_an_application_is_unavailable_at_once_while_another_is_served()
 {
    let h = Calls::start_with(&[B]);
    let body = holder_body();
    let before = h.own.vm.records().len();

    let (holders, probed_to) = std::thread::scope(|scope| {
        // A holds 60 MiB: four requests of 15 MiB.
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let body = &body;
                let h = &h;
                scope.spawn(move || {
                    let user = User::new(&format!("holder-{i}"));
                    h.post_to(A, &user, "/hold", body).1
                })
            })
            .collect();
        // The program has decided all 16 calls of the holders.
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let decided = h
                .own
                .vm
                .records()
                .iter()
                .skip(before)
                .filter(|r| event(r) == "host-call" && r["operation"] == "query")
                .count();
            if decided >= 16 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let probes_began = Instant::now();
        // 4 MiB is taken: exactly 64 MiB held.
        let (_, taken) = h.seq_on(A, &[sized("nonesuch", 4 * MIB)]);
        assert_taken(
            &taken[0],
            "nonesuch",
            "a call of 4 MiB with 60 MiB of A's held: exactly the 64 MiB of its share",
        );
        assert!(
            share_full(&h, A).is_empty(),
            "expected no `share-full` record while A is within its share, got {:?}",
            share_full(&h, A)
        );

        // 4 MiB and a byte is refused at once.
        let began = Instant::now();
        let (response, refused) = h.seq_on(A, &[sized("nonesuch", 4 * MIB + 1)]);
        let took = began.elapsed();
        assert_unavailable(
            &refused[0],
            "a call of 4 MiB and one byte with 60 MiB of A's held: it would pass A's 64 MiB",
        );
        assert!(
            took < Duration::from_secs(2),
            "expected the call past the share answered at once, not waited for (the holders sleep {SLEEP} seconds), it took {took:?}"
        );
        assert_eq!(
            h.own.entries_of(&response).len(),
            0,
            "expected no Operation log entry for a call refused for A's share, got\n{}",
            blackbox::scenario::lines(&h.own.entries_of(&response))
        );
        let records: Vec<Value> = h
            .refused_records(&response)
            .into_iter()
            .filter(|r| r["reason"] == "unavailable")
            .collect();
        assert_eq!(
            records.len(),
            1,
            "expected one `host-call` record `refused` with reason `unavailable`, got {records:?}"
        );

        // A call of B of 8 MiB meanwhile is served.
        let (_, b) = h.seq_on(B, &[sized("nonesuch", 8 * MIB)]);
        assert_taken(
            &b[0],
            "nonesuch",
            "a call of 8 MiB of B while A holds 60 MiB",
        );
        let probed_to = epoch_ms();
        // A sleeper that finds no connection of the own database free is
        // answered `unavailable` after about 5 seconds (16 statements at once
        // are more than the pool gives): the probes must end well inside
        // that, so that the 60 MiB are certainly held while they are made.
        assert!(
            probes_began.elapsed() < Duration::from_secs(3),
            "the test's own precondition: the probes were to end within 3 seconds of the holders being decided, they took {:?}",
            probes_began.elapsed()
        );

        let holders: Vec<Value> = handles.into_iter().map(|t| t.join().unwrap()).collect();
        (holders, probed_to)
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

    // The program's log: one `share-full` for A, `call-arguments`, none for B.
    let records = share_full(&h, A);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        records[0]["what"].as_str(),
        Some("call-arguments"),
        "{records:?}"
    );
    assert!(share_full(&h, B).is_empty(), "{:?}", share_full(&h, B));

    // When A's holders end, the large call is served again.
    let (_, after) = h.seq_on(A, &[sized("nonesuch", 4 * MIB + 1)]);
    assert_taken(
        &after[0],
        "nonesuch",
        "the call of 4 MiB and a byte once A's holders ended",
    );
}
