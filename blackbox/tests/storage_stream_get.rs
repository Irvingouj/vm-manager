//! A file's body as a stream, read side (CONTRACT.md, "Storage: Through the
//! SDK", "Through the raw interface", "Bounds", "Who is allowed", "The
//! Operation log"; "What an Application is handed"; ADR 0019, "Bodies: a
//! handle and a stream"). A get answers metadata and a stream the host
//! feeds; a file never enters the Application's JavaScript whole unless its
//! code reads it. The tests show, with fixtures of their own
//! (`fixtures/storage-stream-*.js`, `fixtures/sdk/files`):
//!
//! - a file of 300 MiB got and served as `new Response(file.body)`: the
//!   same length and bytes, `content-length` set by the platform whatever
//!   the code set, the program's resident memory (`VmRSS`, sampled every
//!   20 ms from a warm baseline) risen by no more than 64 MiB;
//! - the head decided, the request's record and the get's entry written,
//!   with the client taking no byte of the body;
//! - the same file taken by a slow client for longer than the request's 30
//!   seconds still arriving whole;
//! - how a response is framed being the platform's, also on a connection
//!   kept for the next request;
//! - the body the file as it was opened (a put and a delete of the same key
//!   meanwhile change nothing of it), a file found shorter while it is read
//!   failing the stream, never ending as if whole;
//! - each of `arrayBuffer()`, `bytes()`, `text()` and `json()` once, the
//!   second use and a use after `body` was read a `TypeError`, `bodyUsed`;
//! - the entry of a get complete when the file is open though the body is
//!   still being read, `duration_ms` to the file's open;
//! - a disable while a file is being sent ending it within about one
//!   second, a Grant changed meanwhile not cutting it.
//!
//! Time: `a_slow_client...` takes about 35 seconds (it must outlast the
//! request's 30), the others a few seconds each. The big files are sparse,
//! written by `place_sparse` and checked byte for byte by `SparseCheck`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::admin::disable_ok;
use blackbox::body::{BodyEnd, Framing, Head, MIB, Raw, assert_framed_truly, memory_of};
use blackbox::sdk::{SDK_REQUEST_TIMEOUT, built, install};
use blackbox::storage::{
    CHUNK_BOUND, FILE_READERS, FILE_WRITERS, MARKETING, STREAM_READ, SparseCheck, Storage,
    entries_by_request, fnv1a, place, place_sparse, post_raw, sampled, wanda,
};
use serde_json::{Value, json};

/// 300 MiB: a file far past the heap of an Application's context (64 MB),
/// so it can only reach a client by the host's own sending.
const BIG: u64 = 300 * 1024 * 1024;

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

fn ok_json(s: &Storage, user: &User, application: &str, target: &str, body: &Value) -> Value {
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
    response.json()
}

/// `/serve` of `key` as [`wanda`], the answer's head read and its body
/// left unread.
fn serve(s: &Storage, key: &str, more: &Value) -> (Raw, Head) {
    let mut body = json!({ "resource": MARKETING, "key": key });
    if let (Some(o), Some(m)) = (body.as_object_mut(), more.as_object()) {
        for (k, v) in m {
            o.insert(k.clone(), v.clone());
        }
    }
    let mut raw = post_raw(&s.vm, &wanda(), STREAM_READ, "/serve", &body);
    let head = raw.read_head(Duration::from_secs(30));
    (raw, head)
}

/// Waits until the Operation log holds an entry for a get of `key` that is
/// not unfinished, up to `within`.
fn get_entry_of(s: &Storage, key: &str, within: Duration) -> Option<Value> {
    let statement = json!(format!("get {}", json!(key)));
    let mut found = None;
    blackbox::storage::eventually(within, || {
        found = s
            .log()
            .into_iter()
            .find(|e| e["operation"] == "storage.get" && e["statement"] == statement);
        found.is_some()
    });
    found
}

