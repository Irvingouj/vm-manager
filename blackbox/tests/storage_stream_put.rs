//! A file's body as a stream, put side (CONTRACT.md, "Storage: Through the
//! SDK", "Through the raw interface", "The Operation log", "Durability";
//! ADR 0019, "Bodies: a handle and a stream"). A put takes a stream, a
//! string, an ArrayBuffer, an ArrayBufferView or `null` (through the SDK
//! also a Blob), writes it to its staging file as it arrives, and the file
//! lands only when the body has ended cleanly.
//!
//! The tests show, with fixtures of their own (`fixtures/storage-stream-*.js`,
//! `fixtures/sdk/files`):
//!
//! - `put(key, request.body)` of a 300 MiB upload whose last two thirds are
//!   held back: while they are, the staging file holds what arrived (the
//!   body is written as it arrives, not kept), nothing is at the key, the
//!   put's entry is unfinished; then the file lands with the same bytes,
//!   the program's resident memory (`VmRSS`, sampled every 20 ms from a
//!   warm baseline) risen by no more than 64 MiB, `duration_ms` to the
//!   landing;
//! - a client that stops half way (the platform's `408`) and one that goes
//!   away: nothing at the key, nothing left in `.vibe-staging/`, a file
//!   already there untouched, the entry failed `body-failed`;
//! - every body kind the raw interface takes (a stream whose chunks are of
//!   any size, a string, an ArrayBuffer, an ArrayBufferView, `null`) and
//!   every one it refuses (`malformed`, a locked or already read stream
//!   included), and the same through the SDK (`TypeError`, nothing sent);
//! - a stream of the code's own that errors half way: `body-failed`, nothing
//!   landed; one the code tries to cancel: locked, the cancel rejects, the
//!   put goes on;
//! - a copy, `put(k2, (await get(k1)).body)`, and the body as the file was
//!   when it was opened;
//! - a put still running when its Application is disabled, or when its
//!   request ends (about 35 seconds: the request's own 30), given up.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::body::{BodyEnd, CHUNK, Framing, MIB, Raw, memory_of, pattern_block};
use blackbox::http::Response;
use blackbox::sdk::{SDK_REQUEST_TIMEOUT, built, install};
use blackbox::storage::{
    FILE_READERS, FILE_WRITERS, MARKETING, STREAM_PUT, Storage, assert_host_calls, binary_with,
    entries_by_request, eventually, fnv1a, hex, sampled, staging_sizes, tree, wanda,
};
use serde_json::{Value, json};

/// The fixture project `fixtures/sdk/files`, installed on a program with
/// `marketing` at `write`, `archive` at `read` and `devops` a database.
fn sdk_files() -> Storage {
    let built = built("files");
    let s = Storage::ready_streams();
    s.register("archive");
    s.register_postgres("devops");
    s.grant("archive", FILE_WRITERS, "write");
    s.grant("archive", FILE_READERS, "read");
    install(&s.vm, &built);
    s
}

fn ok_json(
    s: &Storage,
    user: &User,
    application: &str,
    target: &str,
    body: &Value,
) -> (Response, Value) {
    let response =
        s.vm.request(application, target)
            .as_user(user)
            .method("POST")
            .json(body)
            .timeout(SDK_REQUEST_TIMEOUT)
            .send();
    if response.status != 200 {
        s.vm.fail(&format!(
            "expected `200` with JSON from {application:?} for POST {target}, got\n{}",
            response.describe()
        ));
    }
    let json = response.json();
    (response, json)
}

/// The path of a put of the request's own body for `key`.
fn put_request_body_path(key: &str) -> String {
    format!(
        "/put-request-body?resource={MARKETING}&key={}",
        url::form_urlencoded::byte_serialize(key.as_bytes()).collect::<String>()
    )
}

/// A raw request putting its own body at `key`, `size` bytes stated, none
/// sent yet.
fn upload(s: &Storage, key: &str, size: u64) -> Raw {
    let mut raw = Raw::connect(s.vm.address());
    raw.send_head(
        "POST",
        &put_request_body_path(key),
        &s.vm.host_of(STREAM_PUT),
        Some(&wanda()),
        Framing::Length(size),
        &[("content-type", "application/octet-stream")],
    );
    raw
}

