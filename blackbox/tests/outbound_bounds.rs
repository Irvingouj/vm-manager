//! Bounds (CONTRACT.md, "Outbound calls: Bounds"): 8 calls under way at once
//! for one request, the next waits its turn, 64 may wait and a 73rd is
//! `unavailable` at once; 256 in the program, a call that finds none free
//! waits up to 5 seconds and is then `unavailable`; the memory of about 4 MiB
//! a call under way, whatever its bodies' length. (Tests that wait for the
//! 5 seconds, or run about 10 seconds, say so.)
//!
//! Not tested here: a body of more than 5 GiB (past it the stream fails,
//! `limit` `bytes`): no test sends 5 GiB. The 10 seconds to connect are in
//! `outbound_failures.rs`; the address of 8 KiB and the headers in
//! `outbound_calls.rs` and `outbound_failures.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::body::{BodyEnd, Checksum, Framing, MIB, Raw, memory_of, peak_rss_during};
use blackbox::outbound::{APP, Origin, Outbound, Reply, bob, pattern_sha256};
use serde_json::Value;

fn listed() -> (Outbound, Origin) {
    let o = Outbound::start();
    let origin = o.origin();
    o.allow(&origin.host());
    (o, origin)
}

/// An origin that holds each answer to `/hold` for this long.
fn holding(origin: &Origin, hold: Duration) {
    origin.script(move |seen| {
        if seen.path() == "/hold" {
            Reply::ok(b"held").after(hold)
        } else {
            Reply::ok(b"fine")
        }
    });
}

/// `GET /many?url=<origin>/hold&n=<n>` as bob; the results in the order the
/// calls were made.
fn many(o: &Outbound, origin: &Origin, n: usize) -> Vec<Value> {
    let response =
        o.vm.request(APP, &format!("/many?url={}&n={n}", origin.url("/hold")))
            .as_user(&bob())
            .timeout(Duration::from_secs(120))
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    response.json()["results"].as_array().unwrap().clone()
}

#[test]
fn eight_calls_are_under_way_for_one_request_and_the_next_wait_their_turn_and_are_served() {
    let (o, origin) = listed();
    holding(&origin, Duration::from_millis(1500));
    let results = many(&o, &origin, 12);
    assert_eq!(results.len(), 12);
    for r in &results {
        assert_eq!(
            r["ok"], 200,
            "expected every call to be served in its turn, got {r}"
        );
    }
    assert_eq!(
        origin.max_in_flight(),
        8,
        "expected 8 calls under way at once for one request, never more"
    );
    assert_eq!(origin.requests().len(), 12);
}

#[test]
fn up_to_64_calls_wait_their_turn_and_a_73rd_is_unavailable_at_once() {
    // About 3 seconds: nine turns of 300 ms (a call that waits its turn is not given up before the 5 seconds of a place, which the contract leaves open for the wait of a request: see the report).
    let (o, origin) = listed();
    holding(&origin, Duration::from_millis(300));
    let response =
        o.vm.request(APP, &format!("/many?url={}&n=73", origin.url("/hold")))
            .as_user(&bob())
            .timeout(Duration::from_secs(120))
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    let results = response.json()["results"].as_array().unwrap().clone();
    assert_eq!(results.len(), 73);
    let failed: Vec<&Value> = results.iter().filter(|r| r["ok"].is_null()).collect();
    assert_eq!(
        failed.len(),
        1,
        "expected exactly one of 73 calls refused a place (8 under way and 64 waiting), got {failed:?}"
    );
    let unavailable = failed[0];
    assert_eq!(
        unavailable["rejected"]["failure"]["kind"], "unavailable",
        "{unavailable}"
    );
    assert_eq!(unavailable["rejected"]["isError"], true, "{unavailable}");
    assert!(
        unavailable["ms"].as_u64().unwrap() < 400,
        "expected the 73rd `unavailable` at once, it took {} ms",
        unavailable["ms"]
    );
    assert_eq!(origin.max_in_flight(), 8);
    assert_eq!(origin.requests().len(), 72);
    let entries: Vec<Value> = o
        .entries_of(&response)
        .into_iter()
        .filter(|e| e["error"] == "unavailable")
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected one entry `failed` with `error` `unavailable`, got {entries:?}"
    );
    assert_eq!(entries[0]["outcome"], "failed", "{}", entries[0]);
}