#[test]
fn a_file_of_300_mib_is_served_by_the_host_with_its_length_bytes_and_within_64_mib_of_memory() {
    let s = Storage::ready_streams();
    place_sparse(&s.dir(MARKETING).join("big/served.bin"), BIG);
    place(&s.dir(MARKETING).join("small.bin"), &[7_u8; 64 * 1024]);
    let pid = s.vm.pid();

    // The same route first, with a small file: whatever the program
    // allocates the first time (a context, a connection) is in the baseline.
    let (mut warm, head) = serve(&s, "small.bin", &json!({}));
    assert_eq!(head.status, 200);
    assert_eq!(
        warm.drain_body(Duration::from_secs(10), |_| {}),
        BodyEnd::Whole
    );
    let before = memory_of(pid, "VmRSS");

    let ((head, check, end), peak) = sampled(pid, || {
        // The code sets a content-length of its own: the platform sets the
        // file's, whatever the code set.
        let (mut raw, head) = serve(&s, "big/served.bin", &json!({ "contentLength": 7 }));
        let mut check = SparseCheck::new(BIG);
        let end = raw.drain_body(Duration::from_secs(120), |piece| check.take(piece));
        (head, check, end)
    });
    assert_eq!(head.status, 200, "{head:?}");
    assert_eq!(
        head.content_length(),
        Some(BIG),
        "expected `content-length` the file's size, set by the platform whatever the code set: {head:?}"
    );
    assert_eq!(
        head.header("content-type"),
        Some("application/octet-stream"),
        "{head:?}"
    );
    assert_eq!(end, BodyEnd::Whole, "expected the body whole");
    check.assert_whole("the 300 MiB file served as `new Response(file.body)`");
    let rose = peak.saturating_sub(before);
    assert!(
        rose <= 64 * MIB,
        "expected the program's resident memory (sampled every 20 ms) to rise by no more than 64 MiB serving a 300 MiB file, it rose by {} MiB (from {} to {} MiB)",
        rose / MIB,
        before / MIB,
        peak / MIB
    );
}

#[test]
fn the_head_is_decided_and_the_get_is_logged_while_the_client_takes_no_byte_of_the_body() {
    // The code has ended: the file's body is the host's to send. The
    // request's record and the get's entry are written when the head is
    // decided, not when the body ends, and `duration_ms` is to the open.
    let s = Storage::ready_streams();
    place_sparse(&s.dir(MARKETING).join("big/unread.bin"), BIG);
    let (raw, head) = serve(&s, "big/unread.bin", &json!({}));
    assert_eq!(head.status, 200, "{head:?}");
    let id = head.request_id();
    let records = s.vm.records_of_id(&id);
    assert_eq!(
        records.request["status"], 200,
        "expected the `request` record written at the head, {:?}",
        records.request
    );
    let mut entries = Vec::new();
    blackbox::storage::eventually(Duration::from_secs(5), || {
        entries = entries_by_request(&s, &id);
        entries.len() == 1 && entries[0]["outcome"] != "unfinished"
    });
    assert_eq!(
        entries.len(),
        1,
        "expected one entry for the get, got {entries:?}"
    );
    let entry = &entries[0];
    assert_eq!(
        (entry["outcome"].as_str(), entry["rows"].as_u64()),
        (Some("allowed"), Some(1)),
        "expected the get's entry complete while the body is unread: {entry}"
    );
    assert!(
        entry["duration_ms"].as_u64().is_some_and(|ms| ms < 3_000),
        "expected `duration_ms` to the file's open, not to the body's end: {entry}"
    );
    raw.go_away();
}

#[test]
fn a_slow_client_reading_past_the_requests_30_seconds_still_gets_the_file_whole() {
    // About 35 seconds: one piece of at most 64 KiB every 30 ms until 31
    // seconds have passed (never 30 seconds without a byte), then at full
    // speed. A body pumped through the code, or cut with the request's 30
    // seconds, would not arrive whole.
    let s = Storage::ready_streams();
    place_sparse(&s.dir(MARKETING).join("big/slow.bin"), BIG);
    let (mut raw, head) = serve(&s, "big/slow.bin", &json!({}));
    assert_eq!(head.status, 200, "{head:?}");
    assert_eq!(head.content_length(), Some(BIG), "{head:?}");
    let began = Instant::now();
    let mut check = SparseCheck::new(BIG);
    let end = raw.drain_body(Duration::from_secs(60), |piece| {
        check.take(piece);
        if began.elapsed() < Duration::from_secs(31) {
            std::thread::sleep(Duration::from_millis(30));
        }
    });
    let took = began.elapsed();
    assert!(
        took > Duration::from_secs(30),
        "expected the read to outlast the request's 30 seconds, it took {took:?}"
    );
    assert_eq!(
        end,
        BodyEnd::Whole,
        "expected the whole file after {took:?}"
    );
    check.assert_whole("the file taken slowly");
}

