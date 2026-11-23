//! How a response is framed is the platform's, never the code's
//! (CONTRACT.md, "What an Application is handed": the last bullet on a
//! response's body).
//!
//! `content-length`, `transfer-encoding`, `connection`, `keep-alive`,
//! `upgrade`, `te` and `trailer` set by the code are not sent: the platform
//! sends the length of the body it has, or sends it chunked. A response
//! whose code set `content-length: 5` over 10 bytes arrives as those 10 bytes
//! with `content-length: 10`, and the next request on the same connection is
//! answered as its own.
//!
//! Every scenario runs on ONE kept-alive connection (the harness's raw
//! client): each answer is read, and the next request, a different one with
//! an answer of its own, must come back right. A lying length that was sent,
//! or a connection closed or poisoned by what the code set, shows there.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::body::{BodyEnd, Head, Raw, assert_framed_truly, pattern_block};
use blackbox::{User, Vm};

/// The fixture Application of this file (shared with `body_response.rs`).
const APP: &str = "body-echo";

const WITHIN: Duration = Duration::from_secs(10);

fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

fn vm() -> Vm {
    Vm::with_stand_in(&[(APP, "body-response.js")])
}

/// Percent-encodes a query value.
fn encoded(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `&h=<name>:<value>` for a header the code sets on its response.
fn sets(name: &str, value: &str) -> String {
    format!("&h={}", encoded(&format!("{name}:{value}")))
}

/// Each of the headers the contract lists, with a value the platform would
/// never send itself (`connection: close` and `transfer-encoding` aside,
/// which are the dangerous values a platform does send on its own terms).
const LISTED: [(&str, &str); 6] = [
    ("connection", "close"),
    ("keep-alive", "timeout=999, max=999"),
    ("upgrade", "x-code-upgrade"),
    ("te", "x-code-te"),
    ("trailer", "x-code-trailer"),
    ("transfer-encoding", "gzip, chunked"),
];

/// Fails unless the answer does not carry the code's value of `name`
/// (absent, or the platform's own).
fn assert_not_sent_as_set(head: &Head, name: &str, value: &str, what: &str) {
    if let Some(sent) = head.header(name) {
        assert_ne!(
            sent.to_ascii_lowercase(),
            value.to_ascii_lowercase(),
            "{what}: the code's `{name}: {value}` was sent as set"
        );
    }
    if name == "transfer-encoding"
        && let Some(sent) = head.header(name)
    {
        assert_eq!(
            sent.to_ascii_lowercase(),
            "chunked",
            "{what}: a transfer-encoding the platform sends is `chunked`"
        );
    }
    assert!(
        !(head.content_length().is_some() && head.chunked()),
        "{what}: a content-length and chunked at once"
    );
}

/// The next request on the connection, answered as its own: its marker
/// comes back whole, and the connection is still there.
fn assert_next_request_is_answered_as_its_own(raw: &mut Raw, vm: &Vm, after: &str) {
    let marker = format!("answer-for-the-request-after-{after}");
    let (head, body, end) = raw.exchange(
        "GET",
        &format!("/text?body={}", encoded(&marker)),
        &vm.host_of(APP),
        Some(&bob()),
        None,
        WITHIN,
    );
    assert_eq!(head.status, 200, "after {after}: got {}", head.status);
    assert_eq!(end, BodyEnd::Whole, "after {after}");
    assert_eq!(
        String::from_utf8_lossy(&body),
        marker,
        "after {after}: expected the next request answered as its own"
    );
    assert_framed_truly(&head, u64::try_from(body.len()).expect("fits"), after);
}

#[test]
fn a_body_the_code_made_is_sent_with_the_platforms_framing_and_the_next_request_is_its_own() {
    // About 2 seconds. A string of 10 bytes under each listed header, and
    // under a content-length that is too short (5), too long (100) and
    // (for a body of 100 KiB) a few hundred times its size.
    let vm = vm();
    let mut raw = Raw::connect_keep_alive(vm.address());

    for (kind, size, lie) in [
        ("string", 10, "5"),
        ("string", 10, "100"),
        ("bytes", 10, "5"),
        ("stream", 10, "100"),
        ("transform", 10, "5"),
        ("string", 102_400, "5"),
        ("string", 10, "0"),
    ] {
        let what = format!("a made {kind} of {size} bytes with `content-length: {lie}` set");
        let (head, body, end) = raw.exchange(
            "GET",
            &format!(
                "/made?kind={kind}&size={size}{}",
                sets("content-length", lie)
            ),
            &vm.host_of(APP),
            Some(&bob()),
            None,
            WITHIN,
        );
        assert_eq!(head.status, 200, "{what}: got {}", head.status);
        assert_eq!(end, BodyEnd::Whole, "{what}");
        assert_eq!(
            body.len(),
            size,
            "{what}: expected the body's own {size} bytes, not the code's length"
        );
        assert_framed_truly(&head, u64::try_from(size).expect("fits"), &what);
        assert_next_request_is_answered_as_its_own(&mut raw, &vm, &what);
    }

    for (name, value) in LISTED {
        let (head, body, end) = raw.exchange(
            "GET",
            &format!("/made?kind=string&size=10{}", sets(name, value)),
            &vm.host_of(APP),
            Some(&bob()),
            None,
            WITHIN,
        );
        let what = format!("a made body with `{name}: {value}` set");
        assert_eq!(head.status, 200, "{what}: got {}", head.status);
        assert_eq!(end, BodyEnd::Whole, "{what}");
        assert_eq!(body, b"xxxxxxxxxx", "{what}");
        assert_not_sent_as_set(&head, name, value, &what);
        assert_framed_truly(&head, 10, &what);
        assert_next_request_is_answered_as_its_own(&mut raw, &vm, &what);
    }

    // The contract's own example: `content-length: 5` over 10 bytes arrives
    // as those 10 bytes with `content-length: 10`.
    let (head, body, _) = raw.exchange(
        "GET",
        &format!("/made?kind=string&size=10{}", sets("content-length", "5")),
        &vm.host_of(APP),
        Some(&bob()),
        None,
        WITHIN,
    );
    assert_eq!(body.len(), 10);
    assert_eq!(head.content_length(), Some(10), "the platform's own length");
}

#[test]
fn a_request_body_given_back_unread_is_sent_with_the_platforms_framing_and_the_next_request_is_its_own()
 {
    // About 2 seconds. The same, for a body the host feeds: the echo of a
    // 256 KiB request body, under each listed header and under a length that
    // is too short and one that is far too long. The answer states the
    // request's length (that sentence is the response bullet before).
    let vm = vm();
    let block = pattern_block(256 * 1024);
    let len = u64::try_from(block.len()).expect("fits");
    let mut raw = Raw::connect_keep_alive(vm.address());

    // The lying lengths first: they are the contract's reason.
    let mut cases: Vec<(String, String)> = Vec::new();
    for lie in ["5", "100", "10000000", "0"] {
        cases.push((
            format!("`content-length: {lie}`"),
            sets("content-length", lie),
        ));
    }
    for (n, v) in LISTED {
        cases.push((format!("`{n}: {v}`"), sets(n, v)));
    }
    for (what, set) in cases {
        let what = format!("a request body given back with {what} set");
        let (head, body, end) = raw.exchange(
            "POST",
            &format!("/echo-unread?status=200{set}"),
            &vm.host_of(APP),
            Some(&bob()),
            Some(&block),
            WITHIN,
        );
        assert_eq!(head.status, 200, "{what}: got {}", head.status);
        assert_eq!(end, BodyEnd::Whole, "{what}");
        assert_eq!(body, block, "{what}: expected the request's own bytes");
        assert_eq!(
            head.content_length(),
            Some(len),
            "{what}: expected the request's stated content-length"
        );
        assert!(!head.chunked(), "{what}: sent chunked with a length stated");
        for (name, value) in LISTED {
            if what.contains(&format!("`{name}: {value}`")) {
                assert_not_sent_as_set(&head, name, value, &what);
            }
        }
        assert_next_request_is_answered_as_its_own(&mut raw, &vm, &what);
    }
}

#[test]
fn a_request_body_of_no_stated_length_given_back_is_chunked_whatever_length_the_code_set() {
    // About 1 second. A chunked request body, echoed, under a content-length
    // of 5: no content-length is sent, the body is chunked and whole, and
    // the next request on the connection is its own.
    let vm = vm();
    let block = pattern_block(100 * 1024);
    let mut raw = Raw::connect_keep_alive(vm.address());
    raw.send_head(
        "POST",
        &format!("/echo-unread?x=1{}", sets("content-length", "5")),
        &vm.host_of(APP),
        Some(&bob()),
        blackbox::body::Framing::Chunked,
        &[],
    );
    raw.send_chunk(&block).expect("a chunk is written");
    raw.finish_chunks().expect("the end is written");
    let head = raw.read_head(WITHIN);
    assert_eq!(head.status, 200, "got {}", head.status);
    assert_eq!(
        head.content_length(),
        None,
        "expected no content-length: the request stated none"
    );
    assert!(head.chunked(), "expected the body chunked");
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(WITHIN, |b| body.extend_from_slice(b)),
        BodyEnd::Whole
    );
    assert_eq!(body, block);
    assert_next_request_is_answered_as_its_own(&mut raw, &vm, "a chunked echo");
}
