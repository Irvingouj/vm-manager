//! Bounds of a file's body as a stream (CONTRACT.md, "Storage: Bounds";
//! "What an Application is handed"; ADR 0019, "Bodies: a handle and a
//! stream"). A file is at most 5 GiB, a request's body at most 5 GiB and 64
//! KiB, 16 bodies may be open for one request (128 for one Application, 512
//! in the program) and 16
//! streams of the code's own; chunks are 64 KiB.
//!
//! The file bound is shown without writing 5 GiB through the program: a put
//! of a request stating a content-length of 5,368,709,121 fails `limit`
//! `bytes` in its place in the order, before a byte is read; a sparse file
//! of that many bytes placed by hand is refused a get the same, its `head`
//! answering; one of exactly 5,368,709,120 bytes is opened.
//!
//! Time: `sixteen_bodies...` about 15 seconds (its 17th body waits 5), the
//! program-wide 512 (four Applications at their 128 and a fifth) about 15, the others a few seconds each. The 5 GiB
//! files are sparse (`place_sparse`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::admin::disable_ok;
use blackbox::body::{Framing, Head, MIB, Raw, memory_of};
use blackbox::http::Response;
use blackbox::sdk::{SDK_REQUEST_TIMEOUT, built, install};
use blackbox::storage::{
    FILE_BOUND, FILE_READERS, FILE_WRITERS, MARKETING, REQUEST_BODY_BOUND, STREAM_HOSTILE,
    STREAM_PUT, STREAM_READ, Storage, assert_invalid_key, assert_ok_metadata, eventually, fnv1a,
    place, place_sparse, sampled, staging_sizes, tree, wanda,
};
use serde_json::{Value, json};

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

/// The path of a put of the request's own body for `key`.
fn put_request_body_path(key: &str) -> String {
    format!(
        "/put-request-body?resource={MARKETING}&key={}",
        url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>()
    )
}

/// A raw request to put its own body at `key`, with `length` stated and no
/// byte of it sent, as `user` when given.
fn stated(
    s: &Storage,
    application: &str,
    user: Option<&blackbox::User>,
    key: &str,
    length: Framing,
) -> Raw {
    let mut raw = Raw::connect(s.vm.address());
    raw.send_head(
        "POST",
        &put_request_body_path(key),
        &s.vm.host_of(application),
        user,
        length,
        &[("content-type", "application/octet-stream")],
    );
    raw
}

/// The answer of a raw request: its head and its body as JSON (null when
/// it is none).
fn answered(raw: &mut Raw, within: Duration) -> (Head, Value) {
    let head = raw.read_head(within);
    let mut body = Vec::new();
    raw.drain_body(within, |piece| body.extend_from_slice(piece));
    let json = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (head, json)
}

/// The pattern the hostile fixture writes for sizes given: a 32-bit linear
/// congruential sequence (`n = n * 1664525 + 1013904223`) continued from
/// chunk to chunk, a byte of it each time.
fn hostile_pattern(seed: u32, total: usize) -> Vec<u8> {
    let mut n = seed;
    let mut bytes = Vec::with_capacity(total);
    for _ in 0..total {
        n = n.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        bytes.push(u8::try_from(n >> 24).unwrap_or(0));
    }
    bytes
}

#[test]
fn a_put_of_a_request_stating_more_than_5_gib_fails_limit_bytes_before_a_byte_is_read() {
    // The order: invalid-key before `limit` `bytes`, `limit` `bytes` before
    // the directory, and `duration_ms` null (nothing was handed to the
    // directory). The body is the request's own, its content-length stated
    // and no byte of it sent: an answer at once is one given before a byte
    // was read (a body waited for is `408` after 10 seconds).
    let s = Storage::ready_streams();
    let over = FILE_BOUND + 1;
    let dir = s.dir(MARKETING);
    std::os::unix::fs::symlink(s.place.outside(), dir.join("etc")).unwrap();
    for (key, expected) in [
        ("../x", "invalid-key"),
        ("etc/x", "bytes"),
        ("fine.bin", "bytes"),
    ] {
        let mut raw = stated(&s, STREAM_PUT, Some(&wanda()), key, Framing::Length(over));
        let began = Instant::now();
        let (head, body) = answered(&mut raw, Duration::from_secs(20));
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{key}: expected the answer before a byte of the body was read, it took {:?}",
            began.elapsed()
        );
        assert_eq!(head.status, 200, "{key}: {head:?} {body}");
        let answer = body["answer"].clone();
        if expected == "invalid-key" {
            assert_invalid_key(&answer, "dot-segment", key);
        } else {
            assert_limit_bytes(&answer, key);
        }
        assert!(!dir.join(key).exists(), "expected nothing landed at {key}");
        assert!(
            staging_sizes(&dir).is_empty(),
            "expected nothing left in `.vibe-staging/` for {key}"
        );
        let entries = blackbox::storage::entries_by_request(&s, &head.request_id());
        assert_eq!(entries.len(), 1, "{key}: {entries:?}");
        let entry = &entries[0];
        assert_eq!(entry["error"], expected, "{entry}");
        assert!(
            entry["duration_ms"].is_null(),
            "expected `duration_ms` null: the put ended before it was handed to the directory, got {entry}"
        );
    }
    assert!(tree(&s.place.outside()).is_empty());
}

