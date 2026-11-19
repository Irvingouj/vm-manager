//! The program serves HTTPS itself on its listening address (CONTRACT.md,
//! "The program serves TLS itself: The two files"; ticket "The program
//! serves TLS itself with a supplied wildcard certificate").
//!
//! The program is started with `VIBE_TLS_CERT_FILE` and `VIBE_TLS_KEY_FILE`
//! naming the files of a certificate for `api.<domain>` and `*.<domain>`
//! issued by a test authority, and its key; no front of the harness's
//! stands before it. The harness's own TLS client (src/own_tls.rs) is held
//! to one TLS version at a time, a server name or none, and the ALPN
//! protocols it offers.
//!
//! Acceptance criterion: "`serve` given a certificate and a key ... serves
//! HTTPS on its listening address".
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread;
use std::time::{Duration, Instant};

use blackbox::own_tls::{Ended, HANDSHAKE_LIMIT, Hello, Serving, Versions, plain_exchange, shake};
use blackbox::tls::api_name;
use blackbox::{DOMAIN, User, Vm, fixture_bundle};
use rustls::ProtocolVersion;
use serde_json::json;

/// Fails unless a handshake held to `versions` agrees on `want`, presents
/// the program's certificate under the test's authority, and the API
/// answers `GET /v1/installation` with `200` on it.
fn answers_over(serving: &Serving, versions: Versions, want: ProtocolVersion) {
    let mut shaken = shake(serving.address(), &serving.hello().versions(versions))
        .unwrap_or_else(|e| panic!("expected a {want:?} handshake with the program, got: {e}"));
    assert_eq!(shaken.version, Some(want), "{shaken:?}");
    assert_eq!(
        shaken.fingerprint(),
        serving.pair.fingerprint(),
        "expected the certificate of VIBE_TLS_CERT_FILE"
    );
    let (status, body) = shaken
        .get(&api_name(), "/v1/installation")
        .unwrap_or_else(|e| panic!("expected an answer over {want:?}: {e}"));
    assert_eq!(
        status,
        200,
        "expected GET /v1/installation to answer 200 over {want:?}: {}",
        String::from_utf8_lossy(&body)
    );
    let installation: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(installation["domain"], DOMAIN, "{installation}");
    assert!(
        !shaken.asked_for_client_certificate,
        "expected no client certificate asked for over {want:?}"
    );
}

#[test]
fn the_listening_line_says_tls_true_with_the_two_files_and_false_without() {
    let serving = Serving::start();
    assert_eq!(
        serving.vm.listening()["tls"],
        json!(true),
        "expected `\"tls\": true` in the listening line: {}",
        serving.vm.listening()
    );
    let plain = Vm::with_stand_in(&[]);
    assert_eq!(
        plain.listening()["tls"],
        json!(false),
        "expected `\"tls\": false` in the listening line without the two files: {}",
        plain.listening()
    );
    serving.assert_no_key_logged(&[]);
}

#[test]
fn https_answers_on_the_listening_address_with_tls_1_3_alone() {
    let serving = Serving::start();
    answers_over(&serving, Versions::Tls13, ProtocolVersion::TLSv1_3);
    serving.assert_no_key_logged(&[]);
}

#[test]
fn https_answers_on_the_listening_address_with_tls_1_2_alone() {
    let serving = Serving::start();
    answers_over(&serving, Versions::Tls12, ProtocolVersion::TLSv1_2);
    serving.assert_no_key_logged(&[]);
}

#[test]
fn alpn_agrees_on_http_1_1_alone_and_never_on_h2() {
    let serving = Serving::start();
    let offered = shake(
        serving.address(),
        &serving.hello().alpn(&[b"h2", b"http/1.1"]),
    )
    .unwrap_or_else(|e| panic!("expected a handshake offering h2 and http/1.1: {e}"));
    assert_eq!(
        offered.alpn.as_deref(),
        Some(&b"http/1.1"[..]),
        "expected ALPN to agree on http/1.1 when h2 and http/1.1 are offered: {offered:?}"
    );
    // Offered h2 alone: never agreed on (the handshake may be refused).
    if let Ok(only_h2) = shake(serving.address(), &serving.hello().alpn(&[b"h2"])) {
        assert_ne!(
            only_h2.alpn.as_deref(),
            Some(&b"h2"[..]),
            "expected the program never to agree on h2: {only_h2:?}"
        );
    }
    // Offered nothing: HTTP/1.1 all the same.
    let mut none = shake(serving.address(), &serving.hello().alpn(&[])).unwrap();
    let (status, _) = none.get(&api_name(), "/v1/installation").unwrap();
    assert_eq!(status, 200);
}