/// Waits up to `within` until the staging folder holds a file of at least
/// `at_least` bytes; its size then, if it did.
fn staged_by(s: &Storage, at_least: u64, within: Duration) -> Option<u64> {
    let mut seen = None;
    eventually(within, || {
        seen = staging_sizes(&s.dir(MARKETING))
            .into_iter()
            .find(|size| *size >= at_least);
        seen.is_some()
    });
    seen
}

/// The put's entries for `key`, whatever request they are of.
fn put_entries_of(s: &Storage, key: &str) -> Vec<Value> {
    s.log()
        .into_iter()
        .filter(|e| {
            e["operation"] == "storage.put" && e["statement"] == json!(format!("put \"{key}\""))
        })
        .collect()
}

/// Waits up to `within` for a put's entry for `key` that is failed with
/// this `error`.
fn failed_put_entry(s: &Storage, key: &str, error: &str, within: Duration) -> Option<Value> {
    let mut found = None;
    eventually(within, || {
        found = put_entries_of(s, key)
            .into_iter()
            .find(|e| e["outcome"] == "failed" && e["error"] == error);
        found.is_some()
    });
    found
}

/// Whether the file at `path` is `count` blocks of `block`.
fn file_is_blocks(path: &Path, block: &[u8], count: u64) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut got = vec![0_u8; block.len()];
    for _ in 0..count {
        if file.read_exact(&mut got).is_err() || got != block {
            return false;
        }
    }
    let mut extra = [0_u8; 1];
    file.read(&mut extra).is_ok_and(|n| n == 0)
}

fn assert_body_failed(answer: &Value, context: &str) {
    assert_eq!(
        answer["error"]["kind"].as_str(),
        Some("body-failed"),
        "{context}: expected {{ error: {{ kind: \"body-failed\" }} }}, got {answer}"
    );
}

fn assert_staging_empty(s: &Storage, context: &str) {
    assert!(
        tree(&s.dir(MARKETING).join(".vibe-staging")).is_empty(),
        "{context}: expected nothing left in `.vibe-staging/`, got {:?}",
        tree(&s.dir(MARKETING).join(".vibe-staging"))
    );
}

/// What a test saw of an upload while the rest of its body was held back.
struct Midway {
    staged: Option<u64>,
    key_at_hand: bool,
    staging_files: usize,
    entry: Option<Value>,
}