#[test]
fn a_request_stating_over_5_gib_and_64_kib_is_413_after_every_other_check_and_before_any_408() {
    let s = Storage::ready_streams();
    let over = REQUEST_BODY_BOUND + 1;
    let path = put_request_body_path("request/too-big.bin");

    // One byte over: `413` before the code runs, at once (a body waited for
    // would be `408` after 10 seconds), no entry for the request.
    let mut raw = stated(
        &s,
        STREAM_PUT,
        Some(&wanda()),
        "request/too-big.bin",
        Framing::Length(over),
    );
    let began = Instant::now();
    let head = raw.read_head(Duration::from_secs(20));
    assert_eq!(
        head.status, 413,
        "expected `413` for a body one byte over {REQUEST_BODY_BOUND}, got {head:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "expected the `413` before any wait for the body, it took {:?}",
        began.elapsed()
    );
    assert!(
        blackbox::storage::entries_by_request(&s, &head.request_id()).is_empty(),
        "expected no entry for a request answered before the code ran"
    );

    // After every other check: a request that is not signed in, to an
    // Application that is not there, or to one that is disabled, is answered
    // as that, whatever length it states.
    let disabled = "stream-read";
    disable_ok(&s.vm, disabled);
    for (what, application, user) in [
        ("not signed in", STREAM_PUT, None),
        (
            "an Application that is not there",
            "no-such-application",
            Some(wanda()),
        ),
        ("a disabled Application", disabled, Some(wanda())),
    ] {
        let control = {
            let mut raw = Raw::connect(s.vm.address());
            raw.send_head(
                "POST",
                &path,
                &s.vm.host_of(application),
                user.as_ref(),
                Framing::Length(0),
                &[],
            );
            raw.read_head(Duration::from_secs(20)).status
        };
        let mut raw = Raw::connect(s.vm.address());
        raw.send_head(
            "POST",
            &path,
            &s.vm.host_of(application),
            user.as_ref(),
            Framing::Length(over),
            &[],
        );
        let head = raw.read_head(Duration::from_secs(20));
        assert_ne!(control, 413, "{what}: the control request is not 413");
        assert_eq!(
            head.status, control,
            "{what}: expected the request answered as that whatever length it states (the same as one of no body), got {head:?}"
        );
    }

    // Exactly the bound: the request is taken (the code is given it), and
    // its put of that body is refused the file's bound before a byte is read.
    let mut raw = stated(
        &s,
        STREAM_PUT,
        Some(&wanda()),
        "request/exact.bin",
        Framing::Length(REQUEST_BODY_BOUND),
    );
    let (head, body) = answered(&mut raw, Duration::from_secs(20));
    assert_eq!(
        head.status, 200,
        "expected a request of exactly {REQUEST_BODY_BOUND} bytes taken (not `413`), got {head:?}"
    );
    assert_limit_bytes(
        &body["answer"],
        "a put of a request of exactly the request bound (past the file bound)",
    );
    assert!(!s.dir(MARKETING).join("request/exact.bin").exists());
}

