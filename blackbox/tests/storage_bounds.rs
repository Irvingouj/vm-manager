//! Bounds of Storage (CONTRACT.md, "Storage: Bounds"; ticket: bodies past
//! the bound are refused in the stated way; a request cannot make the
//! program hold an unbounded file in memory).
//!
//! A body of exactly 8 MiB is taken and one byte more is `limit` `bytes`,
//! for a put and for a get of a file placed by hand; a list page of 1,000
//! and the cursor's length; and an Application's request body of 8 MiB and
//! 64 KiB taken and one byte more `413`. (The bounds at once, 8 threads, 64
//! waiting and 256 MiB of bodies, are not provoked: the contract says no
//! black-box test can in a test's time.)
//!
//! Every test that writes 8 MiB writes it in a temporary folder removed
//! when it ends.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Duration;

use blackbox::scenario::Expected;
use blackbox::storage::{
    BODY_BOUND, MARKETING, REQUEST_BODY_BOUND, Storage, assert_metadata, assert_ok_metadata,
    assert_storage_entry, head, list, listed_names, place, storage_expected, wanda,
};
use serde_json::{Value, json};

const BODIES: &str = "bodies";

fn with_bodies() -> Storage {
    let s = Storage::ready();
    s.install(
        BODIES,
        "storage-bodies.js",
        &[(MARKETING, "storage", "write")],
    );
    s
}

fn put_made(s: &Storage, key: &str, size: usize, byte: u8, more: &Value) -> (String, Value) {
    let mut body = json!({ "resource": MARKETING, "key": key, "size": size, "byte": byte });
    if let Some(o) = more.as_object() {
        for (k, v) in o {
            body[k] = v.clone();
        }
    }
    let (response, answer) = s.post(&wanda(), BODIES, "/put-made", &body);
    (response.request_id(), answer["answer"].clone())
}

fn assert_limit_bytes(answer: &Value, context: &str) {
    assert_eq!(
        (
            answer["error"]["kind"].as_str(),
            answer["error"]["limit"].as_str()
        ),
        (Some("limit"), Some("bytes")),
        "{context}: expected {{ error: {{ kind: \"limit\", limit: \"bytes\" }} }}, got {answer}"
    );
}

#[test]
fn a_put_of_exactly_8_mib_is_taken_and_one_byte_more_is_limit_bytes() {
    let s = with_bodies();
    let (_, answer) = put_made(&s, "big/exact.bin", BODY_BOUND, 0x41, &json!({}));
    assert_ok_metadata(
        &answer,
        "big/exact.bin",
        BODY_BOUND,
        "application/octet-stream",
        "8 MiB",
    );
    let on_disk = std::fs::read(s.dir(MARKETING).join("big/exact.bin")).unwrap();
    assert!(
        on_disk.len() == BODY_BOUND && on_disk.iter().all(|b| *b == 0x41),
        "expected the 8 MiB on the disk"
    );
    let (response, got) = s.post(
        &wanda(),
        BODIES,
        "/get-made",
        &json!({ "resource": MARKETING, "key": "big/exact.bin", "size": BODY_BOUND, "byte": 0x41 }),
    );
    drop(response);
    let ok = &got["answer"]["ok"];
    assert_eq!(
        ok["body"]["same"], true,
        "expected the get of 8 MiB to answer the bytes put, got {got}"
    );
    let mut metadata = ok.clone();
    metadata.as_object_mut().unwrap().remove("body");
    assert_metadata(
        &metadata,
        "big/exact.bin",
        BODY_BOUND,
        "application/octet-stream",
        "the get of 8 MiB",
    );

    let (id, over) = put_made(&s, "big/over.bin", BODY_BOUND + 1, 0x41, &json!({}));
    assert_limit_bytes(&over, "a put of 8 MiB and one byte");
    assert!(
        !s.dir(MARKETING).join("big/over.bin").exists(),
        "nothing written"
    );
    let entry = s
        .log()
        .into_iter()
        .find(|e| e["request"] == id.as_str())
        .unwrap_or_else(|| panic!("expected an entry for the put of 8 MiB and one byte"));
    assert_storage_entry(
        &entry,
        &storage_expected(
            Expected {
                resource: Some(MARKETING),
                ran: Some(false),
                ..Expected::failed(&id, "wanda", BODIES, "bytes")
            },
            "storage.put",
            "put \"big/over.bin\"",
        ),
        "the entry of a put too large",
    );
    // Without its padding the same: the decoded length decides.
    let (_, unpadded) = put_made(
        &s,
        "big/unpadded.bin",
        BODY_BOUND + 1,
        0x41,
        &json!({ "unpadded": true }),
    );
    assert_limit_bytes(&unpadded, "8 MiB and one byte, unpadded");
    let (_, exact_unpadded) = put_made(
        &s,
        "big/unpadded.bin",
        BODY_BOUND,
        0x42,
        &json!({ "unpadded": true }),
    );
    assert_ok_metadata(
        &exact_unpadded,
        "big/unpadded.bin",
        BODY_BOUND,
        "application/octet-stream",
        "8 MiB, unpadded",
    );
    // A text longer than 8 MiB encodes to (11,184,812 characters) is past
    // the bound whatever it holds: it is not looked at, and the put has its
    // entry. 8 MiB and 3 bytes encode to 11,184,816.
    let (id, spoiled) = put_made(
        &s,
        "big/spoiled.bin",
        BODY_BOUND + 3,
        0x41,
        &json!({ "spoil": true }),
    );
    assert_limit_bytes(&spoiled, "a text past the bound that is not base64");
    let entry = s
        .log()
        .into_iter()
        .find(|e| e["request"] == id.as_str())
        .unwrap_or_else(|| panic!("expected an entry for a put of a text past the bound"));
    assert_eq!(
        (entry["outcome"].as_str(), entry["error"].as_str()),
        (Some("failed"), Some("bytes")),
        "the entry of a put of a text past the bound: {entry}"
    );
    // No longer and not base64: malformed, with no entry. 8 MiB and one
    // byte encode to 11,184,812 characters, as 8 MiB do.
    let (id, short) = put_made(
        &s,
        "big/short-spoiled.bin",
        BODY_BOUND + 1,
        0x41,
        &json!({ "spoil": true }),
    );
    assert_eq!(
        short["error"]["kind"], "malformed",
        "expected a text within the bound that is not base64 malformed, got {short}"
    );
    assert!(
        !s.log().iter().any(|e| e["request"] == id.as_str()),
        "expected no entry for a malformed put"
    );
    assert!(!s.dir(MARKETING).join("big/spoiled.bin").exists());
    assert!(!s.dir(MARKETING).join("big/short-spoiled.bin").exists());
}