#[test]
fn the_program_has_256_calls_and_one_that_finds_none_free_waits_5_seconds_then_is_unavailable() {
    // About 25 seconds: 33 requests of 8 calls each, every answer held for
    // 9 seconds; the 257th call waits 5 seconds for a place.
    let (o, origin) = listed();
    holding(&origin, Duration::from_secs(9));
    let started = Instant::now();
    let results: Vec<Vec<Value>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..33)
            .map(|_| scope.spawn(|| many(&o, &origin, 8)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let all: Vec<&Value> = results.iter().flatten().collect();
    assert_eq!(all.len(), 33 * 8);
    let unavailable: Vec<&&Value> = all
        .iter()
        .filter(|r| r["rejected"]["failure"]["kind"] == "unavailable")
        .collect();
    assert!(
        !unavailable.is_empty(),
        "expected the calls past the program's 256 to be `unavailable`, got none in {:?}",
        started.elapsed()
    );
    for r in &unavailable {
        let ms = r["ms"].as_u64().unwrap();
        assert!(
            (4000..8500).contains(&ms),
            "expected a call that finds none free to wait about 5 seconds, it waited {ms} ms: {r}"
        );
    }
    let served = all.iter().filter(|r| r["ok"] == 200).count();
    assert_eq!(
        served + unavailable.len(),
        33 * 8,
        "expected every call served or `unavailable`"
    );
    assert!(
        origin.max_in_flight() <= 256,
        "expected at most 256 calls under way in the program, got {}",
        origin.max_in_flight()
    );
    assert!(
        origin.max_in_flight() >= 240,
        "expected the program's calls to reach its 256, got {}",
        origin.max_in_flight()
    );
}

#[test]
fn sixteen_calls_with_large_bodies_in_flight_hold_about_4_mib_each_not_their_bodies() {
    let (o, origin) = listed();
    let each = 48 * MIB;
    origin.script(move |seen| {
        if seen.path() == "/warm" {
            Reply::ok(b"warm")
        } else {
            Reply::new(200).pattern(each)
        }
    });
    let response =
        o.vm.request(APP, &format!("/pass?url={}", origin.url("/warm")))
            .as_user(&bob())
            .send();
    response.expect_status(200);
    let before = memory_of(o.vm.pid(), "VmRSS");
    let address = o.vm.address();
    let host = o.vm.host_of(APP);
    let target = format!("/pass?url={}", origin.url("/big"));
    let (outcomes, peak) = peak_rss_during(o.vm.pid(), || {
        thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        let mut raw = Raw::connect(address);
                        raw.send_head("GET", &target, &host, Some(&bob()), Framing::None, &[]);
                        let head = raw.read_head(Duration::from_secs(60));
                        let mut sha = Checksum::new();
                        let mut total = 0_u64;
                        let end = raw.drain_body(Duration::from_secs(120), |piece| {
                            sha.take(piece);
                            total += u64::try_from(piece.len()).unwrap();
                        });
                        (head.status, end, total, sha.hex())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        })
    });
    let expected = pattern_sha256(each);
    for (status, end, total, sha) in &outcomes {
        assert_eq!(*status, 200);
        assert_eq!(*end, BodyEnd::Whole, "got {total} bytes");
        assert_eq!(*total, each);
        assert_eq!(sha, &expected);
    }
    // 16 calls of 4 MiB are 64 MiB; the bodies are 768 MiB.
    let rise = peak.saturating_sub(before);
    assert!(
        rise < 160 * MIB,
        "expected 16 calls under way to hold about 4 MiB each (not their 768 MiB of bodies), the memory rose by {} MiB",
        rise / MIB
    );
}
