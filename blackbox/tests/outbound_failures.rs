//! Failures (CONTRACT.md, "Outbound calls: Failures"): after the call was
//! allowed and its entry written, a call rejects with an `Error` that has a
//! property `failure`: `{ kind, ... }`, and its entry is `failed` with the
//! table's `error`; an answer's body that stops before its end fails the
//! stream the code reads and, when passed on, ends the client's connection
//! without a clean end; a call ends with its request.
//!
//! Tests that wait for a limit of 30 seconds, or of 10, say so in their
//! names; they are few.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin::{IN_FLIGHT_ALLOWED, disable_ok};
use blackbox::body::{BodyEnd, Framing, Raw};
use blackbox::outbound::{
    APP, Cert, Origin, Outbound, Reply, Wire, assert_failure, assert_resolved, bob,
};
use blackbox::tls::Authority;
use serde_json::{Value, json};

const SOON: Duration = Duration::from_secs(10);

fn listed() -> (Outbound, Origin) {
    let o = Outbound::start();
    let origin = o.origin();
    o.allow(&origin.host());
    (o, origin)
}

/// Fails unless the call's one entry is `failed` with this `error`, and the
/// rejection is an `Error` with this failure kind.
fn assert_failed(o: &Outbound, call: &Value, kind: &str, error: &str) {
    let response = o.call_response(call);
    let report = o.report(&response);
    assert_failure(&report, kind, &call.to_string());
    assert_eq!(
        report["rejected"]["isError"], true,
        "expected the rejection to be an Error (without the SDK), got {report}"
    );
    let entry = o.entry_of(&response);
    assert_eq!(
        (entry["outcome"].as_str(), entry["error"].as_str()),
        (Some("failed"), Some(error)),
        "expected the entry `failed` with `error` {error:?}, got {entry}"
    );
    assert!(
        entry["duration_ms"].is_null() || entry["duration_ms"].is_u64(),
        "{entry}"
    );
}

fn assert_unreachable(report: &Value, why: &[&str], context: &str) {
    assert_failure(report, "unreachable", context);
    let got = report["rejected"]["failure"]["why"].as_str().unwrap_or("");
    assert!(
        why.contains(&got),
        "{context}: expected `why` one of {why:?}, got {report}"
    );
}

#[test]
fn a_name_that_does_not_resolve_is_unreachable_dns_and_its_entry_is_failed_unreachable() {
    let o = Outbound::start();
    o.allow("no-such-name.invalid");
    let response = o.call_response(&json!({ "url": "https://no-such-name.invalid/x" }));
    let report = o.report(&response);
    assert_unreachable(&report, &["dns"], "a name that does not resolve");
    let entry = o.entry_of(&response);
    assert_eq!(
        (
            entry["outcome"].as_str(),
            entry["error"].as_str(),
            entry["host"].as_str()
        ),
        (
            Some("failed"),
            Some("unreachable"),
            Some("no-such-name.invalid")
        ),
        "{entry}"
    );
}

#[test]
fn a_port_nobody_listens_on_is_unreachable_connect() {
    let o = Outbound::start();
    let port = blackbox::ports::chosen().unwrap();
    o.allow(&format!("localhost:{port}"));
    let response = o.call_response(&json!({ "url": format!("https://localhost:{port}/") }));
    let report = o.report(&response);
    assert_unreachable(&report, &["connect"], "a refused connection");
    assert_eq!(o.entry_of(&response)["error"], "unreachable");
}

#[test]
fn a_connection_that_is_made_and_never_answers_the_handshake_is_unreachable_after_10_seconds() {
    // Takes about 10 seconds: the contract's time to resolve and connect,
    // TLS included.
    let o = Outbound::start();
    let silent = Origin::start(Wire::Silent);
    o.allow(&silent.host());
    let began = Instant::now();
    let response = o.call_response(&json!({ "url": silent.url("/") }));
    let waited = began.elapsed();
    let report = o.report(&response);
    assert_unreachable(&report, &["connect", "dns"], "a handshake that never comes");
    assert!(
        waited >= Duration::from_secs(8) && waited < Duration::from_secs(25),
        "expected the call to give up after about 10 seconds, it took {waited:?}"
    );
    assert_eq!(o.entry_of(&response)["error"], "unreachable");
}