#[test]
fn a_put_of_300_mib_from_the_request_is_written_as_it_arrives_and_lands_whole_at_its_end() {
    // About 8 seconds: 100 MiB sent, the rest held back for 2 seconds while
    // the disk and the log are looked at, then sent.
    let s = Storage::ready_streams();
    let pid = s.vm.pid();
    let total: u64 = 300 * MIB;
    let key = "big/upload.bin";
    let block = pattern_block(CHUNK);
    let chunk = u64::try_from(CHUNK).unwrap();
    let blocks = total / chunk;
    let first_part = 100 * MIB / chunk;

    // The same route first, with a small body: what the program allocates
    // the first time is in the baseline.
    let mut warm = upload(&s, "warm.bin", chunk);
    warm.write(&block).unwrap();
    warm.read_head(Duration::from_secs(20));
    let before = memory_of(pid, "VmRSS");

    let mut raw = upload(&s, key, total);
    let ((midway, write_problem, head, answer, end), peak) = sampled(pid, || {
        let mut problem = None;
        for _ in 0..first_part {
            if let Err(e) = raw.write(&block) {
                problem = Some(e);
                break;
            }
        }
        // The body is written to the staging file as it arrives: most of
        // the 100 MiB is there while the other 200 MiB are held back.
        let staged = staged_by(&s, 80 * MIB, Duration::from_secs(10));
        let midway = Midway {
            staged,
            key_at_hand: s.dir(MARKETING).join(key).exists(),
            staging_files: staging_sizes(&s.dir(MARKETING)).len(),
            entry: put_entries_of(&s, key).into_iter().next(),
        };
        std::thread::sleep(Duration::from_secs(2));
        if problem.is_none() {
            for _ in first_part..blocks {
                if let Err(e) = raw.write(&block) {
                    problem = Some(e);
                    break;
                }
            }
        }
        let head = raw.try_read_head(Duration::from_secs(60));
        let mut answer = Vec::new();
        let end = if head.is_ok() {
            raw.drain_body(Duration::from_secs(30), |piece| {
                answer.extend_from_slice(piece)
            })
        } else {
            BodyEnd::StillOpen
        };
        (midway, problem, head, answer, end)
    });
    assert!(
        midway.staged.is_some(),
        "expected the host to have written the body to a staging file as it arrived (at least 80 MiB of the first 100 MiB), not kept it; staging held {:?}",
        staging_sizes(&s.dir(MARKETING))
    );
    assert!(
        !midway.key_at_hand,
        "expected nothing at the key while the body is still arriving"
    );
    assert_eq!(
        midway.staging_files, 1,
        "expected nothing but the one staging file while the put is under way"
    );
    let entry = midway
        .entry
        .unwrap_or_else(|| panic!("expected an entry for the put under way"));
    assert_eq!(
        entry["outcome"], "unfinished",
        "expected a put's entry incomplete until the file has landed: {entry}"
    );
    assert_eq!(write_problem, None, "the whole body was written");
    let head = head.unwrap_or_else(|e| panic!("no answer to the upload: {e}"));
    assert_eq!(head.status, 200, "{head:?}");
    assert_eq!(end, BodyEnd::Whole);
    let answer: Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(
        answer["answer"]["ok"]["size"].as_u64(),
        Some(total),
        "expected the metadata of the file as written: {answer}"
    );
    assert!(
        file_is_blocks(&s.dir(MARKETING).join(key), &block, blocks),
        "expected the whole upload on the disk, byte for byte"
    );
    assert_staging_empty(&s, "once the put landed");
    let rose = peak.saturating_sub(before);
    assert!(
        rose <= 64 * MIB,
        "expected the program's resident memory (sampled every 20 ms) to rise by no more than 64 MiB taking a 300 MiB upload, it rose by {} MiB",
        rose / MIB
    );
    let mut entries = Vec::new();
    eventually(Duration::from_secs(5), || {
        entries = entries_by_request(&s, &head.request_id())
            .into_iter()
            .filter(|e| e["operation"] == "storage.put" && e["outcome"] != "unfinished")
            .collect();
        !entries.is_empty()
    });
    assert_eq!(entries.len(), 1, "one entry for one put: {entries:?}");
    let entry = &entries[0];
    assert_eq!(
        (entry["outcome"].as_str(), entry["rows"].as_u64()),
        (Some("allowed"), Some(1)),
        "{entry}"
    );
    assert!(
        entry["duration_ms"].as_u64().is_some_and(|ms| ms >= 1_800),
        "expected `duration_ms` to the landing, past the 2 seconds the body was held back: {entry}"
    );
}

#[test]
fn a_client_that_stops_half_way_gets_408_and_leaves_nothing_in_staging_and_the_file_untouched() {
    // About 12 seconds: the body waits for a byte that never comes.
    let s = Storage::ready_streams();
    let key = "stopped/kept.bin";
    let before = b"already here, placed by hand";
    blackbox::storage::place(&s.dir(MARKETING).join(key), before);
    let mut raw = upload(&s, key, 64 * MIB);
    let block = pattern_block(CHUNK);
    for _ in 0..(8 * MIB / u64::try_from(CHUNK).unwrap()) {
        raw.write(&block).unwrap();
    }
    assert!(
        staged_by(&s, MIB, Duration::from_secs(10)).is_some(),
        "expected the put to have begun writing its staging file"
    );
    // The client sends no more and waits for the answer.
    let began = Instant::now();
    let head = raw
        .try_read_head(Duration::from_secs(40))
        .unwrap_or_else(|e| panic!("expected the platform to answer a body that stopped: {e}"));
    let took = began.elapsed();
    assert_eq!(
        head.status, 408,
        "expected the platform's `408` whatever the code answers: {head:?}"
    );
    assert!(
        took >= Duration::from_secs(8),
        "expected the `408` after about 10 seconds without a byte, it came after {took:?}"
    );
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join(key)).unwrap(),
        before,
        "expected the file already at the key untouched"
    );
    assert_staging_empty(&s, "after a body that stopped");
    let entries = entries_by_request(&s, &head.request_id());
    let entry =
        failed_put_entry(&s, key, "body-failed", Duration::from_secs(10)).unwrap_or_else(|| {
            panic!(
                "expected the put's entry `failed` with `error` `body-failed`, got {}",
                blackbox::scenario::lines(&entries)
            )
        });
    assert_eq!(entry["request"].as_str(), Some(head.request_id().as_str()));
}