#[test]
fn a_sparse_file_of_more_than_5_gib_is_not_opened_and_its_head_answers() {
    let s = Storage::ready_streams();
    let dir = s.dir(MARKETING);
    place_sparse(&dir.join("hand/over.bin"), FILE_BOUND + 1);
    let (response, answer) = s.post(
        &wanda(),
        STREAM_READ,
        "/get-unread",
        &json!({ "resource": MARKETING, "key": "hand/over.bin" }),
    );
    assert_limit_bytes(&answer["answer"], "a get of a file of 5 GiB and one byte");
    let entry = s.entry_of(&response);
    assert_eq!(
        (entry["outcome"].as_str(), entry["error"].as_str()),
        (Some("failed"), Some("bytes")),
        "the entry of a get too large: {entry}"
    );
    assert!(
        entry["duration_ms"].is_u64(),
        "expected `duration_ms` for a get handed to the directory, got {entry}"
    );
    assert_ok_metadata(
        &s.as_writer(&blackbox::storage::head(MARKETING, "hand/over.bin")),
        "hand/over.bin",
        usize::try_from(FILE_BOUND + 1).unwrap_or(0),
        "application/octet-stream",
        "the head of a file too large to get",
    );

    // Exactly the bound: the file is opened.
    place_sparse(&dir.join("hand/exact.bin"), FILE_BOUND);
    let (_, answer) = s.post(
        &wanda(),
        STREAM_READ,
        "/get-unread",
        &json!({ "resource": MARKETING, "key": "hand/exact.bin" }),
    );
    assert_eq!(
        answer["answer"]["ok"]["size"].as_u64(),
        Some(FILE_BOUND),
        "expected a file of exactly {FILE_BOUND} bytes opened, got {answer}"
    );
    assert_eq!(
        answer["answer"]["isStream"], true,
        "expected the file's body a stream: {answer}"
    );
}

#[test]
fn a_sparse_file_of_more_than_5_gib_fails_the_sdks_get_with_limit_bytes() {
    let built = built("files");
    let s = Storage::ready_streams();
    install(&s.vm, &built);
    s.register("archive");
    s.register_postgres("devops");
    s.grant("archive", FILE_WRITERS, "write");
    s.grant("archive", FILE_READERS, "read");
    place_sparse(&s.dir(MARKETING).join("hand/over.bin"), FILE_BOUND + 1);
    let response =
        s.vm.request("sdk-files", "/caught?case=get&key=hand/over.bin")
            .as_user(&wanda())
            .timeout(SDK_REQUEST_TIMEOUT)
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    let failed = response.json();
    assert_eq!(failed["error"]["kind"], "limit", "{failed}");
    assert_eq!(failed["error"]["detail"]["limit"], "bytes", "{failed}");
}

#[test]
fn sixteen_bodies_are_open_at_once_for_a_request_and_the_seventeenth_waits_5_seconds_then_fails_unavailable()
 {
    // About 20 seconds: the 17th waits its 5 seconds, then a body cancelled
    // and a body read to its end each make room.
    let s = Storage::ready_streams();
    place(&s.dir(MARKETING).join("held.txt"), b"held");
    let keys: Vec<&str> = std::iter::repeat_n("held.txt", 16).collect();
    let seventeen: Vec<&str> = std::iter::repeat_n("held.txt", 17).collect();
    let hold = |body: Value| {
        let request =
            s.vm.request(STREAM_READ, "/hold-bodies")
                .as_user(&wanda())
                .method("POST")
                .json(&body)
                .timeout(Duration::from_secs(30));
        std::thread::spawn(move || request.try_send())
    };
    let joined = |handle: std::thread::JoinHandle<Result<Response, String>>| -> Response {
        handle
            .join()
            .unwrap_or_else(|e| panic!("the holding thread panicked: {e:?}"))
            .unwrap_or_else(|e| panic!("the holder got no answer: {e}"))
    };

    // 16 bodies open: the 17th, in the same request, waits about 5 seconds.
    let response = joined(hold(
        json!({ "resource": MARKETING, "keys": seventeen, "holdMs": 0 }),
    ));
    assert_eq!(response.status, 200, "{}", response.describe());
    let answer = response.json();
    assert_eq!(answer["held"].as_u64(), Some(16), "{answer}");
    assert_eq!(
        answer["failed"]["answer"]["error"]["kind"].as_str(),
        Some("unavailable"),
        "expected the 17th body of one request `unavailable`, got {answer}"
    );
    let waited = answer["failed"]["ms"].as_u64().unwrap_or(0);
    assert!(
        (4_000..=9_000).contains(&waited),
        "expected the 17th body to wait about 5 seconds for a place, it waited {waited} ms: {answer}"
    );

    // The limit is the request's own: while one holds 16, another request
    // finds its places.
    let holder = hold(json!({ "resource": MARKETING, "keys": keys, "holdMs": 3_000 }));
    std::thread::sleep(Duration::from_millis(1_500));
    let (_, other) = s.post(
        &wanda(),
        STREAM_READ,
        "/get-one",
        &json!({ "resource": MARKETING, "key": "held.txt" }),
    );
    assert!(
        other["answer"]["ok"].is_object(),
        "expected another request to find its own places while one holds 16, got {other}"
    );
    assert_eq!(joined(holder).status, 200);

    // A body cancelled frees a place, a body read to its end frees one.
    for (what, freeing) in [
        ("cancelled", json!({ "cancel": 0 })),
        ("read to its end", json!({ "read": 0 })),
    ] {
        let mut body =
            json!({ "resource": MARKETING, "keys": keys, "holdMs": 0, "after": "held.txt" });
        for (k, v) in freeing.as_object().unwrap() {
            body[k] = v.clone();
        }
        let response = joined(hold(body));
        let answer = response.json();
        assert_eq!(
            answer["after"]["ok"], true,
            "expected a 17th get to find a place at once after a body was {what}, got {answer}"
        );
        assert!(
            answer["after"]["ms"].as_u64().unwrap_or(u64::MAX) < 3_000,
            "expected no wait for a place after a body was {what}, got {answer}"
        );
    }
}