#[test]
fn how_a_files_response_is_framed_is_the_platforms_also_on_a_connection_kept_for_the_next_request()
{
    // The code sets `content-length: 5` over a file of 200 KiB, and other
    // framing headers. None is sent: the file's length is, and the next
    // request on the same connection is answered as its own.
    let s = Storage::ready_streams();
    let first: Vec<u8> = (0..200 * 1024)
        .map(|n| u8::try_from(n % 251).unwrap_or(0))
        .collect();
    let second = b"the second file, answered on the same connection";
    place(&s.dir(MARKETING).join("frame/first.bin"), &first);
    place(&s.dir(MARKETING).join("frame/second.txt"), second);
    let mut raw = Raw::connect_keep_alive(s.vm.address());
    let framing_headers = json!({
        "keep-alive": "timeout=1",
        "te": "trailers",
        "trailer": "x-after",
        "upgrade": "h2c",
        "transfer-encoding": "chunked",
        "connection": "close",
    });
    for (n, (key, bytes, more)) in [
        (
            "frame/first.bin",
            first.as_slice(),
            json!({ "contentLength": 5, "headers": framing_headers }),
        ),
        ("frame/second.txt", second.as_slice(), json!({})),
    ]
    .into_iter()
    .enumerate()
    {
        let mut body = json!({ "resource": MARKETING, "key": key });
        for (k, v) in more.as_object().unwrap() {
            body[k] = v.clone();
        }
        let sent = body.to_string().into_bytes();
        let (head, got, end) = raw.exchange(
            "POST",
            "/serve",
            &s.vm.host_of(STREAM_READ),
            Some(&wanda()),
            Some(&sent),
            Duration::from_secs(20),
        );
        assert_eq!(head.status, 200, "request {n}: {head:?}");
        assert_eq!(end, BodyEnd::Whole, "request {n}: the body whole");
        assert_eq!(
            head.content_length(),
            Some(u64::try_from(bytes.len()).unwrap()),
            "request {n}: expected `content-length` the file's size, whatever the code set: {head:?}"
        );
        assert_framed_truly(
            &head,
            u64::try_from(bytes.len()).unwrap(),
            "a file's response",
        );
        assert!(
            !head.chunked(),
            "request {n}: a file has a length, not chunks"
        );
        for name in ["keep-alive", "te", "trailer", "upgrade"] {
            assert_eq!(
                head.header(name),
                None,
                "request {n}: expected `{name}` set by the code not sent"
            );
        }
        assert!(
            !head
                .header("connection")
                .is_some_and(|c| c.eq_ignore_ascii_case("close")),
            "request {n}: expected `connection: close` set by the code not sent: {head:?}"
        );
        assert_eq!(got, bytes, "request {n}: the file's bytes");
    }
}