#[test]
fn a_client_that_goes_away_half_way_leaves_nothing_in_staging_and_the_file_untouched() {
    let s = Storage::ready_streams();
    let key = "gone/kept.bin";
    let before = b"already here, placed by hand";
    blackbox::storage::place(&s.dir(MARKETING).join(key), before);
    let mut raw = upload(&s, key, 64 * MIB);
    let block = pattern_block(CHUNK);
    for _ in 0..(8 * MIB / u64::try_from(CHUNK).unwrap()) {
        raw.write(&block).unwrap();
    }
    assert!(
        staged_by(&s, MIB, Duration::from_secs(10)).is_some(),
        "expected the put to have begun writing its staging file"
    );
    raw.go_away();
    let entry = failed_put_entry(&s, key, "body-failed", Duration::from_secs(15));
    assert!(
        entry.is_some(),
        "expected the put's entry `failed` with `error` `body-failed` once the client was gone, got {}",
        blackbox::scenario::lines(&put_entries_of(&s, key))
    );
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join(key)).unwrap(),
        before,
        "expected the file already at the key untouched"
    );
    assert_staging_empty(&s, "after a client that went away");
}

#[test]
fn every_body_kind_the_raw_interface_takes_lands_and_every_other_is_malformed() {
    let s = Storage::ready_streams();
    let dir = s.dir(MARKETING);
    // A string is written as UTF-8.
    let text = "héllo ☃ — a string body";
    let (_, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/put-string",
        &json!({ "resource": MARKETING, "key": "kinds/string.txt", "body": text }),
    );
    assert!(
        answer["answer"]["ok"].is_object(),
        "a string put: expected {{ ok: <metadata> }}, got {answer}"
    );
    assert_eq!(
        std::fs::read(dir.join("kinds/string.txt")).unwrap(),
        text.as_bytes()
    );
    // An empty string is an empty file.
    let (_, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/put-string",
        &json!({ "resource": MARKETING, "key": "kinds/empty-string.txt", "body": "" }),
    );
    assert_eq!(answer["answer"]["ok"]["size"].as_u64(), Some(0), "{answer}");

    // An ArrayBuffer and an ArrayBufferView of bytes that are not UTF-8.
    let bytes = binary_with("a buffer and a view");
    for (route, key) in [
        ("/put-buffer", "kinds/buffer.bin"),
        ("/put-view", "kinds/view.bin"),
    ] {
        let (_, answer) = s.post(
            &wanda(),
            STREAM_PUT,
            route,
            &json!({ "resource": MARKETING, "key": key, "hex": hex(&bytes) }),
        );
        assert!(
            answer["answer"]["ok"].is_object(),
            "{route}: expected {{ ok: <metadata> }}, got {answer}"
        );
        assert_eq!(
            std::fs::read(dir.join(key)).unwrap(),
            bytes,
            "{route}: expected the bytes on the disk"
        );
    }

    // A stream of the code's own, in chunks (one of them empty).
    let chunks: Vec<String> = [b"one ".as_slice(), b"", b"two ", b"three"]
        .iter()
        .map(|c| hex(c))
        .collect();
    let (_, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/put-own-stream",
        &json!({ "resource": MARKETING, "key": "kinds/stream.bin", "hexChunks": chunks }),
    );
    assert!(
        answer["answer"]["ok"].is_object(),
        "a stream of the code's own: expected {{ ok: <metadata> }}, got {answer}"
    );
    assert_eq!(
        std::fs::read(dir.join("kinds/stream.bin")).unwrap(),
        b"one two three"
    );

    // `null` is an empty file.
    let (_, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/put-none",
        &json!({ "resource": MARKETING, "key": "kinds/null.bin", "kind": "null" }),
    );
    assert_eq!(
        answer["answer"]["ok"]["size"].as_u64(),
        Some(0),
        "a put of `null`: expected an empty file, got {answer}"
    );
    assert_eq!(std::fs::read(dir.join("kinds/null.bin")).unwrap(), b"");

    // Everything that is none of them is `malformed`, with no entry and
    // nothing touched.
    let before = tree(&s.place.base);
    for kind in ["number", "undefined", "boolean", "object", "array", "blob"] {
        let (response, answer) = s.post(
            &wanda(),
            STREAM_PUT,
            "/put-none",
            &json!({ "resource": MARKETING, "key": "kinds/none.bin", "kind": kind }),
        );
        if answer["answer"]["skipped"].is_string() {
            continue;
        }
        blackbox::scenario::assert_malformed(
            &answer["answer"],
            &format!("a body that is a {kind}"),
        );
        assert!(
            s.entries_of(&response).is_empty(),
            "a body that is a {kind}: no entry"
        );
    }
    for route in ["/put-locked", "/put-read"] {
        let (response, answer) = s.post(
            &wanda(),
            STREAM_PUT,
            route,
            &json!({ "resource": MARKETING, "key": "kinds/none.bin" }),
        );
        blackbox::scenario::assert_malformed(
            &answer["answer"],
            &format!("{route}: a stream that is locked or already read"),
        );
        assert!(s.entries_of(&response).is_empty(), "{route}: no entry");
    }
    assert_eq!(tree(&s.place.base), before, "expected nothing touched");
    assert!(!dir.join("kinds/none.bin").exists());
}