#[test]
fn five_hundred_and_twelve_bodies_are_open_in_the_program_and_one_more_waits_then_fails_unavailable()
 {
    // About 15 seconds: four Applications of the same fixture each hold 128
    // bodies (8 requests of 16, the whole of an Application's share), which
    // is 512 in the program; a fifth Application's get waits its 5 seconds
    // for a place that is not there. (One Application stops at its 128:
    // "An Application's share of what the program holds".)
    let s = Storage::ready_streams();
    let holding = ["stream-read-2", "stream-read-3", "stream-read-4"];
    for name in holding {
        s.install(
            name,
            "storage-stream-read.js",
            &[(MARKETING, "storage", "write")],
        );
    }
    let fifth = "stream-read-5";
    s.install(
        fifth,
        "storage-stream-read.js",
        &[(MARKETING, "storage", "write")],
    );
    place(&s.dir(MARKETING).join("held.txt"), b"held");
    let keys: Vec<&str> = std::iter::repeat_n("held.txt", 16).collect();
    let holders: Vec<_> = std::iter::once(STREAM_READ)
        .chain(holding)
        .flat_map(|application| std::iter::repeat_n(application, 8))
        .map(|application| {
            let request =
                s.vm.request(application, "/hold-bodies")
                    .as_user(&wanda())
                    .method("POST")
                    .json(&json!({ "resource": MARKETING, "keys": keys, "holdMs": 12_000 }))
                    .timeout(Duration::from_secs(40));
            std::thread::spawn(move || request.try_send())
        })
        .collect();
    assert_eq!(holders.len(), 32);
    // Every holder has opened its 16 bodies: 512 gets are in the log, none
    // unfinished (a get is complete when its file is open).
    let statement = json!("get \"held.txt\"");
    let all_open = eventually(Duration::from_secs(10), || {
        s.log()
            .iter()
            .filter(|e| {
                e["operation"] == "storage.get"
                    && e["statement"] == statement
                    && e["outcome"] == "allowed"
            })
            .count()
            >= 512
    });
    assert!(all_open, "expected 512 bodies opened within 10 seconds");
    let began = Instant::now();
    let (response, answer) = s.post(
        &wanda(),
        fifth,
        "/get-one",
        &json!({ "resource": MARKETING, "key": "held.txt" }),
    );
    let took = began.elapsed();
    assert_eq!(
        answer["answer"]["error"]["kind"].as_str(),
        Some("unavailable"),
        "expected a 513th body in the program `unavailable`, got {answer}"
    );
    assert!(
        (Duration::from_millis(4_000)..=Duration::from_millis(9_000)).contains(&took),
        "expected it to wait about 5 seconds for a place, it answered after {took:?}"
    );
    let entry = s.entry_of(&response);
    assert_eq!(
        (entry["outcome"].as_str(), entry["error"].as_str()),
        (Some("failed"), Some("unavailable")),
        "{entry}"
    );
    // The program's bound is full, no Application's share: no `share-full`.
    assert!(
        s.vm.records()
            .iter()
            .all(|r| blackbox::records::event(r) != "share-full"),
        "expected no `share-full` record for a refusal because the program's bound is full"
    );
    let mut held = 0;
    for holder in holders {
        let response = holder
            .join()
            .unwrap_or_else(|e| panic!("a holding thread panicked: {e:?}"))
            .unwrap_or_else(|e| panic!("a holder got no answer: {e}"));
        assert_eq!(response.status, 200, "{}", response.describe());
        held += response.json()["held"].as_u64().unwrap_or(0);
    }
    assert_eq!(
        held, 512,
        "expected 32 requests each holding 16 bodies, four Applications' 128 each"
    );
    // The places are free again once the holders' requests ended.
    let (_, answer) = s.post(
        &wanda(),
        fifth,
        "/get-one",
        &json!({ "resource": MARKETING, "key": "held.txt" }),
    );
    assert!(
        answer["answer"]["ok"].is_object(),
        "expected a get to find a place once the holders ended, got {answer}"
    );
}

