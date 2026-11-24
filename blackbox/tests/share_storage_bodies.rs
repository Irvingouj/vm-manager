//! An Application's share of the program's open Storage bodies
//! (CONTRACT.md, "Resources and Grants in full: An Application's share of
//! what the program holds"; "Storage: Bounds"): 128 of the program's 512,
//! all the Application's requests together. An operation past the share
//! waits up to 5 seconds and fails `unavailable`, as one that finds the
//! program's bound full does; another Application is not touched; a place
//! is given back with the program's; the program's log has one
//! `share-full` record for the first refusal and none for the next one
//! within the minute.
//!
//! Not here: the share of the file jobs waiting for a file thread (16 of
//! the program's 64) cannot be provoked from outside in a test's time; the
//! contract says a test beside the code holds it.
//!
//! Time: about 25 seconds for the first test, about 20 for the second.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use blackbox::http::Response;
use blackbox::records::event;
use blackbox::storage::{MARKETING, STREAM_READ, Storage, eventually, mtime, place, wanda};
use serde_json::{Value, json};

/// A: the Application that fills its share. B: the one beside it.
const A: &str = STREAM_READ;
const B: &str = "stream-read-b";

/// Two Applications of the same fixture on one program, both with a Grant
/// on [`MARKETING`], and a file of the Resource for each to get.
fn two_applications() -> Storage {
    let s = Storage::ready_streams();
    s.install(
        B,
        "storage-stream-read.js",
        &[(MARKETING, "storage", "write")],
    );
    place(&s.dir(MARKETING).join("held.txt"), b"held");
    place(&s.dir(MARKETING).join("beside.txt"), b"beside");
    s
}

type Holder = JoinHandle<Result<Response, String>>;

/// `count` requests of `application`, each holding 16 open bodies of
/// `held.txt` for `hold_ms`.
fn holders(s: &Storage, application: &str, count: usize, hold_ms: u64) -> Vec<Holder> {
    let keys: Vec<&str> = std::iter::repeat_n("held.txt", 16).collect();
    (0..count)
        .map(|_| {
            let request =
                s.vm.request(application, "/hold-bodies")
                    .as_user(&wanda())
                    .method("POST")
                    .json(&json!({ "resource": MARKETING, "keys": keys, "holdMs": hold_ms }))
                    .timeout(Duration::from_secs(60));
            std::thread::spawn(move || request.try_send())
        })
        .collect()
}

/// Every holder's answer: `200`, and the number of bodies it held.
fn joined(handles: Vec<Holder>) -> u64 {
    let mut held = 0;
    for handle in handles {
        let response = handle
            .join()
            .unwrap_or_else(|e| panic!("a holding thread panicked: {e:?}"))
            .unwrap_or_else(|e| panic!("a holder got no answer: {e}"));
        assert_eq!(response.status, 200, "{}", response.describe());
        held += response.json()["held"].as_u64().unwrap_or(0);
    }
    held
}

/// How many gets of `held.txt` have been allowed so far, in the Operation
/// log.
fn opened(s: &Storage) -> usize {
    let statement = json!("get \"held.txt\"");
    s.log()
        .iter()
        .filter(|e| {
            e["operation"] == "storage.get"
                && e["statement"] == statement
                && e["outcome"] == "allowed"
        })
        .count()
}

/// Waits until `bodies` more gets of `held.txt` than `before` are open
/// (allowed) in the Operation log.
fn assert_open(s: &Storage, before: usize, bodies: usize) {
    let all_open = eventually(Duration::from_secs(15), || opened(s) >= before + bodies);
    assert!(
        all_open,
        "expected {bodies} bodies opened within 15 seconds"
    );
}

/// A `get` in a request of `application` of `key`, which keeps its body
/// open until the request ends: the response and how long it took.
fn get_one(s: &Storage, application: &str, key: &str) -> (Response, Value, Duration) {
    let began = Instant::now();
    let (response, answer) = s.post(
        &wanda(),
        application,
        "/get-one",
        &json!({ "resource": MARKETING, "key": key }),
    );
    (response, answer, began.elapsed())
}