#[test]
fn the_body_is_the_file_as_it_was_opened_a_put_and_a_delete_meanwhile_change_nothing() {
    let s = Storage::ready_streams();
    // 24 MiB: far more than the 4 MiB the host may hold of one body, so the
    // rest is read from the file as it was opened, not from the key.
    let original: Vec<u8> = (0..24 * 1024 * 1024)
        .map(|n| u8::try_from(n % 251).unwrap_or(0))
        .collect();
    place(&s.dir(MARKETING).join("raced/file.bin"), &original);
    // The reader reads the first chunk, then waits until the key is deleted,
    // then reads on. Meanwhile the same key is put again and then deleted.
    let reader = {
        let request = s
            .vm
            .request(STREAM_READ, "/read-until")
            .as_user(&wanda())
            .method("POST")
            .json(&json!({ "resource": MARKETING, "key": "raced/file.bin", "until": "deleted" }))
            .timeout(Duration::from_secs(40));
        std::thread::spawn(move || request.try_send())
    };
    get_entry_of(&s, "raced/file.bin", Duration::from_secs(10))
        .expect("expected the reader to have opened the file");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !reader.is_finished(),
        "expected the reader to be waiting for the delete"
    );
    s.put_ok("raced/file.bin", "a different file, put meanwhile", None);
    assert_ne!(
        std::fs::read(s.dir(MARKETING).join("raced/file.bin")).unwrap(),
        original,
        "the put meanwhile replaced the file on the disk"
    );
    s.as_writer(&blackbox::storage::delete(MARKETING, "raced/file.bin"));
    assert!(
        !s.dir(MARKETING).join("raced/file.bin").exists(),
        "the delete meanwhile removed the file"
    );

    let response = reader
        .join()
        .unwrap_or_else(|e| panic!("the reading thread panicked: {e:?}"))
        .unwrap_or_else(|e| panic!("the read got no answer: {e}"));
    assert_eq!(response.status, 200, "{}", response.describe());
    let answer = response.json()["answer"].clone();
    assert_eq!(
        answer["ok"]["size"].as_u64(),
        Some(u64::try_from(original.len()).unwrap()),
        "expected the metadata of the file as it was opened: {answer}"
    );
    assert!(
        answer["waited"].as_u64().is_some_and(|ms| ms < 19_000),
        "expected the reader to have seen the delete: {answer}"
    );
    assert_eq!(
        (
            answer["read"]["length"].as_u64(),
            answer["read"]["digest"].as_u64(),
            answer["read"]["failed"].as_str()
        ),
        (
            Some(u64::try_from(original.len()).unwrap()),
            Some(u64::from(fnv1a(&original))),
            None
        ),
        "expected every byte of the file as it was opened, though it was put and deleted meanwhile: {answer}"
    );
}

#[test]
fn a_file_found_shorter_while_it_is_read_fails_the_stream_and_never_ends_as_if_whole() {
    let s = Storage::ready_streams();
    let size: u64 = 24 * 1024 * 1024;
    let bytes: Vec<u8> = (0..size)
        .map(|n| u8::try_from(n % 253).unwrap_or(0))
        .collect();
    let path = s.dir(MARKETING).join("cut/short.bin");
    place(&path, &bytes);
    let reader = {
        let request =
            s.vm.request(STREAM_READ, "/read-until")
                .as_user(&wanda())
                .method("POST")
                .json(&json!({ "resource": MARKETING, "key": "cut/short.bin", "until": "changed" }))
                .timeout(Duration::from_secs(40));
        std::thread::spawn(move || request.try_send())
    };
    get_entry_of(&s, "cut/short.bin", Duration::from_secs(10))
        .expect("expected the reader to have opened the file");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!reader.is_finished(), "expected the reader to be waiting");
    // Someone on the host makes the file shorter, in place, while it is read.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    file.set_len(64 * 1024)
        .unwrap_or_else(|e| panic!("cannot shorten {}: {e}", path.display()));
    drop(file);

    let response = reader
        .join()
        .unwrap_or_else(|e| panic!("the reading thread panicked: {e:?}"))
        .unwrap_or_else(|e| panic!("the read got no answer: {e}"));
    assert_eq!(response.status, 200, "{}", response.describe());
    let answer = response.json()["answer"].clone();
    assert!(
        answer["read"]["failed"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "expected the stream to fail when the file is found shorter, never to end as if whole: {answer}"
    );
    assert!(
        answer["read"]["length"].as_u64().unwrap_or(u64::MAX) < size,
        "expected the reader to have received fewer bytes than the size it was opened with: {answer}"
    );
}

