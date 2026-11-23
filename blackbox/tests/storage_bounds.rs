//! Bounds of Storage (CONTRACT.md, "Storage: Bounds"; ticket: bodies past
//! the bound are refused in the stated way; a request cannot make the
//! program hold an unbounded file in memory).
//!
//! What still stands here: a list page of 1,000 and the cursor's length.
//! The bounds of a body — a file at most 5 GiB, the request's body at most
//! 5 GiB and 64 KiB, the bodies open at once — are in
//! `blackbox/tests/storage_stream_bounds.rs`, with the bodies as streams
//! the host feeds (ADR 0019, "Bodies: a handle and a stream").
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use blackbox::storage::{MARKETING, Storage, assert_pages_hold_the_limit, listed_names, place};
use serde_json::json;

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
    // Pages of at most 1,000, exactly 1,000 when answered in under 4
    // seconds with more following, each cursor at most 2,048 bytes, until
    // the cursor is null.
    let pages = s.timed_pages(&json!({}), Some(1000));
    assert_pages_hold_the_limit(&pages, 1000, "`limit` 1,000");
    let mut count = 0;
    for (page, _) in &pages {
        let (files, _) = listed_names(page);
        assert!(
            files.len() <= 1000,
            "a page of at most 1,000: {}",
            files.len()
        );
        count += files.len();
        if let Some(cursor) = page["cursor"].as_str() {
            assert!(
                cursor.len() <= 2048,
                "expected a cursor of at most 2,048 bytes after keys of {} bytes, got {}",
                files.last().map_or(0, String::len),
                cursor.len()
            );
        }
    }
    assert_eq!(count, 1001, "the pages together");
    // A cursor after the longest key there can be.
    let longest = [
        "a".repeat(255),
        "b".repeat(255),
        "c".repeat(255),
        "d".repeat(250),
        "e".repeat(5),
    ]
    .join("/");
    s.put_ok(&longest, "x", None);
    s.put_ok("zzz", "x", None);
    // Pages of `limit` 1 (a page may be short): every cursor, the one
    // after the key of 1,024 bytes included, is at most 2,048 bytes.
    let pages = s.every_page(&json!({}), 1);
    let mut names = Vec::new();
    for page in &pages {
        if let Some(cursor) = page["cursor"].as_str() {
            assert!(
                cursor.len() <= 2048,
                "a cursor among keys of up to 1,024 bytes: {} bytes",
                cursor.len()
            );
        }
        names.extend(
            listed_names(page)
                .0
                .into_iter()
                .filter(|n| !n.starts_with(&format!("{folder}/"))),
        );
    }
    assert_eq!(names, [longest, "zzz".to_owned()]);
}
