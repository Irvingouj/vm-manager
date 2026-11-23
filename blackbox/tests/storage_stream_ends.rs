//! What a Storage body holds and when it gives it back, the chunks a put's
//! stream may hold, and a wait for a place (CONTRACT.md, "Storage: Through
//! the SDK", "Storage: Bounds").
//!
//! - What the program held for a body is given back when the body stops
//!   counting, not only when the request ends.
//! - A chunk of a put's stream is bytes or a string; anything else fails the
//!   put `body-failed` and nothing lands; an empty chunk writes nothing.
//! - An operation waiting for a place is given up at once when its request
//!   ends or its Application is disabled.
//! - A put holds its place from its decision on, before its first byte.
//!
//! Slow ones say how long they take.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin::{IN_FLIGHT_ALLOWED, disable_ok};
use blackbox::body::{MIB, memory_of};
use blackbox::http::Response;
use blackbox::storage::{MARKETING, Storage, eventually, place, sampled, staging_sizes, wanda};
use serde_json::{Value, json};

/// The Application of this file (`fixtures/storage-stream-ends.js`).
const ENDS: &str = "stream-ends";

fn ends() -> Storage {
    let s = Storage::ready_streams();
    s.install(
        ENDS,
        "storage-stream-ends.js",
        &[(MARKETING, "storage", "write")],
    );
    s
}

fn post(s: &Storage, path: &str, body: &Value, timeout: Duration) -> Response {
    s.vm.request(ENDS, path)
        .as_user(&wanda())
        .method("POST")
        .json(body)
        .timeout(timeout)
        .send()
}

/// A request that gets `key` and reads it to its end `count` times (at most
/// `budget_ms`): the answer.
fn get_loop(s: &Storage, count: u64, budget_ms: u64) -> Value {
    let response = post(
        s,
        "/get-loop",
        &json!({ "resource": MARKETING, "key": "small.txt", "count": count, "budgetMs": budget_ms }),
        Duration::from_secs(60),
    );
    assert_eq!(response.status, 200, "{}", response.describe());
    response.json()
}

// ---------------------------------------------------------------------
// 1. What a body held is given back when it stops counting
// ---------------------------------------------------------------------

#[test]
fn a_request_that_gets_a_small_file_and_reads_it_to_its_end_many_times_holds_no_more_at_its_end() {
    // About 60 seconds. "A request that gets a small file and reads it to
    // its end many thousand times holds no more for it at its end than at its
    // start." From outside the program's resident memory is sampled every 20
    // ms: after a warm-up request of 300 gets and a rest (the baseline), a
    // short request of 100 gets, then one of up to 5,000 gets (or 20 seconds)
    // give the peak above the baseline of each; a request that held a list
    // entry and a channel for every get until it ended would show the long
    // one above the short one (by 4 MiB at the most: a get is 100 bytes). Then five long requests one after another:
    // the level after them and a rest is no more than 16 MiB above the
    // baseline. The numbers are printed (`--nocapture`).
    let s = ends();
    place(&s.dir(MARKETING).join("small.txt"), &[b'x'; 100]);
    let pid = s.vm.pid();
    get_loop(&s, 300, 20_000);
    std::thread::sleep(Duration::from_secs(2));
    let baseline = memory_of(pid, "VmRSS");

    let (short, peak_short) = sampled(pid, || get_loop(&s, 100, 20_000));
    assert_eq!(short["done"], 100, "{short}");
    let (long, peak_long) = sampled(pid, || get_loop(&s, 5_000, 20_000));
    let done = long["done"].as_u64().unwrap_or(0);
    assert!(
        done >= 1_000,
        "the long request got only {done} gets in its time: {long}"
    );
    eprintln!(
        "baseline {} MiB, short peak {} MiB, long ({done} gets) peak {} MiB",
        baseline / MIB,
        peak_short / MIB,
        peak_long / MIB
    );
    let rise_short = peak_short.saturating_sub(baseline);
    let rise_long = peak_long.saturating_sub(baseline);
    assert!(
        rise_long <= rise_short + 4 * MIB,
        "expected a request of {done} gets to rise no more than 4 MiB above one of 100 (peaks above the baseline: {} MiB and {} MiB): what a body held is to be given back when it stops counting",
        rise_long / MIB,
        rise_short / MIB
    );
    let mut total = 0;
    for _ in 0..5 {
        total += get_loop(&s, 5_000, 20_000)["done"].as_u64().unwrap_or(0);
    }
    std::thread::sleep(Duration::from_secs(2));
    let after = memory_of(pid, "VmRSS");
    eprintln!("after {total} more gets: {} MiB", after / MIB);
    assert!(
        after.saturating_sub(baseline) <= 16 * MIB,
        "expected the program's resident memory no more than 16 MiB above its baseline after {total} gets, it is {} MiB above",
        after.saturating_sub(baseline) / MIB
    );
}