#[test]
fn a_streams_chunks_may_be_of_any_size_and_the_file_has_the_same_bytes() {
    // The platform's glue passes them on in pieces of at most 64 KiB: a
    // chunk of 1 MiB lands like any other.
    let s = Storage::ready_streams();
    let sizes: [usize; 9] = [1, 65_535, 65_536, 65_537, 0, 200_000, 1_048_576, 3, 131_072];
    let mut expected = Vec::new();
    for (i, size) in sizes.iter().enumerate() {
        expected.extend(std::iter::repeat_n(
            u8::try_from((i + 1) & 0xff).unwrap(),
            *size,
        ));
    }
    let (response, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/put-own-stream",
        &json!({ "resource": MARKETING, "key": "sizes/any.bin", "sizes": sizes }),
    );
    assert_eq!(
        answer["answer"]["ok"]["size"].as_u64(),
        Some(u64::try_from(expected.len()).unwrap()),
        "expected a stream with chunks of any size to land: {answer}"
    );
    let got = std::fs::read(s.dir(MARKETING).join("sizes/any.bin")).unwrap();
    assert_eq!(
        (got.len(), fnv1a(&got)),
        (expected.len(), fnv1a(&expected)),
        "expected the file to hold the same bytes as the chunks"
    );
    assert_staging_empty(&s, "after a stream of chunks of any size");
    let entry = s.entry_of(&response);
    assert_eq!(entry["outcome"], "allowed", "{entry}");
}