#[test]
fn each_reader_reads_the_body_once_and_the_second_use_is_a_type_error() {
    let s = sdk_files();
    let content = "{\"greeting\":\"héllo\",\"n\":[1,2,3]}";
    place(
        &s.dir(MARKETING).join("readers/doc.json"),
        content.as_bytes(),
    );
    for (case, expected) in [
        (
            "bytes",
            json!({ "length": content.len(), "b64": blackbox::storage::b64(content.as_bytes()) }),
        ),
        (
            "arrayBuffer",
            json!({ "byteLength": content.len(), "b64": blackbox::storage::b64(content.as_bytes()) }),
        ),
        ("text", json!({ "text": content })),
        (
            "json",
            json!({ "value": { "greeting": "héllo", "n": [1, 2, 3] } }),
        ),
    ] {
        let answer = ok_json(
            &s,
            &wanda(),
            "sdk-files",
            &format!("/readers?case={case}&key=readers/doc.json"),
            &json!({}),
        );
        assert_eq!(
            answer["value"], expected,
            "expected `{case}()` to read the body to its end: {answer}"
        );
        let twice = ok_json(
            &s,
            &wanda(),
            "sdk-files",
            &format!("/read-twice?case={case}&key=readers/doc.json"),
            &json!({}),
        );
        assert_eq!(
            twice["first"], expected,
            "expected the first use of `{case}()` to read the body: {twice}"
        );
        assert_eq!(
            twice["second"]["error"]["isTypeError"], true,
            "expected a second use of `{case}()` to reject with a TypeError: {twice}"
        );
    }
    let after = ok_json(
        &s,
        &wanda(),
        "sdk-files",
        "/read-after-body?key=readers/doc.json",
        &json!({}),
    );
    assert_eq!(
        after["error"]["isTypeError"], true,
        "expected a reader used after `body` was read to reject with a TypeError: {after}"
    );
    let used = ok_json(
        &s,
        &wanda(),
        "sdk-files",
        "/body-used?key=readers/doc.json",
        &json!({}),
    );
    assert_eq!(
        (used["before"].as_bool(), used["after"].as_bool()),
        (Some(false), Some(true)),
        "expected `bodyUsed` false before the body began to be read and true once it did: {used}"
    );
}

#[test]
fn the_entry_of_a_get_is_complete_when_the_file_is_open_though_the_body_is_still_being_read() {
    let s = Storage::ready_streams();
    place_sparse(&s.dir(MARKETING).join("logged/slow.bin"), 8 * 1024 * 1024);
    // The reader reads the first chunk and then waits 6 seconds.
    let reader = {
        let request =
            s.vm.request(STREAM_READ, "/read-stall")
                .as_user(&wanda())
                .method("POST")
                .json(&json!({ "resource": MARKETING, "key": "logged/slow.bin", "pauseMs": 6000 }))
                .timeout(Duration::from_secs(30));
        std::thread::spawn(move || request.try_send())
    };
    let entry = get_entry_of(&s, "logged/slow.bin", Duration::from_secs(5))
        .expect("expected an entry for the get");
    assert!(
        !reader.is_finished(),
        "expected the body still being read when the get's entry is looked at"
    );
    assert_eq!(
        entry["outcome"], "allowed",
        "expected a get's entry complete when the file is open, though its body is still being read: {entry}"
    );
    assert_eq!(entry["rows"].as_u64(), Some(1), "{entry}");
    let duration = entry["duration_ms"]
        .as_u64()
        .unwrap_or_else(|| panic!("expected `duration_ms` a number, to the file's open: {entry}"));
    assert!(
        duration < 3_000,
        "expected `duration_ms` to the file's open, not to the end of the body read ({duration} ms): {entry}"
    );
    let response = reader
        .join()
        .unwrap_or_else(|e| panic!("the reading thread panicked: {e:?}"))
        .unwrap_or_else(|e| panic!("the slow read got no answer: {e}"));
    assert_eq!(
        response.json()["answer"]["read"]["length"].as_u64(),
        Some(8 * 1024 * 1024),
        "{}",
        response.describe()
    );
}

