//! A put's body is not arguments (CONTRACT.md, "A host call's arguments are
//! bounded": "The size of a call's arguments"; "Storage": put, "Bounds"): a
//! string of 20 MiB (past the 8 MiB of a call) is written as its UTF-8,
//! whatever the characters, and the size of the put's other arguments is
//! still bounded.
//!
//! The first test holds before the ticket; the second is red until the bound
//! is there. Through the raw interface (`env.VM.invoke("storage.put", ..)`);
//! not through the SDK, whose own tests for a put's string body are
//! `storage_sdk.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::host_call::{CALL_BOUND, MIB, assert_limit, assert_measured, padded, short};
use blackbox::storage::{MARKETING, Storage, wanda};
use serde_json::json;

const APP: &str = "host-calls";

fn ready() -> Storage {
    let s = Storage::ready();
    s.install(
        APP,
        "host-call-bounds.js",
        &[(MARKETING, "storage", "write")],
    );
    s
}

fn seq(s: &Storage, calls: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let (_, body) = s.post(&wanda(), APP, "/seq", &json!({ "calls": calls }));
    body["results"].as_array().cloned().unwrap_or_default()
}

/// The string the fixture makes for `length` bytes of its pad with
/// `multibyte`: characters of 3, 2, 4 and 1 bytes, then `a`s.
fn pad(length: usize) -> Vec<u8> {
    let unit = "\u{20ac}\u{e9}\u{1F600}a";
    let mut text = unit.repeat(length / 10);
    text.push_str(&"a".repeat(length % 10));
    text.into_bytes()
}

