//! Bodies are streams in both directions (CONTRACT.md, "Outbound calls: What
//! Application code does"; "What an Application is handed"; "Storage: A file
//! is a handle"): an answer's body is a stream the host feeds in chunks of
//! at most 64 KiB, and `return fetch(url)` passes the bytes from the other
//! side to the client natively; `request.body` and a stored file's `body`
//! given as a call's `body` go from their source to the other side natively;
//! an answer's body put into Storage goes as it comes. Nothing of a body is in
//! the Application's memory: the program's resident memory, sampled while
//! 64 MiB passes, rises by far less.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::body::{
    BodyEnd, CHUNK, Checksum, Framing, MIB, Raw, memory_of, pattern_block, peak_rss_during,
};
use blackbox::outbound::{
    APP, OPEN_LOOPBACK, Origin, Outbound, Reply, Setup, allow_on, assert_resolved, bob,
    pattern_sha256,
};
use blackbox::storage::{FILE_WRITERS, MARKETING, Place, Storage, wanda};
use serde_json::json;

/// 64 MiB, the size of the bodies that pass.
const BIG: u64 = 64 * MIB;

/// What the program's resident memory may rise by while 64 MiB passes: far
/// less than the body (about 4 MiB for a call under way, "Bounds").
const MEMORY_SLACK: u64 = 32 * MIB;

fn listed() -> (Outbound, Origin) {
    let o = Outbound::start();
    let origin = o.origin();
    o.allow(&origin.host());
    (o, origin)
}

/// Passes one small answer first, so that what the program allocates the
/// first time is in the baseline; returns the resident memory then.
fn warm(o: &Outbound, origin: &Origin, route: &str) -> u64 {
    let response =
        o.vm.request(APP, &format!("{route}?url={}", origin.url("/warm")))
            .as_user(&bob())
            .send();
    response.expect_status(200);
    memory_of(o.vm.pid(), "VmRSS")
}

/// Reads the answer's body to its end, hashing it; (bytes, sha256, end).
fn checksum_of(raw: &mut Raw, within: Duration) -> (u64, String, BodyEnd) {
    let mut sha = Checksum::new();
    let mut total = 0_u64;
    let end = raw.drain_body(within, |piece| {
        sha.take(piece);
        total += u64::try_from(piece.len()).unwrap();
    });
    (total, sha.hex(), end)
}

#[test]
fn an_answer_of_64_mib_given_back_as_the_response_arrives_whole_without_entering_the_memory() {
    for route in ["/pass", "/pass-new"] {
        let (o, origin) = listed();
        origin.script(|seen| {
            if seen.path() == "/warm" {
                Reply::ok(b"warm").header("content-type", "text/plain")
            } else {
                Reply::new(200)
                    .header("content-type", "application/octet-stream")
                    .pattern(BIG)
            }
        });
        let before = warm(&o, &origin, route);
        let mut raw = Raw::connect(o.vm.address());
        raw.send_head(
            "GET",
            &format!("{route}?url={}", origin.url("/big")),
            &o.vm.host_of(APP),
            Some(&bob()),
            Framing::None,
            &[],
        );
        let ((head, (total, sha, end)), peak) = peak_rss_during(o.vm.pid(), || {
            let head = raw.read_head(Duration::from_secs(30));
            let body = checksum_of(&mut raw, Duration::from_secs(60));
            (head, body)
        });
        assert_eq!(head.status, 200, "{route}: {head:?}");
        assert_eq!(
            end,
            BodyEnd::Whole,
            "{route}: expected the whole body, got {end:?} after {total} bytes"
        );
        assert_eq!(total, BIG, "{route}: expected 64 MiB");
        assert_eq!(
            sha,
            pattern_sha256(BIG),
            "{route}: expected the bytes as the other side sent them"
        );
        eprintln!(
            "MEASURED streams {}: the program's memory rose by {} bytes ({} MiB)",
            std::thread::current().name().unwrap_or("test"),
            peak.saturating_sub(before),
            peak.saturating_sub(before) / MIB
        );
        assert!(
            peak.saturating_sub(before) < MEMORY_SLACK,
            "{route}: expected the program's memory to rise by far less than 64 MiB, it rose from {before} to {peak} bytes"
        );
    }
}

