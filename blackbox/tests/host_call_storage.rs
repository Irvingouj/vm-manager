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