#[test]
fn every_body_kind_a_put_takes_through_the_sdk_and_every_one_it_refuses() {
    let s = sdk_files();
    let dir = s.dir(MARKETING);
    let bytes = b"%PDF binary \xff\xfe through the SDK";
    for (as_kind, key) in [
        ("text", "sdk/text.txt"),
        ("bytes", "sdk/bytes.bin"),
        ("buffer", "sdk/buffer.bin"),
        ("blob", "sdk/blob.bin"),
        ("dataview", "sdk/dataview.bin"),
    ] {
        let response =
            s.vm.request("sdk-files", &format!("/put?key={key}&as={as_kind}"))
                .as_user(&wanda())
                .method("POST")
                .header("content-type", "application/octet-stream")
                .body(bytes)
                .timeout(SDK_REQUEST_TIMEOUT)
                .send();
        assert_eq!(
            response.status,
            200,
            "{as_kind}: expected `200`, got\n{}",
            response.describe()
        );
        let answer = response.json();
        assert!(
            answer["info"].is_object(),
            "{as_kind}: expected a FileInfo, got {answer}"
        );
        let expected: Vec<u8> = if as_kind == "text" {
            String::from_utf8_lossy(bytes).as_bytes().to_vec()
        } else {
            bytes.to_vec()
        };
        assert_eq!(
            std::fs::read(dir.join(key)).unwrap(),
            expected,
            "{as_kind}: expected the bytes on the disk"
        );
    }
    // `null` is an empty file, and so is a request with no body.
    let (_, answer) = ok_json(
        &s,
        &wanda(),
        "sdk-files",
        "/put-null?key=sdk/null.bin",
        &json!({}),
    );
    assert_eq!(answer["info"]["size"].as_u64(), Some(0), "{answer}");
    assert_eq!(std::fs::read(dir.join("sdk/null.bin")).unwrap(), b"");
    let response =
        s.vm.request("sdk-files", "/upload?key=sdk/no-body.bin")
            .as_user(&wanda())
            .method("POST")
            .timeout(SDK_REQUEST_TIMEOUT)
            .send();
    assert_eq!(response.status, 200, "{}", response.describe());
    assert_eq!(
        response.json()["info"]["size"].as_u64(),
        Some(0),
        "expected `put(key, request.body)` of a request with no body an empty file: {}",
        response.describe()
    );
    assert_eq!(std::fs::read(dir.join("sdk/no-body.bin")).unwrap(), b"");

    // A stream the code makes, chunk by chunk: chunks of 1 MiB are cut by
    // the platform's glue; the file still holds every byte.
    let (_, answer) = ok_json(
        &s,
        &wanda(),
        "sdk-files",
        "/own-stream?key=sdk/stream.bin&chunks=4&chunkSize=1048576",
        &json!({}),
    );
    assert!(
        answer["info"].is_object(),
        "a stream the code makes: expected a FileInfo, got {answer}"
    );
    let mut expected = Vec::new();
    for n in 0..4_u8 {
        expected.extend(std::iter::repeat_n(n, 1024 * 1024));
    }
    assert_eq!(
        std::fs::read(dir.join("sdk/stream.bin")).unwrap(),
        expected,
        "expected the stream's bytes on the disk"
    );

    // None of the six, a locked stream, an already read stream: a
    // `TypeError`, and nothing is sent to the host.
    for case in [
        "body-number",
        "body-object",
        "body-missing",
        "body-boolean",
        "body-locked-stream",
        "body-read-stream",
    ] {
        let (response, answer) = ok_json(
            &s,
            &wanda(),
            "sdk-files",
            &format!("/type-error?case={case}"),
            &json!({}),
        );
        assert_eq!(
            answer["error"]["isTypeError"], true,
            "{case}: expected a TypeError, got {answer}"
        );
        assert_host_calls(
            &s.vm,
            &response,
            &[],
            &format!("{case}: nothing sent to the host"),
        );
        assert!(s.entries_of(&response).is_empty(), "{case}: no entry");
    }
}