#[test]
fn a_body_whose_sending_ended_early_gives_back_its_place_so_more_than_512_may_be_sent_in_turn() {
    // 520 files of 300 MiB are each sent as a response and given up after
    // the head: a place that was not given back would be gone by the 513th
    // (and, for one Application, by the 129th: no more than 8 are sent at
    // once, so the test stays within one Application's share).
    let s = Storage::ready_streams();
    place_sparse(&s.dir(MARKETING).join("big/dropped.bin"), 300 * MIB);
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let vm_address = s.vm.address();
            let host = s.vm.host_of(STREAM_READ);
            std::thread::spawn(move || {
                let mut statuses = Vec::new();
                for _ in 0..65 {
                    let body = json!({ "resource": MARKETING, "key": "big/dropped.bin" })
                        .to_string()
                        .into_bytes();
                    let mut raw = Raw::connect(vm_address);
                    raw.send_head(
                        "POST",
                        "/serve",
                        &host,
                        Some(&wanda()),
                        Framing::Length(u64::try_from(body.len()).unwrap_or(0)),
                        &[],
                    );
                    raw.write(&body).unwrap();
                    statuses.push(raw.try_read_head(Duration::from_secs(60)).map(|h| h.status));
                    raw.go_away();
                }
                statuses
            })
        })
        .collect();
    let mut served = 0;
    for thread in threads {
        for status in thread.join().unwrap() {
            assert_eq!(
                status,
                Ok(200),
                "expected every one of the 520 files to be opened and its head sent, after {served} had been"
            );
            served += 1;
        }
    }
    assert_eq!(served, 520);
    let began = Instant::now();
    let (_, answer) = s.post(
        &wanda(),
        STREAM_READ,
        "/get-one",
        &json!({ "resource": MARKETING, "key": "big/dropped.bin" }),
    );
    assert!(
        answer["answer"]["ok"].is_object() && began.elapsed() < Duration::from_secs(4),
        "expected a get to find a place once the sendings ended, got {answer} after {:?}",
        began.elapsed()
    );
}

#[test]
fn sixteen_streams_of_the_codes_own_and_the_seventeenth_fails_unavailable_at_once() {
    let s = Storage::ready_streams();
    let began = Instant::now();
    let (response, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/own-stream-puts",
        &json!({ "resource": MARKETING, "keyPrefix": "held", "count": 16 }),
    );
    let took = began.elapsed();
    assert_eq!(
        answer["seventeenth"]["error"]["kind"].as_str(),
        Some("unavailable"),
        "expected the 17th stream of the code's own `unavailable`, got {answer}"
    );
    assert!(
        took <= Duration::from_secs(3),
        "expected the 17th stream to fail `unavailable` at once, not waited for; it took {took:?}"
    );
    assert!(
        !s.dir(MARKETING).join("held").exists(),
        "expected nothing landed by the puts whose streams never yielded"
    );
    let entries = s.entries_eventually(&response, 17);
    let mut request_ended = 0;
    let mut unavailable = 0;
    for entry in &entries {
        match (
            entry["outcome"].as_str(),
            entry["error"].as_str(),
            entry["reason"].as_str(),
        ) {
            (Some("failed"), Some("request-ended"), _) => request_ended += 1,
            (Some("failed"), Some("unavailable"), _) => unavailable += 1,
            _ => panic!("unexpected entry of the 17 puts: {entry}"),
        }
    }
    assert_eq!(
        (request_ended, unavailable),
        (16, 1),
        "expected the 16 puts given up `request-ended` and the 17th `unavailable`, got:\n{}",
        blackbox::scenario::lines(&entries)
    );
    assert!(
        staging_sizes(&s.dir(MARKETING)).is_empty(),
        "expected nothing left in `.vibe-staging/`, got {:?}",
        tree(&s.dir(MARKETING).join(".vibe-staging"))
    );
}