#[test]
fn request_body_of_64_mib_given_as_a_calls_body_arrives_whole_whatever_the_clients_framing() {
    for chunked in [false, true] {
        let (o, origin) = listed();
        origin.always(Reply::ok(b"received"));
        // A first small upload, for the baseline.
        let mut warm = Raw::connect(o.vm.address());
        let (head, _, _) = warm.exchange(
            "POST",
            &format!("/send-request-body?url={}", origin.url("/warm")),
            &o.vm.host_of(APP),
            Some(&bob()),
            Some(&pattern_block(CHUNK)),
            Duration::from_secs(20),
        );
        assert_eq!(head.status, 200, "{head:?}");
        let before = memory_of(o.vm.pid(), "VmRSS");
        let mut raw = Raw::connect(o.vm.address());
        raw.send_head(
            "POST",
            &format!(
                "/send-request-body?url={}&method=PUT&type=application/octet-stream",
                origin.url("/up")
            ),
            &o.vm.host_of(APP),
            Some(&bob()),
            if chunked {
                Framing::Chunked
            } else {
                Framing::Length(BIG)
            },
            &[("content-type", "application/octet-stream")],
        );
        let block = pattern_block(CHUNK);
        let (head, peak) = peak_rss_during(o.vm.pid(), || {
            for _ in 0..(BIG / u64::try_from(CHUNK).unwrap()) {
                if chunked {
                    raw.send_chunk(&block).unwrap();
                } else {
                    raw.write(&block).unwrap();
                }
            }
            if chunked {
                raw.finish_chunks().unwrap();
            }
            raw.read_head(Duration::from_secs(60))
        });
        assert_eq!(head.status, 200, "chunked {chunked}: {head:?}");
        let seen = origin
            .wait_requests(2, Duration::from_secs(30))
            .pop()
            .unwrap();
        assert_eq!(seen.method, "PUT", "{}", seen.describe());
        assert_eq!(seen.body_len, BIG, "chunked {chunked}: {}", seen.describe());
        assert_eq!(
            seen.body_sha256,
            pattern_sha256(BIG),
            "chunked {chunked}: expected the body as the client sent it"
        );
        assert!(seen.body_complete);
        assert_eq!(
            seen.header("content-type"),
            Some("application/octet-stream")
        );
        eprintln!(
            "MEASURED streams {}: the program's memory rose by {} bytes ({} MiB)",
            std::thread::current().name().unwrap_or("test"),
            peak.saturating_sub(before),
            peak.saturating_sub(before) / MIB
        );
        assert!(
            peak.saturating_sub(before) < MEMORY_SLACK,
            "chunked {chunked}: expected the program's memory to rise by far less than 64 MiB, it rose from {before} to {peak} bytes"
        );
    }
}

/// The program with Storage and outbound calls, `marketing` writable by the
/// writers, the fixture installed declaring it, and an origin listed.
struct WithStorage {
    s: Storage,
    origin: Origin,
    _setup: Setup,
}

fn with_storage() -> WithStorage {
    let setup = Setup::new(&OPEN_LOOPBACK);
    let place = Place::new("outbound");
    let root = place.root();
    let s = Storage::start_on(place, Some(&root), &setup.options);
    s.register(MARKETING);
    s.grant(MARKETING, FILE_WRITERS, "write");
    s.install(APP, "outbound.js", &[(MARKETING, "storage", "write")]);
    let origin = Origin::start(setup.cert().wire());
    allow_on(&s.vm, &origin.host());
    WithStorage {
        s,
        origin,
        _setup: setup,
    }
}