#[test]
fn a_disable_while_a_file_is_being_sent_ends_it_within_about_one_second() {
    let s = Storage::ready_streams();
    place_sparse(&s.dir(MARKETING).join("big/disabled.bin"), BIG);
    let (mut raw, head) = serve(&s, "big/disabled.bin", &json!({}));
    assert_eq!(head.status, 200, "{head:?}");
    assert_eq!(head.content_length(), Some(BIG), "{head:?}");
    // The client takes no byte from here on: the program fills the
    // connection and holds the rest of the body.
    let began = Instant::now();
    while raw.peek() != blackbox::body::Peeked::Data {
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "expected bytes of the body in the socket within 5 s"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    disable_ok(&s.vm, STREAM_READ);
    let disabled = Instant::now();
    let closed = raw.wait_closed_without_taking(Duration::from_secs(8));
    let closed = closed.map(|_| disabled.elapsed());
    assert!(
        closed.is_some_and(|d| d <= Duration::from_secs(3)),
        "expected the program to close the connection within about a second of the disable, it took {closed:?}"
    );
    let mut received = 0_u64;
    let end = raw.drain_body(Duration::from_secs(10), |piece| {
        received += u64::try_from(piece.len()).unwrap();
    });
    assert_eq!(
        end,
        BodyEnd::ClosedShort,
        "expected the connection closed without a clean end, short of its content-length"
    );
    assert!(
        received < BIG,
        "expected a body short of {BIG}, got {received}"
    );
}

#[test]
fn a_grant_changed_while_a_file_is_being_sent_does_not_cut_it() {
    // The decision was taken at the get. Milestone 1 cannot withdraw a
    // Grant ("Resources and Grants in full" brings that): the change here
    // is the one it can make, a writers' Grant replaced by `read`. That a
    // sending that decided again at each chunk would be caught only once a
    // Grant can be withdrawn; today this pins that a change does not cut.
    let s = Storage::ready_streams();
    let size = 100 * 1024 * 1024;
    place_sparse(&s.dir(MARKETING).join("big/granted.bin"), size);
    let (mut raw, head) = serve(&s, "big/granted.bin", &json!({}));
    assert_eq!(head.status, 200, "{head:?}");
    assert_eq!(head.content_length(), Some(size), "{head:?}");
    let mut check = SparseCheck::new(size);
    // Some of the body, then the change, then the rest.
    let mut taken = 0_u64;
    let mut changed = false;
    let end = raw.drain_body(Duration::from_secs(60), |piece| {
        check.take(piece);
        taken += u64::try_from(piece.len()).unwrap();
        if !changed && taken >= 8 * MIB {
            changed = true;
            s.grant(MARKETING, FILE_WRITERS, "read");
        }
    });
    assert!(
        changed,
        "expected the Grant replaced while the body was sent"
    );
    assert_eq!(
        end,
        BodyEnd::Whole,
        "expected the whole file after the Grant changed"
    );
    check.assert_whole("the file after the Grant changed");
}

#[test]
fn the_host_feeds_the_body_in_chunks_of_at_most_64_kib() {
    let s = Storage::ready_streams();
    let bytes: Vec<u8> = (0..512 * 1024)
        .map(|n| u8::try_from(n % 247).unwrap_or(0))
        .collect();
    place(&s.dir(MARKETING).join("chunks/measured.bin"), &bytes);
    let answer = s.read("chunks/measured.bin");
    let max_chunk = answer["read"]["maxChunk"].as_u64().unwrap_or(u64::MAX);
    assert!(
        max_chunk <= u64::try_from(CHUNK_BOUND).unwrap(),
        "expected the host to feed the body in chunks of at most {CHUNK_BOUND} bytes, got a chunk of {max_chunk}: {answer}"
    );
    assert!(
        answer["read"]["chunks"].as_u64().unwrap_or(0) >= 8,
        "expected a 512 KiB body in chunks of at most 64 KiB: {answer}"
    );
}

#[test]
fn a_file_served_through_the_sdk_as_new_response_of_its_body_arrives_whole_with_its_length() {
    let s = sdk_files();
    let size = 100 * 1024 * 1024;
    place_sparse(&s.dir(MARKETING).join("sdk/served.bin"), size);
    let mut raw = Raw::connect(s.vm.address());
    raw.send_head(
        "GET",
        "/serve?key=sdk/served.bin",
        &s.vm.host_of("sdk-files"),
        Some(&wanda()),
        Framing::None,
        &[],
    );
    let head = raw.read_head(Duration::from_secs(30));
    assert_eq!(head.status, 200, "{head:?}");
    assert_eq!(head.content_length(), Some(size), "{head:?}");
    let mut check = SparseCheck::new(size);
    let end = raw.drain_body(Duration::from_secs(60), |piece| check.take(piece));
    assert_eq!(end, BodyEnd::Whole);
    check.assert_whole("a file served through the SDK");
}