#[test]
fn a_stream_of_the_codes_own_that_errors_half_way_fails_body_failed_and_one_it_tries_to_cancel_does_not()
 {
    let s = Storage::ready_streams();
    let dir = s.dir(MARKETING);
    let chunks: Vec<String> = ["00112233", "44556677", "8899aabb"]
        .iter()
        .map(|c| (*c).to_owned())
        .collect();
    // The stream errors after one chunk, then again after two.
    for fail_at in [1, 2] {
        let key = format!("own/failed-{fail_at}.bin");
        let (response, answer) = s.post(
            &wanda(),
            STREAM_PUT,
            "/put-own-stream",
            &json!({
                "resource": MARKETING,
                "key": key,
                "hexChunks": chunks,
                "failAt": fail_at,
            }),
        );
        assert_body_failed(
            &answer["answer"],
            &format!("a stream that errors after {fail_at} chunk(s)"),
        );
        assert!(!dir.join(&key).exists(), "expected nothing landed at {key}");
        assert_staging_empty(&s, "after a stream that errored");
        let entry = s.entry_of(&response);
        assert_eq!(
            (entry["outcome"].as_str(), entry["error"].as_str()),
            (Some("failed"), Some("body-failed")),
            "the entry of a put whose body errored: {entry}"
        );
    }
    // A put over a file already there leaves it as it was.
    s.put_ok("own/kept.bin", "kept", None);
    let (_, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/put-own-stream",
        &json!({
            "resource": MARKETING,
            "key": "own/kept.bin",
            "hexChunks": chunks,
            "failAt": 1,
        }),
    );
    assert_body_failed(&answer["answer"], "a stream that errors over a file");
    assert_eq!(std::fs::read(dir.join("own/kept.bin")).unwrap(), b"kept");
    // A stream given to a put is the put's from the call on: locked, so the
    // code cannot cancel it (`cancel()` rejects with a TypeError) and the
    // put goes on to land the stream's bytes.
    let key = "own/locked.bin";
    let (_, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/put-own-stream",
        &json!({
            "resource": MARKETING,
            "key": key,
            "hexChunks": chunks,
            "cancelAt": 0,
        }),
    );
    assert_eq!(
        answer["locked"], true,
        "expected the stream locked: {answer}"
    );
    assert_eq!(
        answer["cancel"]["isTypeError"], true,
        "expected `cancel()` of a stream given to a put to reject with a TypeError: {answer}"
    );
    assert!(
        answer["answer"]["ok"].is_object(),
        "expected the put to go on after the cancel was refused: {answer}"
    );
    assert_eq!(
        std::fs::read(dir.join(key)).unwrap(),
        [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb
        ],
        "expected every byte of the stream on the disk"
    );
    assert_staging_empty(&s, "after a put whose stream the code tried to cancel");
}

#[test]
fn a_stream_given_to_a_put_through_the_sdk_is_locked_and_cannot_be_cancelled() {
    let s = sdk_files();
    let (_, answer) = ok_json(
        &s,
        &wanda(),
        "sdk-files",
        "/own-stream-cancel?key=sdk/locked.bin",
        &json!({}),
    );
    assert_eq!(answer["locked"], true, "{answer}");
    assert_eq!(answer["cancel"]["isTypeError"], true, "{answer}");
    assert_eq!(answer["info"]["size"].as_u64(), Some(3 * 1024), "{answer}");
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("sdk/locked.bin"))
            .unwrap()
            .len(),
        3 * 1024
    );
    // The way to fail a put is to error the stream: nothing lands, a file
    // already there is untouched, nothing is left in staging.
    s.put_ok("sdk/failed.bin", "kept", None);
    let (_, failed) = ok_json(
        &s,
        &wanda(),
        "sdk-files",
        "/own-stream?key=sdk/failed.bin&chunks=3&chunkSize=1024&failAt=1",
        &json!({}),
    );
    assert_eq!(failed["error"]["kind"], "body-failed", "{failed}");
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("sdk/failed.bin")).unwrap(),
        b"kept"
    );
    assert_staging_empty(&s, "after an errored stream through the SDK");
}