#[test]
fn a_certificate_that_does_not_hold_is_unreachable_tls_and_no_request_is_sent() {
    let o = Outbound::start();
    let stranger = Authority::new("an authority nobody trusts");
    let other = Authority::new("outbound test authority");
    let cases: Vec<(&str, Cert)> = vec![
        (
            "another name",
            Cert::for_names_and_addresses(&o.authority, &["other.example"], &[]),
        ),
        ("an unknown authority", Cert::localhost(&stranger)),
        (
            "an authority of the same name but not the one trusted",
            Cert::localhost(&other),
        ),
        (
            "an expired certificate",
            Cert::from_identity(&o.authority.issue_expired(&["localhost"])),
        ),
        (
            "a certificate not valid yet",
            Cert::from_identity(&o.authority.issue_not_valid_yet(&["localhost"])),
        ),
    ];
    for (what, cert) in cases {
        let origin = Origin::start(cert.wire());
        o.allow(&origin.host());
        let response = o.call_response(&json!({ "url": origin.url("/") }));
        let report = o.report(&response);
        assert_unreachable(&report, &["tls"], what);
        assert!(
            origin.requests().is_empty(),
            "{what}: expected no request sent over a connection whose certificate does not hold, got {:?}",
            origin.requests()
        );
        let entry = o.entry_of(&response);
        assert_eq!(
            (entry["outcome"].as_str(), entry["error"].as_str()),
            (Some("failed"), Some("unreachable")),
            "{what}: {entry}"
        );
    }
}

#[test]
fn plain_http_on_the_port_and_an_answer_that_is_not_http_are_unreachable() {
    let o = Outbound::start();
    let plain = Origin::start(Wire::Plain);
    o.allow(&plain.host());
    let report = o.call(&json!({ "url": plain.url("/") }));
    assert_unreachable(&report, &["tls", "protocol"], "plain HTTP on the port");

    let origin = o.origin();
    o.allow(&origin.host());
    origin.always(Reply::raw(
        b"this is not HTTP at all\r\n\r\n",
        Duration::from_secs(5),
    ));
    let report = o.call(&json!({ "url": origin.url("/") }));
    assert_unreachable(&report, &["protocol"], "an answer that is not HTTP/1.1");
}

#[test]
fn no_headers_within_30_seconds_is_a_limit_time_failure_after_30_seconds() {
    // Takes about 30 seconds: the time from the decision to the answer's
    // headers.
    let (o, origin) = listed();
    origin.always(Reply::never(Duration::from_secs(90)));
    let began = Instant::now();
    let response = o.call_response(&json!({ "url": origin.url("/hang") }));
    let waited = began.elapsed();
    let report = o.report(&response);
    assert_failure(&report, "limit", "no headers");
    assert_eq!(report["rejected"]["failure"]["limit"], "time", "{report}");
    assert!(
        waited >= Duration::from_secs(25) && waited < Duration::from_secs(50),
        "expected the call to fail after about 30 seconds, it took {waited:?}"
    );
    let entry = o.entry_of(&response);
    assert_eq!(
        (entry["outcome"].as_str(), entry["error"].as_str()),
        (Some("failed"), Some("time")),
        "{entry}"
    );
    // The connection is closed by the platform.
    assert!(
        origin
            .wait_peer_closed(began, Duration::from_secs(5))
            .is_some(),
        "expected the connection closed when the call failed"
    );
}