#[test]
fn a_file_larger_than_8_mib_placed_by_hand_is_not_read_and_its_head_answers() {
    let s = with_bodies();
    let dir = s.dir(MARKETING);
    place(&dir.join("hand/exact.bin"), &vec![0x43; BODY_BOUND]);
    place(&dir.join("hand/over.bin"), &vec![0x43; BODY_BOUND + 1]);
    let (_, got) = s.post(
        &wanda(),
        BODIES,
        "/get-made",
        &json!({ "resource": MARKETING, "key": "hand/exact.bin", "size": BODY_BOUND, "byte": 0x43 }),
    );
    assert_eq!(
        got["answer"]["ok"]["body"]["same"], true,
        "expected a file of exactly 8 MiB placed by hand read whole, got {got}"
    );
    let (response, got) = s.post(
        &wanda(),
        BODIES,
        "/get-made",
        &json!({ "resource": MARKETING, "key": "hand/over.bin", "size": BODY_BOUND + 1, "byte": 0x43 }),
    );
    assert_limit_bytes(&got["answer"], "a get of a file of 8 MiB and one byte");
    let entry = s.entry_of(&response);
    assert_eq!(
        (entry["outcome"].as_str(), entry["error"].as_str()),
        (Some("failed"), Some("bytes")),
        "the entry of a get too large: {entry}"
    );
    assert_ok_metadata(
        &s.as_writer(&head(MARKETING, "hand/over.bin")),
        "hand/over.bin",
        BODY_BOUND + 1,
        "application/octet-stream",
        "the head of a file too large to get",
    );
}

#[test]
fn a_list_page_holds_up_to_1000_and_its_cursor_is_at_most_2048_bytes() {
    let s = Storage::ready();
    let dir = s.dir(MARKETING);
    let folder = "f".repeat(255);
    for n in 0..1001 {
        place(
            &dir.join(&folder)
                .join(format!("{n:04}-{}", "k".repeat(200))),
            b"",
        );
    }
    let page = s.as_writer(&list(MARKETING, &json!({ "limit": 1000 })));
    let (files, _) = listed_names(&page["ok"]);
    assert_eq!(files.len(), 1000, "expected a page of 1,000");
    let cursor = page["ok"]["cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a cursor: one more follows"));
    assert!(
        cursor.len() <= 2048,
        "expected a cursor of at most 2,048 bytes after a key of {} bytes, got {}",
        files.last().unwrap().len(),
        cursor.len()
    );
    let next = s.as_writer(&list(
        MARKETING,
        &json!({ "limit": 1000, "cursor": cursor }),
    ));
    assert_eq!(listed_names(&next["ok"]).0.len(), 1, "{next}");
    assert!(next["ok"]["cursor"].is_null());
    // A cursor after the longest key there can be.
    let longest = [
        "a".repeat(255),
        "b".repeat(255),
        "c".repeat(255),
        "d".repeat(250),
        "e".repeat(5),
    ]
    .join("/");
    s.put_ok(&longest, b"x", None);
    s.put_ok("zzz", b"x", None);
    let page = s.as_writer(&list(MARKETING, &json!({ "limit": 1 })));
    let cursor = page["ok"]["cursor"].as_str().unwrap();
    assert!(
        cursor.len() <= 2048,
        "a cursor after a key of 1,024 bytes: {} bytes",
        cursor.len()
    );
    assert_eq!(listed_names(&page["ok"]).0, std::slice::from_ref(&longest));
}

#[test]
fn an_applications_request_body_may_be_8_mib_and_64_kib_and_one_byte_more_is_413() {
    let s = with_bodies();
    for (size, status) in [(REQUEST_BODY_BOUND, 200), (REQUEST_BODY_BOUND + 1, 413)] {
        let response =
            s.vm.request(BODIES, "/size")
                .as_user(&wanda())
                .method("POST")
                .header("content-type", "application/octet-stream")
                .body(vec![b'u'; size])
                .timeout(Duration::from_secs(60))
                .send();
        assert_eq!(
            response.status,
            status,
            "expected a request body of {size} bytes answered `{status}`, got\n{}",
            response.describe()
        );
        if status == 200 {
            assert_eq!(
                response.json()["bytes"].as_u64(),
                u64::try_from(size).ok(),
                "expected the Application to get the whole body"
            );
        }
    }
}