#[test]
fn a_copy_moves_the_body_as_the_file_was_opened_not_as_the_key_is_now() {
    let s = sdk_files();
    // A plain copy through the SDK: put(k2, (await get(k1)).body).
    let copied_bytes = b"the file copied, placed by hand";
    blackbox::storage::place(&s.dir(MARKETING).join("copy/a.txt"), copied_bytes);
    let (_, copied) = ok_json(
        &s,
        &wanda(),
        "sdk-files",
        "/copy?from=copy/a.txt&to=copy/b.txt",
        &json!({}),
    );
    assert!(copied["info"].is_object(), "the copy: {copied}");
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("copy/b.txt")).unwrap(),
        copied_bytes
    );

    // The body is the file as it was opened: the key is put again after the
    // get and before the put of the body, and the copy still holds the old
    // file. The file is larger than one chunk, so that the host cannot have
    // read all of it at the get.
    let original: Vec<u8> = (0..512 * 1024)
        .map(|n| u8::try_from(n % 241).unwrap_or(0))
        .collect();
    blackbox::storage::place(&s.dir(MARKETING).join("copy/source.bin"), &original);
    let replace_with = "replaced before the copy's put read it";
    let (_, answer) = s.post(
        &wanda(),
        STREAM_PUT,
        "/copy-after-replace",
        &json!({
            "resource": MARKETING,
            "key": "copy/source.bin",
            "from": "copy/source.bin",
            "to": "copy/after.bin",
            "replaceWith": replace_with,
        }),
    );
    assert!(
        answer["second"]["ok"].is_object(),
        "expected the copy's put to land: {answer}"
    );
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join("copy/source.bin")).unwrap(),
        replace_with.as_bytes(),
        "expected the key put again while the body was held"
    );
    let copied = std::fs::read(s.dir(MARKETING).join("copy/after.bin")).unwrap();
    assert_eq!(
        (copied.len(), fnv1a(&copied)),
        (original.len(), fnv1a(&original)),
        "expected the file as it was opened, not the put that followed"
    );
}

#[test]
fn a_put_still_running_when_its_application_is_disabled_is_given_up_and_lands_nothing() {
    let s = Storage::ready_streams();
    let key = "disabled/put.bin";
    let before = b"placed by hand, to stay untouched";
    blackbox::storage::place(&s.dir(MARKETING).join(key), before);
    let mut raw = upload(&s, key, 64 * MIB);
    let block = pattern_block(CHUNK);
    for _ in 0..(8 * MIB / u64::try_from(CHUNK).unwrap()) {
        raw.write(&block).unwrap();
    }
    // The put is under way (its staging file has begun) when the
    // Application is disabled: it is given up, as a statement still running
    // then is cancelled.
    assert!(
        staged_by(&s, MIB, Duration::from_secs(10)).is_some(),
        "expected the put to have begun writing its staging file"
    );
    blackbox::admin::disable_ok(&s.vm, STREAM_PUT);
    let entry = failed_put_entry(&s, key, "application-disabled", Duration::from_secs(10));
    assert!(
        entry.is_some(),
        "expected the put's entry `failed` with `error` `application-disabled`, got {}",
        blackbox::scenario::lines(&put_entries_of(&s, key))
    );
    assert_eq!(
        std::fs::read(s.dir(MARKETING).join(key)).unwrap(),
        before,
        "expected the file already at the key untouched"
    );
    assert_staging_empty(&s, "after a disable");
    raw.go_away();
}

#[test]
fn a_put_still_running_when_its_request_ends_is_given_up_and_lands_nothing() {
    // About 35 seconds: the request's own 30, then the entry.
    let s = Storage::ready_streams();
    let key = "ended/put.bin";
    let started = Instant::now();
    let response =
        s.vm.request(STREAM_PUT, "/put-own-stream")
            .as_user(&wanda())
            .method("POST")
            .json(&json!({
                "resource": MARKETING,
                "key": key,
                "hexChunks": ["00112233"],
                "stallMs": 60000,
            }))
            .timeout(Duration::from_secs(45))
            .send();
    let took = started.elapsed();
    assert!(
        took <= Duration::from_secs(40),
        "expected the request answered at its own 30 seconds, it took {took:?}"
    );
    assert!(
        !s.dir(MARKETING).join(key).exists(),
        "expected nothing landed by a put whose request ended first"
    );
    let entry = failed_put_entry(&s, key, "request-ended", Duration::from_secs(10));
    assert!(
        entry.is_some(),
        "expected the put's entry `failed` with `error` `request-ended`, got {}",
        blackbox::scenario::lines(&put_entries_of(&s, key))
    );
    assert_staging_empty(&s, "after a request that ended");
    drop(response);
}