#[test]
fn an_answer_with_too_many_headers_or_headers_too_large_is_a_limit_headers_failure() {
    let (o, origin) = listed();
    origin.always(Reply::ok(b"fine").headers_numbered(98));
    let report = o.call(&json!({ "url": origin.url("/ok-headers") }));
    assert_resolved(&report, 200, "98 headers and a content-length");

    origin.always(Reply::ok(b"too many").headers_numbered(101));
    assert_failed(
        &o,
        &json!({ "url": origin.url("/many") }),
        "limit",
        "headers",
    );
    let report = o.call(&json!({ "url": origin.url("/many") }));
    assert_eq!(
        report["rejected"]["failure"]["limit"], "headers",
        "{report}"
    );

    origin.always(Reply::ok(b"fat").header("x-big", &"v".repeat(60 * 1024)));
    let report = o.call(&json!({ "url": origin.url("/fat-ok") }));
    assert_resolved(&report, 200, "60 KiB of headers");

    origin.always(Reply::ok(b"fatter").header("x-big", &"v".repeat(70 * 1024)));
    let report = o.call(&json!({ "url": origin.url("/fat") }));
    assert_failure(&report, "limit", "headers of more than 64 KiB");
    assert_eq!(
        report["rejected"]["failure"]["limit"], "headers",
        "{report}"
    );
}

#[test]
fn a_stream_of_the_codes_own_that_errors_is_a_body_failed_failure() {
    let (o, origin) = listed();
    assert_failed(
        &o,
        &json!({
            "url": origin.url("/stream-fails"), "method": "POST",
            "body": { "kind": "stream", "chunks": ["first", "second"], "failAt": 1 },
        }),
        "body-failed",
        "body-failed",
    );
    // What the other side got is not taken for a whole body: either the
    // request never completed, or it did not arrive at all.
    for seen in origin.requests() {
        assert!(
            !seen.body_complete || seen.body_len < 11,
            "expected a body that failed not sent as whole, got {}",
            seen.describe()
        );
    }
}

#[test]
fn an_answers_body_cut_short_fails_the_stream_the_code_reads_and_is_never_ended_as_whole() {
    let (o, origin) = listed();
    origin.always(Reply::ok(b"").cut_short(100_000, &vec![b'x'; 5000]));
    let report = o.call(&json!({ "url": origin.url("/short") }));
    assert_resolved(&report, 200, "headers that came before the body was cut");
    assert!(
        report["resolved"]["readFailed"].is_string(),
        "expected reading the body to fail when it is shorter than its content-length, got {report}"
    );
    assert!(
        report["resolved"]["length"].as_u64().unwrap() < 100_000,
        "{report}"
    );

    origin.always(Reply::new(200).chunked_then_close(b"a chunk and no end"));
    let report = o.call(&json!({ "url": origin.url("/chunked-short") }));
    assert_resolved(&report, 200, "a chunked body that is cut");
    assert!(
        report["resolved"]["readFailed"].is_string(),
        "expected reading a chunked body that never ended to fail, got {report}"
    );
}

#[test]
fn an_answers_body_cut_short_when_passed_on_ends_the_clients_connection_without_a_clean_end() {
    let (o, origin) = listed();
    origin.always(Reply::ok(b"").cut_short(2_000_000, &vec![b'y'; 300_000]));
    let mut raw = Raw::connect(o.vm.address());
    raw.send_head(
        "GET",
        &format!("/pass?url={}", origin.url("/short")),
        &o.vm.host_of(APP),
        Some(&bob()),
        Framing::None,
        &[],
    );
    let head = raw.read_head(Duration::from_secs(30));
    assert_eq!(head.status, 200, "{head:?}");
    let mut got = 0_usize;
    let end = raw.drain_body(Duration::from_secs(30), |piece| got += piece.len());
    assert_ne!(
        end,
        BodyEnd::Whole,
        "expected the client to see the connection end short of the body, got a whole body of {got} bytes"
    );
    assert_eq!(end, BodyEnd::ClosedShort, "got {got} bytes");
}

