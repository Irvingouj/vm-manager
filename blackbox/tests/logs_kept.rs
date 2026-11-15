//! The newest 5,000 log entries of an Application are kept (CONTRACT.md,
//! "Versions, going back, and logs: Logs, on the platform's API",
//! "Bounds"). Apart from the other tests of logs because it makes more than
//! 5,000 entries: cheaply, with an Application that prints 100 lines each
//! time its top level loads (each its own entry), so that 52 requests make
//! 5,252 entries.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::logs::{page, prints_bundle, read_after, wait_for_entry};
use blackbox::own_database::Own;
use blackbox::{INSTALLER, User};
use serde_json::Value;

const KEPT: usize = 5000;
const AT_LOAD: usize = 100;
const REQUESTS: usize = 52;

#[test]
fn the_newest_5000_entries_of_an_application_are_kept_and_older_ones_deleted() {
    let own = Own::start();
    let alice = User::new(INSTALLER);
    let api = own.vm.api_as(&alice);
    api.create_with_bundle("board", &prints_bundle("v1", AT_LOAD));
    let start = page(&api, "board", "").next;
    let mut last = String::new();
    for i in 0..REQUESTS {
        let response = own.vm.get_as(&User::new("bob"), "board", &format!("/n{i}"));
        response.expect_status(200);
        // Paced by its entry, so that entries never wait in their
        // thousands and none is dropped.
        wait_for_entry(&api, "board", &response.request_id());
        last = response.request_id();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let entries: Vec<Value> = loop {
        let (entries, _) = read_after(&api, "board", &start, 1000);
        if entries.len() <= KEPT || Instant::now() > deadline {
            break entries;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let dropped: u64 = entries
        .iter()
        .map(|e| e["dropped_before"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(
        dropped, 0,
        "the harness's own check: entries were dropped, so this test cannot count what is kept"
    );
    assert_eq!(
        entries.len(),
        KEPT,
        "expected the newest {KEPT} of the {} or more entries made kept (read within 10 s)",
        REQUESTS * (AT_LOAD + 1)
    );
    assert_eq!(
        entries.last().and_then(|e| e["request"].as_str()),
        Some(last.as_str()),
        "expected the newest entry kept"
    );
    let requests = entries.iter().filter(|e| e["request"].is_string()).count();
    assert!(
        requests < REQUESTS,
        "expected the oldest entries deleted: the entries of the first requests gone, found {requests} of {REQUESTS}"
    );
}