#[test]
fn a_stored_files_body_given_as_a_calls_body_goes_from_the_directory_to_the_other_side() {
    let w = with_storage();
    let len = 24 * MIB;
    let mut bytes = Vec::new();
    let block = pattern_block(CHUNK);
    while u64::try_from(bytes.len()).unwrap() < len {
        bytes.extend_from_slice(&block);
    }
    std::fs::write(w.s.dir(MARKETING).join("big.bin"), &bytes).unwrap();
    w.origin.always(Reply::ok(b"stored and sent"));
    let before = memory_of(w.s.vm.pid(), "VmRSS");
    let (response, peak) = peak_rss_during(w.s.vm.pid(), || {
        w.s.vm
            .request(APP, "/send-file")
            .as_user(&wanda())
            .method("POST")
            .json(&json!({
                "url": w.origin.url("/upload"), "method": "PUT",
                "resource": MARKETING, "key": "big.bin",
            }))
            .timeout(Duration::from_secs(60))
            .send()
    });
    assert_eq!(response.status, 200, "{}", response.describe());
    assert_eq!(response.json()["status"], 200, "{}", response.describe());
    let seen = w.origin.the_request();
    assert_eq!(seen.method, "PUT");
    assert_eq!(
        seen.body_len,
        u64::try_from(bytes.len()).unwrap(),
        "{}",
        seen.describe()
    );
    assert_eq!(
        seen.body_sha256,
        blackbox::outbound::sha256(&bytes),
        "{}",
        seen.describe()
    );
    eprintln!(
        "MEASURED streams {}: the program's memory rose by {} bytes ({} MiB)",
        std::thread::current().name().unwrap_or("test"),
        peak.saturating_sub(before),
        peak.saturating_sub(before) / MIB
    );
    assert!(
        peak.saturating_sub(before) < MEMORY_SLACK,
        "expected the file not to enter the memory: it rose from {before} to {peak} bytes"
    );
}

#[test]
fn an_answers_body_put_into_storage_lands_as_it_came_without_entering_the_memory() {
    let w = with_storage();
    let len = 24 * MIB;
    w.origin.always(Reply::new(200).pattern(len));
    let before = memory_of(w.s.vm.pid(), "VmRSS");
    let (response, peak) = peak_rss_during(w.s.vm.pid(), || {
        w.s.vm
            .request(APP, "/put-answer")
            .as_user(&wanda())
            .method("POST")
            .json(&json!({
                "url": w.origin.url("/download"), "resource": MARKETING, "key": "from-outside.bin",
            }))
            .timeout(Duration::from_secs(60))
            .send()
    });
    assert_eq!(response.status, 200, "{}", response.describe());
    let report = response.json();
    assert!(
        report["put"]["ok"].is_object(),
        "expected the put to land, got {report}"
    );
    let landed = std::fs::read(w.s.dir(MARKETING).join("from-outside.bin")).unwrap();
    assert_eq!(u64::try_from(landed.len()).unwrap(), len, "{report}");
    assert_eq!(
        blackbox::outbound::sha256(&landed),
        pattern_sha256(len),
        "expected the file to hold the answer's bytes"
    );
    eprintln!(
        "MEASURED streams {}: the program's memory rose by {} bytes ({} MiB)",
        std::thread::current().name().unwrap_or("test"),
        peak.saturating_sub(before),
        peak.saturating_sub(before) / MIB
    );
    assert!(
        peak.saturating_sub(before) < MEMORY_SLACK,
        "expected the answer not to enter the memory: it rose from {before} to {peak} bytes"
    );
}

#[test]
fn an_answers_body_read_by_the_code_comes_in_chunks_of_at_most_64_kib() {
    let (o, origin) = listed();
    origin.always(Reply::new(200).pattern(2 * MIB));
    let report = o.call(&json!({ "url": origin.url("/chunks") }));
    assert_resolved(&report, 200, "a body of 2 MiB");
    let answer = &report["resolved"];
    assert_eq!(answer["length"].as_u64(), Some(2 * MIB), "{report}");
    assert!(
        answer["maxChunk"].as_u64().unwrap() <= 64 * 1024,
        "expected chunks of at most 64 KiB, got {report}"
    );
    assert!(answer["chunks"].as_u64().unwrap() >= 32, "{report}");
    assert_eq!(answer["bodyIsStream"], true, "{report}");
}