#[test]
fn the_runtimes_own_globals_throw_on_a_17th_stream_and_on_a_chunk_over_64_kib() {
    // Code that reaches past the SDK and the raw interface for more gets an
    // error thrown: `__responseStreamCreate()` a 17th time in one request,
    // `__responseStreamWrite(id, chunk)` with a chunk of 65,537 bytes. The
    // limit is the request's own: a second request has 16 again.
    let s = Storage::ready_streams();
    for round in 0..2 {
        let (_, answer) = s.post(&wanda(), STREAM_HOSTILE, "/globals", &json!({}));
        assert_eq!(
            (
                answer["defined"]["create"].as_str(),
                answer["defined"]["write"].as_str()
            ),
            (Some("function"), Some("function")),
            "round {round}: expected the runtime's globals to be there for a hostile Application to call: {answer}"
        );
        let creates = answer["creates"].as_array().unwrap();
        assert_eq!(creates.len(), 17, "{answer}");
        let thrown: Vec<usize> = creates
            .iter()
            .enumerate()
            .filter(|(_, c)| c["threw"].is_string())
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            thrown,
            [16],
            "round {round}: expected the 17th `__responseStreamCreate()` of a request, and only it, to throw: {answer}"
        );
        assert!(
            answer["write"]["over"]["threw"].is_string(),
            "round {round}: expected `__responseStreamWrite(id, chunk)` with a chunk of 65,537 bytes to throw: {answer}"
        );
        assert_eq!(
            answer["write"]["exact"]["ok"], true,
            "round {round}: expected a chunk of exactly 65,536 bytes taken: {answer}"
        );
    }
}