// ---------------------------------------------------------------------
// 2. A chunk that is not bytes
// ---------------------------------------------------------------------

#[test]
fn a_chunk_of_a_puts_stream_that_is_not_bytes_or_a_string_fails_the_put_and_nothing_lands() {
    // About 10 seconds. Between the bytes `AB` and `CD` a number, an object,
    // `null`, `undefined` or a boolean: the put fails `body-failed`, a file
    // already at the key is untouched, nothing is left in staging, and the
    // entry is `failed` `body-failed`.
    let s = ends();
    let dir = s.dir(MARKETING);
    for kind in ["number", "object", "null", "undefined", "boolean"] {
        let key = format!("chunks/{kind}.bin");
        place(&dir.join(&key), b"existing");
        let response = post(
            &s,
            "/own-chunks",
            &json!({ "resource": MARKETING, "key": key, "kind": kind }),
            Duration::from_secs(20),
        );
        assert_eq!(response.status, 200, "{kind}: {}", response.describe());
        let answer = response.json();
        assert_eq!(
            answer["answer"]["error"]["kind"].as_str(),
            Some("body-failed"),
            "{kind}: expected the put to fail `body-failed`, got {answer}"
        );
        assert_eq!(
            std::fs::read(dir.join(&key)).unwrap(),
            b"existing",
            "{kind}: expected the file already there untouched"
        );
        assert!(
            staging_sizes(&dir).is_empty(),
            "{kind}: expected nothing left in `.vibe-staging/`"
        );
        let entries = s.entries_eventually(&response, 1);
        let entry = entries.last().expect("an entry");
        assert_eq!(
            (entry["outcome"].as_str(), entry["error"].as_str()),
            (Some("failed"), Some("body-failed")),
            "{kind}: {entries:?}"
        );
        // And with no file there: none lands.
        let fresh = format!("chunks/fresh-{kind}.bin");
        post(
            &s,
            "/own-chunks",
            &json!({ "resource": MARKETING, "key": fresh, "kind": kind }),
            Duration::from_secs(20),
        );
        assert!(
            !dir.join(&fresh).exists(),
            "{kind}: expected nothing to land at a key with no file"
        );
    }
}

#[test]
fn a_string_chunk_is_written_as_utf_8_and_an_empty_chunk_writes_nothing() {
    // A few seconds. `AB`, then `é` (written as UTF-8), then `CD`: the file
    // holds `AB`, 0xC3 0xA9, `CD`. An empty `Uint8Array` between them writes
    // nothing: `ABCD`. An `ArrayBuffer` and a `Uint16Array` are bytes.
    let s = ends();
    let dir = s.dir(MARKETING);
    for (kind, expected) in [
        ("string", b"AB\xC3\xA9CD".to_vec()),
        ("empty", b"ABCD".to_vec()),
        ("arraybuffer", b"ABxyCD".to_vec()),
        ("view", b"ABxyCD".to_vec()),
    ] {
        let key = format!("chunks/{kind}.bin");
        let response = post(
            &s,
            "/own-chunks",
            &json!({ "resource": MARKETING, "key": key, "kind": kind }),
            Duration::from_secs(20),
        );
        let answer = response.json();
        assert!(
            answer["answer"]["ok"].is_object(),
            "{kind}: expected the put to land, got {answer}"
        );
        assert_eq!(
            std::fs::read(dir.join(&key)).unwrap(),
            expected,
            "{kind}: expected these bytes"
        );
    }
}

// ---------------------------------------------------------------------
// 3. A wait for a place is given up with its request
// ---------------------------------------------------------------------

/// The entry of the 17th get of `id` (the last of its 17 gets), waiting up
/// to `within` for it to be `failed`.
fn seventeenth_entry(s: &Storage, id: &str, within: Duration) -> Option<Value> {
    let mut found = None;
    eventually(within, || {
        let entries: Vec<Value> = s
            .log()
            .into_iter()
            .filter(|e| e["request"].as_str() == Some(id))
            .collect();
        found = entries.iter().find(|e| e["outcome"] == "failed").cloned();
        found.is_some()
    });
    found
}