#[test]
fn a_put_of_a_string_body_of_20_mib_with_multibyte_characters_writes_exactly_its_utf_8() {
    let s = ready();
    let total = 20 * MIB;
    let base = json!({ "resource": MARKETING, "key": "big/text.txt" });
    let results = seq(&s, &[padded("storage.put", &base, "body", total, true)]);
    assert_eq!(results.len(), 1, "expected the fixture's answer");
    assert_measured(&results[0], total, "the put's arguments");
    assert!(
        results[0]["answer"]["ok"].is_object(),
        "expected a put with a string body of 20 MiB (not arguments: a body) to be taken, got {}",
        short(&results[0])
    );
    // The text of the arguments minus the string's own bytes.
    let overhead = format!(r#"{{"resource":"{MARKETING}","key":"big/text.txt","body":""}}"#).len();
    let expected = pad(total - overhead);
    let got = std::fs::read(s.dir(MARKETING).join("big/text.txt")).unwrap();
    assert_eq!(
        got.len(),
        expected.len(),
        "expected the file to hold the UTF-8 bytes of the string (not its characters, not its JSON)"
    );
    assert!(
        got == expected,
        "expected the file to hold exactly the string's UTF-8 bytes; they differ"
    );
    assert_eq!(
        results[0]["answer"]["ok"]["size"].as_u64(),
        u64::try_from(expected.len()).ok(),
        "expected the metadata's size to be the UTF-8 length: {}",
        short(&results[0])
    );
}

#[test]
fn a_put_whose_other_arguments_are_too_large_is_limit_whatever_its_body() {
    let s = ready();
    let results = seq(
        &s,
        &[
            // A key of 8 MiB and a byte (the put is refused for the size
            // before the key is looked at), a small body.
            padded(
                "storage.put",
                &json!({ "resource": MARKETING, "body": "x" }),
                "key",
                CALL_BOUND + 1,
                false,
            ),
            // A content type, the same.
            padded(
                "storage.put",
                &json!({ "resource": MARKETING, "key": "ct.txt", "body": "x" }),
                "content_type",
                CALL_BOUND + 1,
                false,
            ),
        ],
    );
    assert_eq!(results.len(), 2);
    assert_limit(
        &results[0],
        "arguments",
        "a put with a key of 8 MiB and a byte",
    );
    assert_limit(
        &results[1],
        "arguments",
        "a put with a content type of 8 MiB and a byte",
    );
    assert!(
        !s.dir(MARKETING).join("ct.txt").exists(),
        "expected nothing written for a put refused for its arguments' size"
    );
}

// ---------------------------------------------------------------------------
// A string body's threshold, and puts refused before their body is taken.
// `fixtures/host-call-puts.js` makes the puts; see its header.

const PUTS: &str = "host-call-puts";
/// Declares only [`MARKETING`].
const NARROW: &str = "host-call-narrow";
/// A Storage Resource the writers hold no Grant on.
const LOCKED: &str = "locked";
/// The threshold: the JSON text of a string, between its quotes.
const THRESHOLD: usize = 64 * 1024;

fn puts_ready() -> Storage {
    let s = Storage::ready();
    s.register(LOCKED);
    s.install(
        PUTS,
        "host-call-puts.js",
        &[
            (MARKETING, "storage", "write"),
            (LOCKED, "storage", "write"),
        ],
    );
    s.install(
        NARROW,
        "host-call-puts.js",
        &[(MARKETING, "storage", "write")],
    );
    // A Resource just registered may answer `unavailable` until its
    // directory has been opened (up to 10 seconds): wait that out.
    let began = std::time::Instant::now();
    loop {
        let (_, body) = s.post(
            &wanda(),
            PUTS,
            "/run",
            &json!({ "steps": [put_bytes(MARKETING, "warm.bin", 1)] }),
        );
        if body["results"][0]["answer"]["ok"].is_object() {
            break;
        }
        assert!(
            began.elapsed() < std::time::Duration::from_secs(30),
            "the test's own precondition: {MARKETING} usable within 30 s, got {body}"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    s
}

fn run(
    s: &Storage,
    app: &str,
    streams: usize,
    steps: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let (_, body) = s.post(
        &wanda(),
        app,
        "/run",
        &json!({ "streams": streams, "steps": steps }),
    );
    let made = body["streams"].as_array().cloned().unwrap_or_default();
    assert!(
        made.iter().all(|m| m == "ok") && made.len() == streams,
        "the test's own precondition: {streams} streams of the code's own opened, got {body}"
    );
    body["results"].as_array().cloned().unwrap_or_default()
}

/// A string made of these parts (text, repeat), and the same for the fixture.
fn text_of(parts: &[(&str, usize)]) -> String {
    parts.iter().map(|(t, n)| t.repeat(*n)).collect()
}

/// The bytes of the JSON text of `text` between its quotes.
fn json_bytes(text: &str) -> usize {
    serde_json::to_string(text).unwrap().len() - 2
}

fn put_string(key: &str, parts: &[(&str, usize)]) -> serde_json::Value {
    json!({ "resource": MARKETING, "key": key, "body": { "kind": "string", "parts": parts } })
}

fn put_bytes(resource: &str, key: &str, n: usize) -> serde_json::Value {
    json!({ "resource": resource, "key": key, "body": { "kind": "bytes", "n": n } })
}

fn assert_ok_put(result: &serde_json::Value, context: &str) {
    assert!(
        result["answer"]["ok"].is_object(),
        "{context}: expected the put to land, got {}",
        short(result)
    );
}

#[test]
fn a_string_whose_json_text_is_at_most_64_kib_needs_no_stream_and_a_longer_one_does() {
    let s = puts_ready();
    // With all 16 streams of its own in use: exactly 65,536 bytes of JSON
    // text travels with the arguments; one byte more needs a stream, and
    // there is none: `unavailable`.
    let cases = [
        ("plain at the threshold", vec![("a", THRESHOLD)], true),
        ("plain one byte over", vec![("a", THRESHOLD + 1)], false),
        // Under 64 KiB of UTF-8 (40,000 bytes), over it as JSON text.
        (
            "quotes: 40,000 bytes, 80,000 as JSON",
            vec![("\"", 40_000)],
            false,
        ),
        // Control characters are six bytes each as JSON: 10,923 are 65,538.
        ("control characters over", vec![("\u{1}", 10_923)], false),
        (
            "control characters under",
            vec![("\u{1}", 10_922), ("a", 4)],
            true,
        ),
        // Two- and four-byte characters are as many bytes as JSON text.
        (
            "multi-byte at the threshold",
            vec![("\u{e9}", 32_768)],
            true,
        ),
        (
            "multi-byte one byte over",
            vec![("\u{e9}", 32_768), ("a", 1)],
            false,
        ),
    ];
    let steps: Vec<_> = cases
        .iter()
        .enumerate()
        .map(|(n, (_, parts, _))| put_string(&format!("t/{n}.txt"), parts))
        .collect();
    let results = run(&s, PUTS, 16, &steps);
    assert_eq!(results.len(), cases.len());
    for (n, (what, parts, fits)) in cases.iter().enumerate() {
        let text = text_of(parts);
        let json = json_bytes(&text);
        let context = format!("{what} (JSON text {json} bytes, UTF-8 {})", text.len());
        if *fits {
            assert!(json <= THRESHOLD, "the test's own check: {context}");
            assert_ok_put(
                &results[n],
                &format!("{context}, all 16 streams of its own in use"),
            );
        } else {
            assert!(json > THRESHOLD, "the test's own check: {context}");
            assert_eq!(
                results[n]["answer"]["error"]["kind"].as_str(),
                Some("unavailable"),
                "{context}: expected a put that needs a stream, with the request's 16 in use, `unavailable`, got {}",
                short(&results[n])
            );
        }
    }
}

#[test]
fn strings_at_the_threshold_with_multi_byte_characters_write_exactly_their_utf_8() {
    let s = puts_ready();
    // Four-byte characters (a surrogate pair in the code's string) and
    // others across the place where a 64 KiB piece would end, under the
    // threshold and over it.
    let mut cases: Vec<(String, Vec<(&str, usize)>)> = Vec::new();
    for offset in [65_531_usize, 65_532, 65_533, 65_534, 65_535] {
        cases.push((
            format!("pair after {offset} bytes"),
            vec![
                ("a", offset),
                ("\u{1F600}", 1),
                ("\u{e9}\u{20ac}", 2),
                ("z", 7),
            ],
        ));
    }
    for offset in [65_532_usize, 65_534, 65_535, 65_536, 65_537] {
        cases.push((
            format!("pairs after {offset} bytes, longer"),
            vec![
                ("a", offset),
                ("\u{1F600}", 3),
                ("\u{20ac}", 5),
                ("q", 70_000),
            ],
        ));
    }
    cases.push((
        "multi-byte only, past two pieces".to_owned(),
        vec![("\u{1F600}\u{e9}\u{20ac}", 30_000)],
    ));
    let steps: Vec<_> = cases
        .iter()
        .enumerate()
        .map(|(n, (_, parts))| put_string(&format!("exact/{n}.txt"), parts))
        .collect();
    let results = run(&s, PUTS, 0, &steps);
    for (n, (what, parts)) in cases.iter().enumerate() {
        assert_ok_put(&results[n], what);
        let expected = text_of(parts).into_bytes();
        let got = std::fs::read(s.dir(MARKETING).join(format!("exact/{n}.txt"))).unwrap();
        assert!(
            got == expected,
            "{what}: expected the file to hold exactly the string's UTF-8 ({} bytes), it holds {} bytes",
            expected.len(),
            got.len()
        );
    }
}

/// Twenty puts, one after another, each refused (or failing) before its
/// body is taken, then one that lands, with a body of `kind`.
fn refused_then_good(
    case: &str,
    app: &str,
    refused: impl Fn(usize) -> serde_json::Value,
    check: impl Fn(&serde_json::Value, usize),
    kind: &str,
) {
    let s = puts_ready();
    let mut steps: Vec<_> = (0..20).map(&refused).collect();
    let good = if kind == "bytes" {
        put_bytes(MARKETING, "good.bin", 100 * 1024)
    } else {
        put_string("good.txt", &[("\u{e9}", 51_200)])
    };
    steps.push(good);
    let results = run(&s, app, 0, &steps);
    assert_eq!(results.len(), 21, "{case}");
    for (n, result) in results[..20].iter().enumerate() {
        check(result, n);
    }
    assert_ok_put(
        &results[20],
        &format!("{case} ({kind} bodies): the put the User may make after twenty refused ones"),
    );
}

fn assert_refused_reason(result: &serde_json::Value, reason: &str, n: usize, case: &str) {
    assert_eq!(
        result["answer"]["refused"]["refusal"]["reason"].as_str(),
        Some(reason),
        "{case}: put {} of twenty: expected `refused` `{reason}` (never `unavailable` for want of a stream), got {}",
        n + 1,
        short(result)
    );
}

fn body_of(kind: &str, n: usize) -> serde_json::Value {
    if kind == "bytes" {
        json!({ "kind": "bytes", "n": n })
    } else {
        json!({ "kind": "string", "parts": [["a", n]] })
    }
}

#[test]
fn twenty_puts_refused_no_grant_hold_no_stream_afterwards() {
    for kind in ["bytes", "string"] {
        refused_then_good(
            "no Grant",
            PUTS,
            |n| json!({ "resource": LOCKED, "key": format!("l/{n}"), "body": body_of(kind, 100 * 1024) }),
            |r, n| assert_refused_reason(r, "no-grant", n, &format!("no Grant, {kind} body")),
            kind,
        );
    }
}

#[test]
fn twenty_puts_past_the_bound_for_their_other_arguments_hold_no_stream_afterwards() {
    for kind in ["bytes", "string"] {
        refused_then_good(
            "other arguments past the bound",
            PUTS,
            |_| json!({ "resource": MARKETING, "keyLen": 8 * 1024 * 1024 + 1, "body": body_of(kind, 100 * 1024) }),
            |r, n| {
                assert_eq!(
                    (
                        r["answer"]["error"]["kind"].as_str(),
                        r["answer"]["error"]["limit"].as_str()
                    ),
                    (Some("limit"), Some("arguments")),
                    "other arguments past the bound, {kind} body: put {} of twenty: expected `limit` `arguments`, got {}",
                    n + 1,
                    short(r)
                );
            },
            kind,
        );
    }
}

#[test]
fn twenty_puts_to_a_resource_the_version_did_not_declare_hold_no_stream_afterwards() {
    for kind in ["bytes", "string"] {
        refused_then_good(
            "not declared",
            NARROW,
            |n| json!({ "resource": LOCKED, "key": format!("n/{n}"), "body": body_of(kind, 100 * 1024) }),
            |r, n| {
                assert_refused_reason(r, "not-declared", n, &format!("not declared, {kind} body"))
            },
            kind,
        );
    }
}

#[test]
fn a_put_of_100_kib_of_bytes_or_of_a_string_lands_when_nothing_was_refused_before_it() {
    // The control of the tests above: the same last put, first in its request.
    let s = puts_ready();
    let results = run(
        &s,
        PUTS,
        0,
        &[
            put_bytes(MARKETING, "c/b.bin", 100 * 1024),
            put_string("c/s.txt", &[("\u{e9}", 51_200)]),
        ],
    );
    assert_ok_put(&results[0], "a put of bytes, first in its request");
    assert_ok_put(&results[1], "a put of a string, after one of bytes");
}

// ---------------------------------------------------------------------------
// Streams of the code's own: given back when a put ends, 16,384 in all, and
// cancelled when a put is refused before its body is taken.

/// A request's streams of its own, open or closed, in all.
const STREAMS_IN_ALL: usize = 16_384;

fn post_route(s: &Storage, path: &str, body: &serde_json::Value) -> serde_json::Value {
    s.post(&wanda(), PUTS, path, body).1
}

#[test]
fn forty_puts_one_after_another_in_one_request_each_give_their_stream_back() {
    let s = puts_ready();
    let body = post_route(&s, "/forty", &json!({ "resource": MARKETING }));
    for (n, result) in body["results"].as_array().unwrap().iter().enumerate() {
        assert_ok_put(
            result,
            &format!(
                "put {} of forty in one request ({})",
                n + 1,
                if n % 2 == 0 {
                    "100 KiB of bytes"
                } else {
                    "70,000 characters of string"
                }
            ),
        );
    }
    for (n, result) in body["together"].as_array().unwrap().iter().enumerate() {
        assert_ok_put(
            result,
            &format!(
                "put {} of sixteen started together after the forty (the 16 are available again)",
                n + 1
            ),
        );
    }
    let sizes: Vec<_> = body["heads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["ok"]["size"].as_u64())
        .collect();
    assert_eq!(
        sizes,
        [Some(102_400), Some(70_000), Some(102_400)],
        "expected the sizes of forty/38 (bytes), forty/39 (string) and together/15, got {}",
        short(&body["heads"])
    );
}

#[test]
fn streams_the_code_ended_but_nothing_read_are_still_open_and_count_against_the_16() {
    let s = puts_ready();
    let out = post_route(&s, "/cap", &json!({ "n": 17, "how": "end" }));
    assert_eq!(
        (out["made"].as_u64(), out["threw"]["at"].as_u64()),
        (Some(16), Some(16)),
        "expected 16 streams ended with `__responseStreamEnd` and never read to be open still, the 17th create throwing, got {}",
        short(&out)
    );
}

#[test]
fn a_request_makes_at_most_16384_streams_of_its_own_in_all_then_create_throws() {
    let s = puts_ready();
    // The boundary: exactly 16,384 made and ended, no throw, and the
    // 16,384th was the last one allowed.
    let exact = post_route(&s, "/cap", &json!({ "n": STREAMS_IN_ALL }));
    assert_eq!(
        (exact["made"].as_u64(), exact["threw"].is_null()),
        (Some(STREAMS_IN_ALL as u64), true),
        "expected 16,384 streams made and ended with no throw, got {}",
        short(&exact)
    );
    // One more: the 16,385th throws; a put of bytes needs a stream and is
    // `unavailable`; a short string travels with the arguments and lands.
    let over = post_route(
        &s,
        "/cap",
        &json!({
            "n": STREAMS_IN_ALL,
            "afterCreate": true,
            "steps": [
                { "resource": MARKETING, "key": "cap/bytes.bin", "body": { "kind": "bytes", "n": 100 * 1024 } },
                { "resource": MARKETING, "key": "cap/short.txt", "body": { "kind": "string", "parts": [["a", 1000]] } },
            ],
        }),
    );
    assert!(
        over["after"]["threw"].is_string(),
        "expected the 16,385th `__responseStreamCreate()` of a request to throw, got {}",
        short(&over)
    );
    // Not the message of the 16 open (loosely).
    assert!(
        !over["after"]["threw"]
            .as_str()
            .unwrap_or("")
            .contains("open"),
        "expected the 16,385th create to throw for the request's total, not for 16 open, got {}",
        short(&over)
    );
    let results = over["results"].as_array().unwrap();
    assert_eq!(
        results[0]["answer"]["error"]["kind"].as_str(),
        Some("unavailable"),
        "expected a put of bytes after the cap `{{ error: {{ kind: \"unavailable\" }} }}`, got {}",
        short(&results[0])
    );
    assert_ok_put(
        &results[1],
        "a put of a short string after the cap (it needs no stream)",
    );
    // The count is per request: a new request makes streams and puts bytes.
    let fresh = post_route(
        &s,
        "/cap",
        &json!({
            "n": 10,
            "afterCreate": true,
            "steps": [{ "resource": MARKETING, "key": "cap/fresh.bin", "body": { "kind": "bytes", "n": 100 * 1024 } }],
        }),
    );
    assert_eq!(fresh["after"], json!("ok"), "{}", short(&fresh));
    assert_ok_put(&fresh["results"][0], "a put of bytes in a new request");
}

fn cancelled_case(s: &Storage, request: &serde_json::Value, case: &str) {
    let out = post_route(s, "/cancel", request);
    assert!(
        out["result"]["timedOut"].is_null(),
        "{case}: expected the put to be answered with its refusal (within 5 s), got {}",
        short(&out)
    );
    let answer = &out["result"]["answer"];
    assert!(
        answer["refused"].is_object() || answer["error"]["kind"] == "limit",
        "{case}: expected the put's own refusal, got {}",
        short(&out)
    );
    assert!(
        out["ms"].as_u64().unwrap_or(u64::MAX) < 2_000,
        "{case}: expected the refusal within a second or two, it took {} ms",
        out["ms"]
    );
    assert_eq!(
        out["cancelsLater"],
        json!(1),
        "{case}: expected the code's stream cancelled once (its `cancel` called) by 200 ms after the answer, got {}",
        short(&out)
    );
    assert_eq!(
        out["pullsLater"],
        out["pullsAtAnswer"],
        "{case}: expected the stream read no further after the answer, got {}",
        short(&out)
    );
}

#[test]
fn a_stream_body_refused_before_it_is_taken_is_cancelled_and_read_no_further() {
    let s = puts_ready();
    cancelled_case(
        &s,
        &json!({ "resource": LOCKED, "key": "c/no-grant", "mode": "flowing" }),
        "no Grant",
    );
    cancelled_case(
        &s,
        &json!({ "resource": MARKETING, "keyLen": 8 * 1024 * 1024 + 1, "mode": "flowing" }),
        "limit arguments",
    );
}

#[test]
fn a_stream_body_with_a_read_pending_is_still_refused_at_once_and_cancelled() {
    let s = puts_ready();
    cancelled_case(
        &s,
        &json!({ "resource": LOCKED, "key": "c/pending", "mode": "pending" }),
        "no Grant, a read pending",
    );
    cancelled_case(
        &s,
        &json!({ "resource": MARKETING, "keyLen": 8 * 1024 * 1024 + 1, "mode": "pending" }),
        "limit arguments, a read pending",
    );
}
