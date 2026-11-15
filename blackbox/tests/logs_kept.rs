//! The newest 5,000 log entries of an Application are kept, and never more
//! than 5,100 (CONTRACT.md, "Versions, going back, and logs: Logs, on the
//! platform's API", "Bounds"). Apart from the other tests of logs because
//! it makes more than 5,100 requests, one after another, and waits only for
//! the last one's entry.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::Instant;

use blackbox::logs::{page, prints_bundle, read_after, wait_for_entry};
use blackbox::own_database::Own;
use blackbox::{INSTALLER, User};

const KEPT: usize = 5000;
const AT_MOST: usize = 5100;
const REQUESTS: usize = 5150;

#[test]
fn the_newest_5000_entries_are_kept_in_order_and_never_more_than_5100() {
    let own = Own::start();
    let alice = User::new(INSTALLER);
    let api = own.vm.api_as(&alice);
    api.create_with_bundle("board", &prints_bundle("v1", 0));
    let start = page(&api, "board", "").next;
    let began = Instant::now();
    let bob = User::new("bob");
    let ids: Vec<String> = (0..REQUESTS)
        .map(|i| {
            let response = own.vm.get_as(&bob, "board", &format!("/n{i}"));
            response.expect_status(200);
            response.request_id()
        })
        .collect();
    let sent = began.elapsed();
    wait_for_entry(&api, "board", ids.last().unwrap());
    let (entries, _) = read_after(&api, "board", &start, 1000);
    eprintln!(
        "{REQUESTS} requests sent in {sent:?}; their entries read after {:?}",
        began.elapsed()
    );
    let dropped: u64 = entries
        .iter()
        .map(|e| e["dropped_before"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(
        dropped, 0,
        "the harness's own check: entries were dropped, so this test cannot say which are kept"
    );
    assert!(
        (KEPT..=AT_MOST).contains(&entries.len()),
        "expected between {KEPT} and {AT_MOST} entries readable of the {REQUESTS} made, got {}",
        entries.len()
    );
    let read: Vec<&str> = entries
        .iter()
        .map(|e| e["request"].as_str().unwrap())
        .collect();
    let newest: Vec<&str> = ids[REQUESTS - entries.len()..]
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(
        read, newest,
        "expected the entries readable to be the newest, oldest first, none missing"
    );
}