#[test]
fn a_plain_http_request_to_the_port_gets_no_http_answer_and_the_connection_is_closed() {
    let serving = Serving::start();
    let request = format!("GET /v1/installation HTTP/1.1\r\nHost: api.{DOMAIN}\r\n\r\n");
    let (got, ended) = plain_exchange(
        serving.address(),
        request.as_bytes(),
        Duration::from_secs(15),
    );
    assert!(
        !got.starts_with(b"HTTP/"),
        "expected no HTTP answer to plain HTTP on the TLS port, got {:?}",
        String::from_utf8_lossy(&got)
    );
    assert!(
        matches!(ended, Ended::Closed(_)),
        "expected the program to close the connection, it was still open after 15 s ({} bytes came)",
        got.len()
    );
    eprintln!(
        "plain HTTP on the TLS port: {} bytes back, {ended:?}",
        got.len()
    );
    // And the port goes on serving TLS.
    answers_over(&serving, Versions::Both, ProtocolVersion::TLSv1_3);
}

#[test]
fn the_same_certificate_is_presented_whatever_server_name_is_asked() {
    let serving = Serving::start();
    let want = serving.pair.fingerprint();
    for name in [
        Some(api_name()),
        Some(format!("dashboard.{DOMAIN}")),
        Some(format!("auth.{DOMAIN}")),
        Some(format!("an-application.{DOMAIN}")),
        Some("another.example".to_owned()),
        None,
    ] {
        let shaken = shake(serving.address(), &Hello::any(name.as_deref()))
            .unwrap_or_else(|e| panic!("expected a handshake asking for {name:?}: {e}"));
        assert_eq!(
            shaken.fingerprint(),
            want,
            "expected the same certificate asking for the server name {name:?}"
        );
    }
}

#[test]
fn an_application_and_the_api_are_found_by_host_over_the_programs_own_tls() {
    let serving = Serving::start();
    serving.vm.install("board", &fixture_bundle("honest.js"));
    let answer = serving.vm.get_as(&User::new("ada"), "board", "/over-tls");
    answer.expect_status(200);
    assert_eq!(
        answer.json()["user"]["id"],
        "ada",
        "expected the Application to answer for the User: {}",
        answer.describe()
    );
    serving
        .vm
        .api_request("/v1/installation")
        .send()
        .expect_status(200);
    // A request for another Application's name on the same connection's
    // certificate: found by `Host`, as ever.
    serving
        .vm
        .get_as(&User::new("ada"), "nothing-here", "/")
        .expect_status(404);
}

/// A TLS record header for a ClientHello of 512 bytes, and the first few of
/// them: a handshake begun and never finished.
const BEGUN: &[u8] = &[
    0x16, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0xfc, 0x03, 0x03,
];

#[test]
fn a_handshake_left_unfinished_is_ended_within_ten_seconds_while_others_are_served() {
    let serving = Serving::start();
    let address = serving.address();
    let silent = thread::spawn(move || plain_exchange(address, b"", Duration::from_secs(20)));
    let begun = thread::spawn(move || plain_exchange(address, BEGUN, Duration::from_secs(20)));
    // Meanwhile, others are served.
    let began = Instant::now();
    let mut served = 0;
    while began.elapsed() < Duration::from_secs(8) {
        let mut shaken = shake(address, &serving.hello())
            .unwrap_or_else(|e| panic!("expected other connections served meanwhile: {e}"));
        let (status, _) = shaken.get(&api_name(), "/v1/installation").unwrap();
        assert_eq!(status, 200);
        served += 1;
        thread::sleep(Duration::from_millis(500));
    }
    for (what, ended) in [
        ("a connection that sent nothing", silent.join().unwrap()),
        (
            "a connection that began a ClientHello",
            begun.join().unwrap(),
        ),
    ] {
        let (_, how) = ended;
        let Ended::Closed(after) = how else {
            panic!(
                "expected {what} to be closed within {HANDSHAKE_LIMIT:?}, it was still open after 20 s"
            );
        };
        eprintln!("{what}: closed by the program after {after:?}");
        assert!(
            after >= Duration::from_secs(9) && after <= Duration::from_secs(15),
            "expected {what} to be ended about {HANDSHAKE_LIMIT:?} after it was opened (between 9 and 15 s), it was ended after {after:?}"
        );
    }
    assert!(
        served >= 10,
        "expected other connections served meanwhile, {served} were"
    );
}
