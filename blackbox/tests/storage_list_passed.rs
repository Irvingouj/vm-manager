//! A list's page may be short, and names passed over never make it fail
//! (CONTRACT.md, "Storage: Through the raw interface", list): a page may
//! hold fewer than `limit`, even nothing, and still have a cursor; a list
//! spends at most 5 seconds looking and answers the cursor of where it got
//! to; names it passes over cost it no more than names it answers, so a
//! folder of many of them is paged through and never fails by itself.
//!
//! Under `d/`, 20,000 empty folders, or 20,000 files whose names are no
//! keys, and after them all one file `d/zz.txt`: every way of listing
//! `d/` pages through to the cursor null, no page fails, each answers
//! within the operation's time, and the pages together answer exactly
//! `d/zz.txt`, within 30 seconds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::path::Path;
use std::time::{Duration, Instant};

use blackbox::storage::{MARKETING, Storage, list, listed_names, place};
use serde_json::json;

/// The operation's time ("Bounds"), and a second for the request around it.
const PAGE_TIME: Duration = Duration::from_secs(11);
/// The whole paging of one way of listing.
const PAGING_TIME: Duration = Duration::from_secs(30);

/// Pages through `d/` in each way (with and without the delimiter, `limit`
/// 1 and 1,000) and checks what the module says.
fn every_way_answers_only_the_file(s: &Storage, what: &str) {
    for delimiter in [false, true] {
        for limit in [1, 1000] {
            let way = format!(
                "{what}: a list of `d/` {} the delimiter, `limit` {limit}",
                if delimiter { "with" } else { "without" }
            );
            let started = Instant::now();
            let mut cursor: Option<String> = None;
            let mut names = Vec::new();
            let mut pages = 0;
            loop {
                let mut args = json!({ "prefix": "d/", "limit": limit });
                if delimiter {
                    args["delimiter"] = json!("/");
                }
                if let Some(c) = &cursor {
                    args["cursor"] = json!(c);
                }
                let asked = Instant::now();
                let answer = s.as_writer(&list(MARKETING, &args));
                let took = asked.elapsed();
                pages += 1;
                let page = &answer["ok"];
                assert!(
                    page.is_object(),
                    "{way}: page {pages} failed (a folder of names passed over never makes a list fail), got {answer}"
                );
                assert!(
                    took <= PAGE_TIME,
                    "{way}: page {pages} answered in {took:?}, past the operation's time"
                );
                let (files, folders) = listed_names(page);
                assert!(
                    files.len() + folders.len() <= limit,
                    "{way}: page {pages} holds more than `limit`: {page}"
                );
                names.extend(files);
                names.extend(folders);
                cursor = page["cursor"].as_str().map(str::to_owned);
                if cursor.is_none() {
                    break;
                }
                assert!(
                    started.elapsed() <= PAGING_TIME,
                    "{way}: still paging after {:?} ({pages} pages, {names:?} so far)",
                    started.elapsed()
                );
            }
            assert_eq!(
                names,
                ["d/zz.txt"],
                "{way}: expected the pages together to answer exactly `d/zz.txt` ({pages} pages)"
            );
            assert!(
                started.elapsed() <= PAGING_TIME,
                "{way}: the paging took {:?}, past 30 seconds ({pages} pages)",
                started.elapsed()
            );
        }
    }
}

fn zz(dir: &Path) {
    place(&dir.join("d").join("zz.txt"), b"the one file");
}

#[test]
fn a_folder_of_20000_empty_folders_before_the_one_file_is_paged_through() {
    let s = Storage::ready();
    let d = s.dir(MARKETING).join("d");
    std::fs::create_dir_all(&d).unwrap();
    for n in 0..20_000 {
        std::fs::create_dir(d.join(format!("e{n:05}"))).unwrap();
    }
    zz(&s.dir(MARKETING));
    every_way_answers_only_the_file(&s, "20,000 empty folders");
}

#[test]
fn a_folder_of_20000_names_that_are_no_keys_before_the_one_file_is_paged_through() {
    let s = Storage::ready();
    let d = s.dir(MARKETING).join("d");
    std::fs::create_dir_all(&d).unwrap();
    for n in 0..20_000 {
        std::fs::write(d.join(format!(".vibe-{n:05}")), b"").unwrap();
    }
    zz(&s.dir(MARKETING));
    every_way_answers_only_the_file(&s, "20,000 names beginning `.vibe-`");
}
