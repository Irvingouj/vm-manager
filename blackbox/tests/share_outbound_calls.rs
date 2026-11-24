//! An Application's share of the program's outbound calls under way
//! (CONTRACT.md, "Resources and Grants in full: An Application's share of
//! what the program holds"; "Outbound calls: Bounds"): 64 of the
//! program's 256. A call past the share waits up to 5 seconds and is then
//! `unavailable`, never reaching the host; another Application's call is
//! served at once; a place is given back when the call's answer is read;
//! the program's log has one `share-full` record, `outbound-calls`.
//!
//! Time: about 20 seconds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::outbound::{APP, OPEN_LOOPBACK, Origin, Outbound, Reply, bob};
use blackbox::records::event;
use serde_json::Value;

/// A: [`APP`], which fills its share. B: the same fixture beside it.
const A: &str = APP;
const B: &str = "outbound-b";

/// How long the origin holds an answer to `/hold`.
const HOLD: Duration = Duration::from_secs(14);

fn listed() -> (Outbound, Origin) {
    let o = Outbound::start_with(&OPEN_LOOPBACK, &[(B, "outbound.js")]);
    let origin = o.origin();
    o.allow(&origin.host());
    origin.script(|seen| {
        if seen.path() == "/hold" {
            Reply::ok(b"held").after(HOLD)
        } else {
            Reply::ok(b"fine")
        }
    });
    (o, origin)
}

/// `GET /many?url=<url>&n=<n>` on `application` as bob; the results in the
/// order the calls were made.
fn many(o: &Outbound, application: &str, url: &str, n: usize) -> Vec<Value> {
    let response =
        o.vm.request(application, &format!("/many?url={url}&n={n}"))
            .as_user(&bob())
            .timeout(Duration::from_secs(120))
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    response.json()["results"].as_array().unwrap().clone()
}

fn share_full(o: &Outbound, application: &str) -> Vec<Value> {
    o.vm.records()
        .into_iter()
        .filter(|r| event(r) == "share-full" && r["application"] == application)
        .collect()
}

#[test]
fn an_application_with_64_calls_under_way_waits_5_seconds_for_the_65th_while_another_is_served_at_once()
 {
    let (o, origin) = listed();
    let hold = origin.url("/hold");

    // A: 8 requests of 8 calls, held by the origin.
    let holders: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| many(&o, A, &hold, 8)))
            .collect();

        // All 64 are under way at the origin.
        let deadline = Instant::now() + Duration::from_secs(10);
        while origin.max_in_flight() < 64 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            origin.max_in_flight(),
            64,
            "expected 64 calls of A under way at the host, and no more"
        );

        // A's 65th call: waits about 5 seconds, then `unavailable`, and
        // the host never sees it. B's call meanwhile is answered at once.
        let probe = scope.spawn(|| {
            let began = Instant::now();
            let results = many(&o, A, &origin.url("/probe-a"), 1);
            (results, began.elapsed())
        });
        thread::sleep(Duration::from_millis(1_000));
        let began = Instant::now();
        let b = many(&o, B, &origin.url("/probe-b"), 1);
        let b_took = began.elapsed();
        assert_eq!(
            b[0]["ok"], 200,
            "expected a call of B to be served while A is at its share, got {}",
            b[0]
        );
        assert!(
            b_took < Duration::from_secs(2),
            "expected B's call answered at once (under 2 seconds), it took {b_took:?}"
        );
        let (results, took) = probe.join().unwrap();
        let refused = &results[0];
        assert_eq!(
            refused["rejected"]["failure"]["kind"], "unavailable",
            "expected the 65th call of A `unavailable`, got {refused}"
        );
        assert!(
            took >= Duration::from_millis(4_000) && took <= Duration::from_millis(9_000),
            "expected the 65th call to wait about 5 seconds (between 4 and 9), it took {took:?}"
        );
        assert!(
            origin.requests().iter().all(|r| r.path() != "/probe-a"),
            "expected the host never to see the 65th call of A"
        );

        // The program's log: one `share-full`, `outbound-calls`, for A.
        let records = share_full(&o, A);
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(
            records[0]["what"].as_str(),
            Some("outbound-calls"),
            "{records:?}"
        );
        assert!(
            !records[0].to_string().contains(&origin.host()),
            "expected the record to name no host, got {}",
            records[0]
        );
        assert!(share_full(&o, B).is_empty(), "{:?}", share_full(&o, B));

        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    for results in &holders {
        for r in results {
            assert_eq!(
                r["ok"], 200,
                "expected each of A's 64 calls served in the end, got {r}"
            );
        }
    }

    // When A's calls have ended, A's next call is served.
    let after = many(&o, A, &origin.url("/after-a"), 1);
    assert_eq!(
        after[0]["ok"], 200,
        "expected A's next call served once its calls ended, got {}",
        after[0]
    );
    assert!(origin.requests().iter().any(|r| r.path() == "/after-a"));
}