/// The `share-full` records of the program's log for `application`.
fn share_full(s: &Storage, application: &str) -> Vec<Value> {
    s.vm.records()
        .into_iter()
        .filter(|r| event(r) == "share-full" && r["application"] == application)
        .collect()
}

fn assert_unavailable_after_about_5_seconds(answer: &Value, took: Duration, what: &str) {
    assert_eq!(
        answer["answer"]["error"]["kind"].as_str(),
        Some("unavailable"),
        "expected {what} `unavailable`, got {answer}"
    );
    assert!(
        took >= Duration::from_millis(4_000) && took <= Duration::from_millis(9_000),
        "expected {what} to wait about 5 seconds for a place (between 4 and 9), it answered after {took:?}"
    );
}

#[test]
fn an_application_at_128_open_bodies_waits_5_seconds_for_the_129th_while_another_is_served_at_once()
{
    let s = two_applications();
    let held_file = s.dir(MARKETING).join("held.txt");
    let mtime_before = mtime(&held_file);

    // A opens 128 bodies in 8 requests of 16 and holds them.
    let a_holders = holders(&s, A, 8, 22_000);
    assert_open(&s, 0, 128);

    // A ninth request of A: its `get` waits, then fails `unavailable`; B's
    // `get` of a file of the same Resource is answered at once meanwhile.
    let probe = {
        let request =
            s.vm.request(A, "/get-one")
                .as_user(&wanda())
                .method("POST")
                .json(&json!({ "resource": MARKETING, "key": "held.txt" }))
                .timeout(Duration::from_secs(30));
        std::thread::spawn(move || {
            let began = Instant::now();
            let sent = request.try_send();
            (sent, began.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(1_000));
    let (_, b_answer, b_took) = get_one(&s, B, "beside.txt");
    assert!(
        b_answer["answer"]["ok"].is_object(),
        "expected a get of B to be served while A is at its share, got {b_answer}"
    );
    assert!(
        b_took < Duration::from_secs(2),
        "expected B's get answered at once (under 2 seconds) while A's waits for a place, it took {b_took:?}"
    );
    let (sent, took) = probe.join().unwrap();
    let response = sent.unwrap_or_else(|e| panic!("the probe of A got no answer: {e}"));
    assert_eq!(response.status, 200, "{}", response.describe());
    assert_unavailable_after_about_5_seconds(
        &response.json(),
        took,
        "the 129th body of A (a get in a ninth request)",
    );
    let entry = s.entry_of(&response);
    assert_eq!(
        (entry["outcome"].as_str(), entry["error"].as_str()),
        (Some("failed"), Some("unavailable")),
        "expected the entry of the get refused for A's share `failed` with `error` `unavailable`: {entry}"
    );
    assert_eq!(
        std::fs::read(&held_file).unwrap(),
        b"held",
        "expected the file untouched"
    );
    assert_eq!(
        mtime(&held_file),
        mtime_before,
        "expected the file untouched"
    );

    // The program's log: one `share-full` for A, naming nothing else.
    let first = eventually(Duration::from_secs(5), || !share_full(&s, A).is_empty());
    assert!(
        first,
        "expected a `share-full` record for {A} after its first refusal within 5 seconds"
    );
    let records = share_full(&s, A);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        records[0]["what"].as_str(),
        Some("storage-bodies"),
        "{records:?}"
    );
    let text = records[0].to_string();
    for private in [MARKETING, "wanda"] {
        assert!(
            !text.contains(private),
            "expected the record to name no User and no Resource, got {text}"
        );
    }

    // A second refusal of A within the minute writes no second record.
    let (response, answer, took) = get_one(&s, A, "held.txt");
    assert_unavailable_after_about_5_seconds(&answer, took, "a second get of A past its share");
    assert_eq!(s.entry_of(&response)["error"].as_str(), Some("unavailable"));
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(
        share_full(&s, A).len(),
        1,
        "expected still one `share-full` record for A after a second refusal within the minute, got {:?}",
        share_full(&s, A)
    );
    assert!(
        share_full(&s, B).is_empty(),
        "expected no `share-full` record for B, got {:?}",
        share_full(&s, B)
    );

    // Once A's holders have ended, a get of A is answered.
    assert_eq!(
        joined(a_holders),
        128,
        "expected 8 requests holding 16 each"
    );
    let (_, answer, took) = get_one(&s, A, "held.txt");
    assert!(
        answer["answer"]["ok"].is_object() && took < Duration::from_secs(3),
        "expected a get of A to find a place once its holders ended, got {answer} after {took:?}"
    );
}

#[test]
fn a_put_of_an_application_at_128_open_bodies_waits_5_seconds_and_fails_unavailable_while_another_lands_at_once()
 {
    // About 12 seconds.
    let s = two_applications();
    let a_holders = holders(&s, A, 8, 10_000);
    assert_open(&s, 0, 128);
    // A put waits for a place of the share too: `unavailable` after about 5
    // seconds, its entry `failed` `unavailable`, no file landed; a put of B
    // meanwhile lands at once.
    let put_probe = {
        let request =
            s.vm.request(A, "/put-one")
                .as_user(&wanda())
                .method("POST")
                .json(&json!({ "resource": MARKETING, "key": "a-put.txt", "body": "from A" }))
                .timeout(Duration::from_secs(30));
        std::thread::spawn(move || {
            let began = Instant::now();
            let sent = request.try_send();
            (sent, began.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(1_000));
    let began = Instant::now();
    let (_, b_put) = s.post(
        &wanda(),
        B,
        "/put-one",
        &json!({ "resource": MARKETING, "key": "b-put.txt", "body": "from B" }),
    );
    assert!(
        b_put["answer"]["ok"].is_object() && began.elapsed() < Duration::from_secs(2),
        "expected a put of B to land at once while A is at its share, got {b_put} after {:?}",
        began.elapsed()
    );
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("b-put.txt")).unwrap(),
        b"from B"
    );
    let (sent, took) = put_probe.join().unwrap();
    let response = sent.unwrap_or_else(|e| panic!("the put of A got no answer: {e}"));
    assert_eq!(response.status, 200, "{}", response.describe());
    assert_eq!(
        response.json()["answer"]["error"]["kind"].as_str(),
        Some("unavailable"),
        "expected a put of A past its share `unavailable`, got {}",
        response.json()
    );
    assert!(
        (Duration::from_millis(4_000)..=Duration::from_millis(9_000)).contains(&took),
        "expected the put to wait about 5 seconds for a place, it answered after {took:?}"
    );
    let entry = s.entry_of(&response);
    assert_eq!(
        (entry["outcome"].as_str(), entry["error"].as_str()),
        (Some("failed"), Some("unavailable")),
        "expected the put's entry `failed` `unavailable`: {entry}"
    );
    assert!(
        !s.dir(MARKETING).join("a-put.txt").exists(),
        "expected no file at the key of the refused put"
    );
    let (_, head) = s.post(
        &wanda(),
        B,
        "/get-one",
        &json!({ "resource": MARKETING, "key": "a-put.txt" }),
    );
    assert!(
        head["answer"]["ok"].is_null(),
        "expected B to find no file at the key of A's refused put, got {head}"
    );

    assert_eq!(joined(a_holders), 128);
}

#[test]
fn a_body_read_to_its_end_or_cancelled_gives_back_a_place_of_the_share_at_once() {
    let s = two_applications();
    let keys: Vec<&str> = std::iter::repeat_n("held.txt", 16).collect();
    for (what, freeing) in [
        ("cancelled", json!({ "cancel": 0 })),
        ("read to its end", json!({ "read": 0 })),
    ] {
        // 7 requests of A hold 112 bodies; an eighth opens its 16, so A is
        // at its 128; it gives one back and gets one more at once.
        let before = opened(&s);
        let seven = holders(&s, A, 7, 9_000);
        assert_open(&s, before, 112);
        let mut body =
            json!({ "resource": MARKETING, "keys": keys, "holdMs": 0, "after": "held.txt" });
        for (k, v) in freeing.as_object().unwrap() {
            body[k] = v.clone();
        }
        let response =
            s.vm.request(A, "/hold-bodies")
                .as_user(&wanda())
                .method("POST")
                .json(&body)
                .timeout(Duration::from_secs(30))
                .send();
        assert_eq!(response.status, 200, "{}", response.describe());
        let answer = response.json();
        assert_eq!(answer["held"], json!(17), "{what}: {answer}");
        assert_eq!(
            answer["after"]["ok"], true,
            "expected a get of A at its 128 to find the place a body {what} gave back, got {answer}"
        );
        assert!(
            answer["after"]["ms"].as_u64().unwrap_or(u64::MAX) < 3_000,
            "expected no wait for the place of a body {what}, got {answer}"
        );
        assert_eq!(joined(seven), 112);
    }
}

#[test]
fn a_get_waiting_for_the_share_is_given_up_at_once_when_its_application_is_disabled() {
    // About 8 seconds: A at its 128, a ninth request's get waits; A is
    // disabled 1.5 seconds in; the get's entry is `failed` with `error`
    // `application-disabled` within about 2 seconds of the disable, not
    // after the 5 seconds of the wait.
    let s = two_applications();
    let a_holders = holders(&s, A, 8, 12_000);
    assert_open(&s, 0, 128);
    let request =
        s.vm.request(A, "/get-one")
            .as_user(&wanda())
            .method("POST")
            .json(&json!({ "resource": MARKETING, "key": "held.txt" }))
            .timeout(Duration::from_secs(30));
    let waiting = std::thread::spawn(move || request.try_send());
    std::thread::sleep(Duration::from_millis(1_500));
    blackbox::admin::disable_ok(&s.vm, A);
    let disabled_at = Instant::now();
    let response = waiting
        .join()
        .unwrap()
        .unwrap_or_else(|e| panic!("the waiting request got no answer: {e}"));
    let id = response.request_id();
    let mut entry = None;
    let seen = eventually(Duration::from_secs(3), || {
        entry = s
            .log()
            .into_iter()
            .find(|e| e["request"].as_str() == Some(id.as_str()) && e["outcome"] == "failed");
        entry.is_some()
    });
    assert!(
        seen,
        "expected the waiting get's entry `failed` within 3 seconds of the disable"
    );
    assert!(
        disabled_at.elapsed() < Duration::from_secs(4),
        "expected the wait given up soon after the disable, not after 5 seconds, it took {:?}",
        disabled_at.elapsed()
    );
    let entry = entry.unwrap();
    assert_eq!(
        entry["error"].as_str(),
        Some("application-disabled"),
        "expected `application-disabled`, not the wait's own end: {entry}"
    );
    for holder in a_holders {
        drop(holder.join());
    }
}

#[test]
fn the_share_is_the_applications_whatever_version_its_requests_run() {
    // About 12 seconds: 8 requests of A under Version 1 hold 128 bodies; a
    // Version 2 is sent meanwhile; a get of A (served by Version 2) waits
    // and fails `unavailable`.
    let s = two_applications();
    let a_holders = holders(&s, A, 8, 14_000);
    assert_open(&s, 0, 128);
    let sent =
        s.vm.api_as(&blackbox::User::new(blackbox::vm::INSTALLER))
            .send_version(
                A,
                &blackbox::storage::version_body_with_kinds(
                    &blackbox::vm::fixture_bundle("storage-stream-read.js"),
                    &[(MARKETING, "storage", "write")],
                ),
            );
    assert_eq!(sent.status, 201, "{}", sent.describe());
    assert_eq!(
        sent.json()["number"]
            .as_u64()
            .or(sent.json()["version"].as_u64()),
        Some(2),
        "the test's own precondition: a second Version, got {}",
        sent.json()
    );
    let (_, answer, took) = get_one(&s, A, "held.txt");
    assert_unavailable_after_about_5_seconds(
        &answer,
        took,
        "a get under Version 2 while Version 1's requests hold A's 128",
    );
    for holder in a_holders {
        drop(holder.join());
    }
}