#[test]
fn an_answers_body_that_stalls_for_30_seconds_fails_the_stream_after_30_seconds() {
    // Takes about 30 seconds: a body on which no byte moves for 30 seconds
    // is failed.
    let (o, origin) = listed();
    origin.always(Reply::ok(b"").stalls_after(100_000, b"just this", Duration::from_secs(120)));
    let began = Instant::now();
    let report = o.call(&json!({ "url": origin.url("/stall") }));
    let waited = began.elapsed();
    assert_resolved(&report, 200, "headers before the stall");
    assert!(
        report["resolved"]["readFailed"].is_string(),
        "expected the stalled body to fail the stream, got {report}"
    );
    assert!(
        waited >= Duration::from_secs(25) && waited < Duration::from_secs(55),
        "expected the stream to fail after about 30 seconds, it took {waited:?}"
    );
}

#[test]
fn a_stalled_body_passed_on_ends_the_clients_connection_without_a_clean_end_after_30_seconds() {
    // Takes about 30 seconds.
    let (o, origin) = listed();
    origin.always(Reply::ok(b"").stalls_after(100_000, b"just this", Duration::from_secs(120)));
    let mut raw = Raw::connect(o.vm.address());
    raw.send_head(
        "GET",
        &format!("/pass?url={}", origin.url("/stall")),
        &o.vm.host_of(APP),
        Some(&bob()),
        Framing::None,
        &[],
    );
    let head = raw.read_head(Duration::from_secs(30));
    assert_eq!(head.status, 200);
    let end = raw.drain_body(Duration::from_secs(60), |_| ());
    assert_eq!(
        end,
        BodyEnd::ClosedShort,
        "expected the client's connection ended without a clean end"
    );
}

#[test]
fn a_call_ends_with_its_request_when_the_application_is_disabled_and_the_connection_is_closed() {
    let (o, origin) = listed();
    origin.always(Reply::never(Duration::from_secs(120)));
    let held = std::thread::scope(|scope| {
        let request =
            o.vm.request(APP, &format!("/text?url={}", origin.url("/hang")))
                .as_user(&bob())
                .timeout(Duration::from_secs(60));
        let waiting = scope.spawn(move || request.send());
        origin.wait_requests(1, SOON);
        let disabled_at = Instant::now();
        disable_ok(&o.vm, APP);
        let answered = waiting.join().unwrap();
        (disabled_at, answered)
    });
    let (disabled_at, answered) = held;
    assert!(
        disabled_at.elapsed() < Duration::from_secs(15),
        "the request in flight was not answered in time: {}",
        answered.describe()
    );
    let closed = origin
        .wait_peer_closed(disabled_at, Duration::from_secs(10))
        .unwrap_or_else(|| {
            panic!(
                "expected the connection of a call closed within a few seconds of its request ending (a disable), still open after 10 seconds"
            )
        });
    assert!(
        closed < IN_FLIGHT_ALLOWED + Duration::from_secs(6),
        "expected the call's connection closed within a few seconds of the disable, it was {closed:?}"
    );
}

#[test]
fn a_call_ends_with_its_request_when_the_client_goes_away_while_the_answer_is_passed_on() {
    let (o, origin) = listed();
    origin.always(Reply::ok(b"").stalls_after(
        5_000_000,
        &vec![b'z'; 100_000],
        Duration::from_secs(120),
    ));
    let mut raw = Raw::connect(o.vm.address());
    raw.send_head(
        "GET",
        &format!("/pass?url={}", origin.url("/long")),
        &o.vm.host_of(APP),
        Some(&bob()),
        Framing::None,
        &[],
    );
    let head = raw.read_head(Duration::from_secs(30));
    assert_eq!(head.status, 200);
    let gone_at = Instant::now();
    raw.go_away();
    let closed = origin
        .wait_peer_closed(gone_at, Duration::from_secs(10))
        .unwrap_or_else(|| {
            panic!(
                "expected the call's connection closed within a few seconds of the client going away, still open after 10 seconds"
            )
        });
    assert!(closed < Duration::from_secs(8), "closed after {closed:?}");
}
