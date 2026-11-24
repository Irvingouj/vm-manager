//! Bounds (CONTRACT.md, "Outbound calls: Bounds"): 8 calls under way at once
//! for one request, the next waits its turn, 64 may wait and a 73rd is
//! `unavailable` at once; 256 in the program, a call that finds none free
//! waits up to 5 seconds and is then `unavailable`; the memory of about 7 MiB
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
use blackbox::outbound::{APP, OPEN_LOOPBACK, Origin, Outbound, Reply, bob, pattern_sha256};
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
    // The 73rd has no Operation log entry (nothing is kept for it) and one
    // `host-call` record `refused` with reason `limit`; the 72 before it
    // have theirs.
    let entries = o.entries_of(&response);
    assert_eq!(
        entries.len(),
        72,
        "expected an entry for each of the 72 calls decided and none for the 73rd, got {} (errors {:?})",
        entries.len(),
        entries
            .iter()
            .map(|e| e["error"].clone())
            .collect::<Vec<_>>()
    );
    assert!(
        entries.iter().all(|e| e["error"] != "unavailable"),
        "expected no entry `unavailable` for the 73rd call, got {entries:?}"
    );
    let limited: Vec<Value> = blackbox::outbound::host_call_records(&o.vm, &response)
        .into_iter()
        .filter(|r| r["outcome"] == "refused" && r["reason"] == "limit")
        .collect();
    assert_eq!(
        limited.len(),
        1,
        "expected one `host-call` record `refused` with reason `limit` for the 73rd call, got {limited:?}"
    );
}

#[test]
fn the_program_has_256_calls_and_one_that_finds_none_free_waits_5_seconds_then_is_unavailable() {
    // About 20 seconds: four Applications of the same fixture each have 64
    // calls under way (8 requests of 8 each, every answer held for 12
    // seconds), the whole of an Application's share and 256 in the program;
    // a fifth Application's call waits 5 seconds for a place.
    let applications = ["outbound-2", "outbound-3", "outbound-4", "outbound-5"];
    let o = Outbound::start_with(
        &OPEN_LOOPBACK,
        &[
            (applications[0], "outbound.js"),
            (applications[1], "outbound.js"),
            (applications[2], "outbound.js"),
            (applications[3], "outbound.js"),
        ],
    );
    let origin = o.origin();
    o.allow(&origin.host());
    holding(&origin, Duration::from_secs(12));
    let holding_apps = [APP, applications[0], applications[1], applications[2]];
    let fifth = applications[3];
    let url = origin.url("/hold");
    let many_of = |application: &str, n: usize| -> Vec<Value> {
        let response =
            o.vm.request(application, &format!("/many?url={url}&n={n}"))
                .as_user(&bob())
                .timeout(Duration::from_secs(120))
                .send();
        assert_eq!(response.status, 200, "{}", response.describe());
        response.json()["results"].as_array().unwrap().clone()
    };
    let (held, probe) = thread::scope(|scope| {
        let handles: Vec<_> = holding_apps
            .iter()
            .flat_map(|application| std::iter::repeat_n(*application, 8))
            .map(|application| scope.spawn(move || many_of(application, 8)))
            .collect();
        // The program's 256 are all under way.
        let deadline = Instant::now() + Duration::from_secs(10);
        while origin.max_in_flight() < 256 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            origin.max_in_flight(),
            256,
            "expected the program's calls to reach its 256 with four Applications at 64"
        );
        let began = Instant::now();
        let probe = many_of(fifth, 1);
        let took = began.elapsed();
        let ms = probe[0]["ms"].as_u64().unwrap();
        assert!(
            (4000..8500).contains(&ms) && took < Duration::from_secs(9),
            "expected a call that finds none free to wait about 5 seconds, it waited {ms} ms: {}",
            probe[0]
        );
        let held: Vec<Vec<Value>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        (held, probe)
    });
    assert_eq!(
        probe[0]["rejected"]["failure"]["kind"], "unavailable",
        "expected the fifth Application's call past the program's 256 `unavailable`, got {}",
        probe[0]
    );
    assert_eq!(held.len(), 32);
    for r in held.iter().flatten() {
        assert_eq!(
            r["ok"], 200,
            "expected each of the four Applications' 64 calls served, got {r}"
        );
    }
    assert!(
        origin.max_in_flight() <= 256,
        "expected at most 256 calls under way in the program, got {}",
        origin.max_in_flight()
    );
    // The program's bound is full, no Application's share: no `share-full`.
    assert!(
        o.vm.records()
            .iter()
            .all(|r| blackbox::records::event(r) != "share-full"),
        "expected no `share-full` record for a refusal because the program's bound is full"
    );
}

#[test]
fn sixteen_calls_with_large_bodies_in_flight_hold_a_few_mib_each_not_their_bodies() {
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
    // 16 requests of about 7 MiB are 112 MiB; the bodies are 768 MiB.
    let rise = peak.saturating_sub(before);
    eprintln!(
        "MEASURED sixteen calls of 48 MiB: the program's memory rose by {} bytes ({} MiB), {} MiB a call",
        rise,
        rise / MIB,
        rise / MIB / 16
    );
    assert!(
        rise < 160 * MIB,
        "expected 16 calls under way to hold under 10 MiB each (not their 768 MiB of bodies), the memory rose by {} MiB",
        rise / MIB
    );
}