#[test]
fn a_hostile_application_opens_every_stream_and_writes_the_largest_chunks_within_128_mib() {
    // About 12 seconds. The worst one request may hold, within the two
    // bounds of "Storage: Bounds": files open at once are 16 for one request
    // (a body being read or written counts), streams of its own 16. A put
    // of a stream of its own holds one of each, an unread get only a file;
    // so 16 puts would hold 16 files and 16 streams, 16 gets only 16 files.
    // The mix is 8 unread gets of a 16 MiB file (the bodies open, nothing
    // read) and 8 puts of streams of its own, started together, each
    // writing 4 MiB in chunks of 64 KiB (the largest the glue lets
    // through): 16 files, the whole of that bound, with the program holding
    // the 8 files being written, their staging and their reading ahead at
    // the same time as the 8 reads. One more get and one more put are tried
    // while all of it is open: each finds no place, waits about 5 seconds
    // and fails `unavailable`, as "Files open at once" says (a 17th of
    // either is the bound met, not an error of the test). The program's
    // resident memory, sampled every 20 ms, must stay under 128 MiB above
    // its baseline. One such request is made; the bounds of the whole
    // program (8 threads, 64 waiting, 256 MiB of bodies) are not provoked
    // (see `storage_bounds.rs`).
    let s = Storage::ready_streams();
    let pid = s.vm.pid();
    let held: Vec<u8> = (0..16 * 1024 * 1024)
        .map(|n| u8::try_from(n % 239).unwrap_or(0))
        .collect();
    place(&s.dir(MARKETING).join("flood/held.bin"), &held);
    let bytes_each: usize = 4 * 1024 * 1024;
    let flood = |gets: usize, puts: usize, bytes_each: usize| {
        s.post(
            &wanda(),
            STREAM_HOSTILE,
            "/flood",
            &json!({
                "resource": MARKETING,
                "key": "flood/held.bin",
                "keyPrefix": "flood",
                "gets": gets,
                "puts": puts,
                "bytesEach": bytes_each,
            }),
        )
        .1
    };
    // Once small first: what the program allocates the first time is in the
    // baseline. (It also waits for the extras once: about 5 seconds.)
    flood(1, 1, 64 * 1024);
    // (Its extra put found a place and landed: not the refused one's.)
    std::fs::remove_file(s.dir(MARKETING).join("flood/extra.bin")).unwrap();
    let before = memory_of(pid, "VmRSS");
    let (answer, peak) = sampled(pid, || flood(8, 8, bytes_each));
    let empty = Vec::new();
    let gets = answer["gets"].as_array().unwrap_or(&empty);
    assert_eq!(gets.len(), 8, "{answer}");
    assert!(
        gets.iter().all(|g| g["ok"] == json!(true)),
        "expected all 8 gets of the hostile fixture to open: {answer}"
    );
    assert_eq!(
        answer["together"],
        json!(true),
        "expected all 8 puts to be open and being written at once: {answer}"
    );
    for (what, extra) in [("get", &answer["extraGet"]), ("put", &answer["extraPut"])] {
        assert_eq!(
            extra["answer"]["error"]["kind"].as_str(),
            Some("unavailable"),
            "expected the 17th file of one request (an extra {what}) `unavailable`, got {extra}"
        );
        let waited = extra["ms"].as_u64().unwrap_or(0);
        assert!(
            (4_000..=9_000).contains(&waited),
            "expected the extra {what} to wait about 5 seconds for a place, it waited {waited} ms"
        );
    }
    let puts = answer["puts"].as_array().unwrap_or(&empty);
    assert_eq!(puts.len(), 8, "{answer}");
    for (n, put) in puts.iter().enumerate() {
        assert!(
            put["ok"].is_object(),
            "put {n} of the hostile fixture: expected {{ ok: <metadata> }}, got {put}"
        );
        let got = std::fs::read(s.dir(MARKETING).join(format!("flood/{n}.bin")))
            .unwrap_or_else(|e| panic!("cannot read flood/{n}.bin: {e}"));
        let expected = hostile_pattern(u32::try_from(n + 1).unwrap_or(1), bytes_each);
        assert_eq!(
            (got.len(), fnv1a(&got)),
            (expected.len(), fnv1a(&expected)),
            "put {n}: expected the bytes the code's stream wrote"
        );
    }
    assert!(
        !s.dir(MARKETING).join("flood/extra.bin").exists(),
        "expected the refused extra put to have landed nothing"
    );
    let rose = peak.saturating_sub(before);
    assert!(
        rose <= 128 * MIB,
        "expected the program's resident memory (sampled every 20 ms) to rise by no more than 128 MiB for the hostile request, it rose by {} MiB",
        rose / MIB
    );
}

#[test]
fn a_chunk_larger_than_64_kib_is_cut_by_the_platform_and_the_file_has_the_same_bytes() {
    // Written past the SDK, through the raw interface: chunks of 1 MiB and of
    // odd sizes land whole (the glue cuts them; the native refuses a larger
    // write, which the glue never makes).
    let s = Storage::ready_streams();
    for (key, sizes) in [
        ("flood/one-mib.bin", vec![1_048_576_usize]),
        ("flood/odd.bin", vec![1, 65_537, 131_071, 7, 1_000_003]),
    ] {
        let total: usize = sizes.iter().sum();
        let (_, answer) = s.post(
            &wanda(),
            STREAM_HOSTILE,
            "/put-sizes",
            &json!({ "resource": MARKETING, "key": key, "sizes": sizes }),
        );
        assert_eq!(
            answer["answer"]["ok"]["size"].as_u64(),
            Some(u64::try_from(total).unwrap()),
            "{key}: expected a stream with chunks past 64 KiB to land: {answer}"
        );
        let got = std::fs::read(s.dir(MARKETING).join(key)).unwrap();
        // The fixture's pattern runs on from chunk to chunk (seed 7).
        let expected = hostile_pattern(7, total);
        assert_eq!(
            (got.len(), fnv1a(&got)),
            (expected.len(), fnv1a(&expected)),
            "{key}: expected the file to hold the same bytes as the chunks"
        );
    }
    assert!(
        staging_sizes(&s.dir(MARKETING)).is_empty(),
        "expected nothing left in `.vibe-staging/`"
    );
}