#[test]
fn a_get_waiting_for_a_place_is_given_up_when_the_application_is_disabled() {
    // About 6 seconds. A request holds 16 bodies open unread and starts a
    // 17th get, which waits for a place; the Application is disabled 1.5
    // seconds in. The request is answered as a disabled Application answers
    // within the disable's second, and the 17th get's entry is `failed` with
    // `error` `application-disabled` (not `unavailable`), soon after the
    // disable and not 5 seconds after the wait began.
    let s = ends();
    place(&s.dir(MARKETING).join("held.txt"), b"held");
    let request =
        s.vm.request(ENDS, "/wait-17")
            .as_user(&wanda())
            .method("POST")
            .json(&json!({ "resource": MARKETING, "key": "held.txt", "mode": "await" }))
            .timeout(Duration::from_secs(30));
    let holder = std::thread::spawn(move || request.try_send());
    std::thread::sleep(Duration::from_millis(1_500));
    disable_ok(&s.vm, ENDS);
    let disabled_at = Instant::now();
    let response = holder
        .join()
        .expect("the holder's thread ended")
        .unwrap_or_else(|e| panic!("the holder got no answer: {e}"));
    let answered_after = disabled_at.elapsed();
    let plain = s.vm.get_as(&wanda(), ENDS, "/wait-17");
    assert_eq!(
        response.status,
        plain.status,
        "expected a disabled Application's answer, got {}",
        response.describe()
    );
    assert!(
        answered_after <= IN_FLIGHT_ALLOWED,
        "expected the answer within 1 s of the disable's 200 ({IN_FLIGHT_ALLOWED:?} allowed), it took {answered_after:?}"
    );
    let entry = seventeenth_entry(&s, &response.request_id(), Duration::from_secs(3))
        .unwrap_or_else(|| {
            panic!("expected the 17th get's entry to be `failed` within 3 s of the disable")
        });
    assert_eq!(
        entry["error"].as_str(),
        Some("application-disabled"),
        "expected `application-disabled`, not the wait's own end: {entry}"
    );
}

#[test]
fn a_get_waiting_for_a_place_is_given_up_when_its_request_ends() {
    // About 3 seconds. The code starts the 17th get without awaiting it and
    // answers at once: the request ends during the wait, and the get's entry
    // is `failed` with `error` `request-ended`, soon (not after 5 seconds).
    let s = ends();
    place(&s.dir(MARKETING).join("held.txt"), b"held");
    let response = post(
        &s,
        "/wait-17",
        &json!({ "resource": MARKETING, "key": "held.txt", "mode": "no-await" }),
        Duration::from_secs(30),
    );
    assert_eq!(response.status, 200, "{}", response.describe());
    let ended = Instant::now();
    let entry = seventeenth_entry(&s, &response.request_id(), Duration::from_secs(3))
        .unwrap_or_else(|| {
            panic!(
                "expected the 17th get's entry to be `failed` within 3 s of its request's end ({:?})",
                ended.elapsed()
            )
        });
    assert_eq!(
        entry["error"].as_str(),
        Some("request-ended"),
        "expected `request-ended`, not `unavailable` after the 5 seconds: {entry}"
    );
}

// ---------------------------------------------------------------------
// 5. A put holds its place from its decision on
// ---------------------------------------------------------------------

#[test]
fn a_put_holds_its_place_before_its_first_byte_so_a_17th_operation_waits() {
    // About 6 seconds. 16 puts of streams of the code's own are started,
    // none given a chunk yet (their sources wait on a gate); at once, with no
    // pause, a get of a file is the 17th operation of the request: it waits
    // about 5 seconds for a place and fails `unavailable`.
    let s = ends();
    place(&s.dir(MARKETING).join("held.txt"), b"held");
    let response = post(
        &s,
        "/puts-then-op",
        &json!({ "resource": MARKETING, "keyPrefix": "gated", "key": "held.txt", "count": 16 }),
        Duration::from_secs(30),
    );
    assert_eq!(response.status, 200, "{}", response.describe());
    let answer = response.json();
    assert_eq!(
        answer["answer"]["error"]["kind"].as_str(),
        Some("unavailable"),
        "expected the 17th operation `unavailable` while 16 puts hold their places, got {answer}"
    );
    let waited = answer["ms"].as_u64().unwrap_or(0);
    assert!(
        (4_000..=9_000).contains(&waited),
        "expected the 17th operation to wait about 5 seconds for a place, it waited {waited} ms: {answer}"
    );
}
