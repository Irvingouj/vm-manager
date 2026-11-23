//! A request's body on the platform's own path (CONTRACT.md, "What an
//! Application is handed": the first two bullets, and "Logs": which
//! requests have an entry; and "The local loop, kept running": the same
//! bound under `vibe dev`).
//!
//! - The body is not read before the code asks for it: a request whose code
//!   never reads it is answered though the client has not sent it, and the
//!   unread body is dropped.
//! - Sign-in, sharing and every other check come before the first byte is
//!   read.
//! - A stated `content-length` over 5,368,774,656 bytes is `413` before the
//!   code runs, with no log entry; exactly the bound is taken.
//! - A body with no stated length that passes the bound fails the stream the
//!   code reads, and the platform answers `413` whatever the code does next.
//! - A body from which no byte arrives for 10 seconds while it is being read
//!   fails the stream, and the platform answers `408`. Such a request was
//!   given to the code, so it has its log entry and its records.
//! - The body reaches the code as it arrives: the code can answer with the
//!   first bytes it read while the client still holds the rest back.
//! - The same bound holds in the local run under `vibe dev`.
//!
//! The client is the harness's raw one (`blackbox/src/body.rs`): it holds
//! bytes back, sends them late and watches when the answer's head arrives.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed expectation is a failing test"
)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use blackbox::User;
use blackbox::admin;
use blackbox::body::{BodyEnd, CHUNK, Framing, MIB, REQUEST_BODY_BOUND, Raw, pattern_block};
use blackbox::dev::Dev;
use blackbox::local::{Local, OUTSIDER, local_project};
use blackbox::logs::{page, wait_for_entry};
use blackbox::records::is_from_application_code;
use blackbox::{INSTALLER, Vm, fixture_bundle};
use serde_json::Value;

/// The fixture Application of this file.
const APP: &str = "body-request";

/// How long a test waits for the platform's own answer before it says the
/// platform waited for the body.
const ANSWER_WAIT: Duration = Duration::from_secs(5);

fn bob() -> User {
    User::new("bob").in_groups(&["staff"])
}

fn installer() -> User {
    User::new(INSTALLER)
}

/// A program with the stand-in sign-in and this file's fixture, shared with
/// everyone.
fn vm() -> Vm {
    Vm::with_stand_in(&[(APP, "body-request.js")])
}

/// Every record of request `id` that Application code made (a `printed`, an
/// `uncaught` or a `host-call`). It waits for the request's `request`
/// record, which is written last for its request ("What the log records per
/// request"), so every record of the code is there when it returns.
fn code_records(vm: &Vm, id: &str) -> Vec<Value> {
    vm.records_of_id(id)
        .all
        .into_iter()
        .filter(is_from_application_code)
        .collect()
}

#[test]
fn a_request_whose_body_the_code_never_reads_is_answered_though_the_client_has_not_sent_it() {
    let vm = vm();
    let mut raw = Raw::connect(vm.address());
    // The head states a MiB; not one byte of the body is sent, and the code
    // never reads it.
    raw.send_head(
        "POST",
        "/never-reads",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(MIB),
        &[],
    );
    let head = raw.read_head(ANSWER_WAIT);
    assert_eq!(
        head.status, 200,
        "expected the code's own answer (200) while the body was never sent, got {}",
        head.status
    );
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(Duration::from_secs(5), |bytes| body
            .extend_from_slice(bytes)),
        BodyEnd::Whole,
        "expected the code's answer whole"
    );
    assert_eq!(body, b"never-reads answered 200");

    // The request was given to the code, so it has its entry, though its body
    // was never sent.
    let (entry, _) = wait_for_entry(&vm.api_as(&installer()), APP, &head.request_id());
    assert_eq!(entry["status"], 200, "{entry}");

    // The unread body is dropped: the request is over, and the program's
    // next answer does not wait for it either.
    let after = vm.get_as(&bob(), APP, "/never-reads");
    after.expect_status(200);
}

/// The head of the answer to a POST whose head states `length` and whose
/// body is never sent.
fn answer_to_held_back_body(vm: &Vm, host: &str, user: Option<&User>, length: u64) -> u16 {
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/never-reads",
        host,
        user,
        Framing::Length(length),
        &[],
    );
    raw.read_head(ANSWER_WAIT).status
}

#[test]
fn sign_in_sharing_and_not_there_are_answered_as_that_whatever_length_is_stated_and_before_a_byte()
{
    // CONTRACT.md: "Sign-in, sharing and every other check still come before
    // the first byte is read", and the stated-length 413 is "after every
    // other check: ... answered as that, whatever length it states". Each
    // case is sent twice: with a MiB stated (under the bound: the answer
    // must not wait for the body) and with a length over the bound.
    let vm = vm();
    let alice = User::new("alice");
    let api = vm.api_as(&alice);
    api.create_application("body-private").expect_status(201);
    api.send_bundle("body-private", &fixture_bundle("body-request.js"))
        .expect_status(201);
    let bob = bob();
    let cases: [(&str, String, Option<&User>, u16); 3] = [
        ("not signed in", vm.host_of(APP), None, 401),
        ("not there", vm.host_of("nothing-here"), Some(&bob), 404),
        (
            "not shared with bob",
            vm.host_of("body-private"),
            Some(&bob),
            403,
        ),
    ];
    for (what, host, user, expected) in &cases {
        for length in [MIB, REQUEST_BODY_BOUND + 1] {
            let status = answer_to_held_back_body(&vm, host, *user, length);
            assert_eq!(
                status, *expected,
                "expected {expected} for a request {what} stating {length} bytes of body (not sent), got {status}"
            );
        }
    }
}

#[test]
fn a_disabled_application_answers_as_disabled_whatever_length_is_stated() {
    // Needs the administrators' Group to disable: its own program.
    let vm = Vm::start(&admin::options());
    let alice = User::new("alice");
    vm.api_as(&alice)
        .create_with_bundle("body-off", &fixture_bundle("body-request.js"));
    let bob = bob();
    let host = vm.host_of("body-off");
    let served = vm.get_as(&bob, "body-off", "/never-reads");
    served.expect_status(200);
    admin::disable_ok(&vm, "body-off");
    let plain = vm.get_as(&bob, "body-off", "/never-reads");
    assert_ne!(plain.status, 200, "the Application is disabled");
    for length in [MIB, REQUEST_BODY_BOUND + 1] {
        let status = answer_to_held_back_body(&vm, &host, Some(&bob), length);
        assert_eq!(
            status, plain.status,
            "expected a disabled Application's answer ({}) for a request stating {length} bytes, got {status}",
            plain.status
        );
    }
}

#[test]
fn a_stated_content_length_over_the_bound_is_413_before_the_code_and_has_no_log_entry() {
    let vm = vm();
    let mut raw = Raw::connect(vm.address());
    // `/read-all` prints a line first, so that "the code ran" would show.
    raw.send_head(
        "POST",
        "/read-all",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(REQUEST_BODY_BOUND + 1),
        &[],
    );
    let head = raw.read_head(ANSWER_WAIT);
    assert_eq!(
        head.status,
        413,
        "expected 413 for a stated content-length of {} (one over the bound), got {}",
        REQUEST_BODY_BOUND + 1,
        head.status
    );
    let id = head.request_id();
    assert_eq!(
        raw.drain_body(Duration::from_secs(5), |_| ()),
        BodyEnd::Whole,
        "expected the platform's 413 answer framed whole"
    );

    // It was answered before the code ran: no record of Application code
    // (read once the request's own `request` record is there, which is
    // written last), and no entry in the Application's logs.
    let code = code_records(&vm, &id);
    assert!(
        code.is_empty(),
        "expected Application code never to run for the request answered 413, got {code:?}"
    );
    // A later request's entry is there once the earlier one's would be.
    let served = vm.get_as(&bob(), APP, "/never-reads");
    wait_for_entry(&vm.api_as(&installer()), APP, &served.request_id());
    let all = page(&vm.api_as(&installer()), APP, "?limit=1000");
    let mine: Vec<&Value> = all
        .entries
        .iter()
        .filter(|e| e["request"].as_str() == Some(id.as_str()))
        .collect();
    assert!(
        mine.is_empty(),
        "expected no log entry for a request answered 413 before the code, got {mine:?}"
    );
}

#[test]
fn a_stated_content_length_of_exactly_the_bound_is_taken_and_answered_without_the_body() {
    let vm = vm();
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/never-reads",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(REQUEST_BODY_BOUND),
        &[],
    );
    let head = raw.read_head(ANSWER_WAIT);
    assert_eq!(
        head.status, 200,
        "expected exactly the bound (5,368,774,656 bytes) to be taken, got {}",
        head.status
    );
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(Duration::from_secs(5), |bytes| body
            .extend_from_slice(bytes)),
        BodyEnd::Whole
    );
    assert_eq!(body, b"never-reads answered 200");
    let (entry, _) = wait_for_entry(&vm.api_as(&installer()), APP, &head.request_id());
    assert_eq!(entry["status"], 200, "{entry}");
}

#[test]
fn the_body_reaches_the_code_as_it_arrives_while_the_client_holds_the_rest_back() {
    let vm = vm();
    let first = pattern_block(CHUNK);
    let mut raw = Raw::connect(vm.address());
    // A MiB stated; only the first 64 KiB is ever sent, and the code answers
    // with exactly those bytes. A collected body could not answer.
    raw.send_head(
        "POST",
        "/first-bytes?n=65536",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(MIB),
        &[],
    );
    raw.write(&first).expect("the first bytes are written");
    let head = raw.read_head(ANSWER_WAIT);
    assert_eq!(
        head.status, 200,
        "expected the code's answer with the first bytes while the client holds the rest back, got {}",
        head.status
    );
    assert_eq!(
        head.content_length(),
        Some(65536),
        "expected the code's 64 KiB answer"
    );
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(Duration::from_secs(5), |bytes| body
            .extend_from_slice(bytes)),
        BodyEnd::Whole
    );
    assert_eq!(body, first, "expected exactly the bytes the code read");
}

/// The two lines the `/read-all` fixture prints when its stream fails: the
/// first when it begins, the second when the read throws.
fn failed_stream_lines() -> Value {
    serde_json::json!([
        { "level": "log", "message": "reading the body", "loading": false },
        { "level": "log", "message": "the body failed", "loading": false }
    ])
}

#[test]
fn a_body_given_back_unread_is_cut_once_the_request_has_passed_the_bound() {
    // CONTRACT.md: no test reads 5 GiB through the code (its CPU limit ends
    // it); "the bound for a body of no stated length is shown where the host
    // does the reading: the body given back and sent by the host is cut once
    // the request has passed the bound". Not measured yet (today's program
    // refuses long before): 5.4 GB through loopback twice (in, out) in 4 MiB
    // chunks should take 10 to 30 seconds; the whole test is kept inside 45
    // seconds by the 40-second limit on the reading, and the writer, which
    // runs on a thread, stops by itself when the program closes.
    let vm = Vm::with_stand_in(&[("body-echo", "body-response.js")]);
    let piece = pattern_block(usize::try_from(4 * MIB).expect("fits"));
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/echo-unread",
        &vm.host_of("body-echo"),
        Some(&bob()),
        Framing::Chunked,
        &[],
    );
    let mut writer = raw.split_writer();
    let written = AtomicU64::new(0);
    let writer_ended = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    let (head, received, end, writer_error, at_end) = thread::scope(|scope| {
        let writing = scope.spawn(|| {
            let mut error = None;
            while !stop.load(Ordering::Relaxed) {
                if let Err(problem) = writer.send_chunk(&piece) {
                    error = Some(problem);
                    break;
                }
                written.fetch_add(4 * MIB, Ordering::Relaxed);
            }
            writer_ended.store(true, Ordering::Relaxed);
            error
        });
        let head = raw.try_read_head(Duration::from_secs(40));
        let mut received: u64 = 0;
        let end = if head.is_ok() {
            Some(raw.drain_body(Duration::from_secs(40), |bytes| {
                received += u64::try_from(bytes.len()).expect("a piece below usize");
            }))
        } else {
            None
        };
        // The writer sees the connection go by itself; give it time to.
        let waited = Instant::now();
        while !writer_ended.load(Ordering::Relaxed) && waited.elapsed() < Duration::from_secs(12) {
            thread::sleep(Duration::from_millis(50));
        }
        let at_end = written.load(Ordering::Relaxed);
        stop.store(true, Ordering::Relaxed);
        raw.go_away();
        let error = writing.join().expect("the writer's thread ended");
        (head, received, end, error, at_end)
    });
    let head = head.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        head.status, 200,
        "expected the echo's head, got {}",
        head.status
    );
    assert!(head.chunked(), "expected the body sent chunked");
    assert_eq!(head.content_length(), None);
    assert_eq!(
        end,
        Some(BodyEnd::ClosedShort),
        "expected the response ended without a clean end once the request passed the bound ({received} bytes received, {at_end} written)"
    );
    assert!(
        received <= REQUEST_BODY_BOUND,
        "expected no more than the bound ({REQUEST_BODY_BOUND}) back, got {received}"
    );
    assert!(
        received >= REQUEST_BODY_BOUND - 64 * MIB,
        "expected the body cut at the bound, not long before it: {received} bytes came back"
    );
    assert!(
        writer_error.is_some(),
        "expected the client's writer to see the connection go ({at_end} bytes written)"
    );
    // The head was decided before the body: its entry has the head's status.
    let (entry, _) = wait_for_entry(&vm.api_as(&installer()), "body-echo", &head.request_id());
    assert_eq!(entry["status"], 200, "{entry}");
}

fn printed_records(vm: &Vm, id: &str) -> Vec<Value> {
    code_records(vm, id)
        .into_iter()
        .filter(|r| r["event"] == "printed")
        .collect()
}

#[test]
fn a_body_that_sends_no_byte_for_ten_seconds_is_408_with_its_log_entry_and_records() {
    // The platform waits 10 seconds without a byte; the test needs about 12.
    let vm = vm();
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/read-all",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Chunked,
        &[],
    );
    raw.send_chunk(&pattern_block(16))
        .expect("one small chunk is written");
    let last_byte_at = Instant::now();
    // The platform must wait the 10 seconds the contract gives, then answer.
    let head = raw.read_head(Duration::from_secs(30));
    let waited = last_byte_at.elapsed();
    assert_eq!(
        head.status, 408,
        "expected 408 after 10 s without a byte, got {}",
        head.status
    );
    assert!(
        waited >= Duration::from_secs(9),
        "the platform answered 408 after only {waited:?}; the contract gives 10 seconds without a byte"
    );

    // The request was given to the code: its entry and its records are
    // there, and the code saw its stream fail.
    let id = head.request_id();
    let (entry, _) = wait_for_entry(&vm.api_as(&installer()), APP, &id);
    assert_eq!(entry["status"], 408, "{entry}");
    assert_eq!(
        entry["printed"],
        failed_stream_lines(),
        "expected the code to see its stream fail, and its lines in the entry of a request whose body failed, {entry}"
    );
    assert_eq!(
        printed_records(&vm, &id).len(),
        2,
        "expected the code's two printed records"
    );
}

#[test]
fn a_body_that_never_goes_quiet_for_ten_seconds_is_read_whole_however_long_it_takes() {
    // About 17 seconds. "A body from which no byte arrives for 10 seconds":
    // an idle limit, not a limit on the whole body. A chunk every 4 seconds
    // for 16 seconds is never quiet for 10.
    let vm = vm();
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/read-all",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Chunked,
        &[],
    );
    // A write that fails is the program having closed: its answer says why.
    let mut write_failed = None;
    for _ in 0..4 {
        if let Err(problem) = raw.send_chunk(&pattern_block(16)) {
            write_failed = Some(problem);
            break;
        }
        thread::sleep(Duration::from_secs(4));
    }
    if write_failed.is_none() {
        write_failed = raw.finish_chunks().err();
    }
    let head = raw.read_head(Duration::from_secs(10));
    assert_eq!(
        head.status, 200,
        "expected the body read whole (200), got {} (a write failed: {write_failed:?})",
        head.status
    );
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(Duration::from_secs(5), |b| body.extend_from_slice(b)),
        BodyEnd::Whole
    );
    let answer: Value = serde_json::from_slice(&body).expect("the code's JSON");
    assert_eq!(answer["bytes"], 64, "{answer}");
    assert_eq!(answer["error"], Value::Null, "{answer}");
}

#[test]
fn a_body_nobody_is_reading_is_not_timed_out_while_the_code_runs_for_longer_than_ten_seconds() {
    // About 12 seconds. The 10 seconds are for a body "while it is being
    // read": the code that never asks for it, and runs 12 seconds, is
    // answered as it answers (200), not `408`, though no byte of the stated
    // MiB arrives.
    let vm = vm();
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/sleep?ms=12000",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(MIB),
        &[],
    );
    let head = raw.read_head(Duration::from_secs(25));
    assert_eq!(
        head.status, 200,
        "expected the code's own 200 after 12 s with its body never read, got {}",
        head.status
    );
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(Duration::from_secs(5), |b| body.extend_from_slice(b)),
        BodyEnd::Whole
    );
    assert_eq!(body, b"slept");
    let (entry, _) = wait_for_entry(&vm.api_as(&installer()), APP, &head.request_id());
    assert_eq!(entry["status"], 200, "{entry}");
}

#[test]
fn a_body_the_code_asked_for_and_then_stopped_reading_fails_after_ten_quiet_seconds() {
    // About 13 seconds. "Once the code has asked for it, the platform reads
    // a little ahead (at most 4 MiB) and waits for more only while that
    // room is not full": the code reads one chunk and waits 12 seconds; the
    // client sends 16 bytes and nothing more, so the platform is waiting for
    // a byte it has room for, and after 10 seconds the body fails. The code
    // goes on to its end (12 s), and the `408` is sent in place of its 200.
    let vm = vm();
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/read-one-then-sleep?ms=12000",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Chunked,
        &[],
    );
    raw.send_chunk(&pattern_block(16))
        .expect("a chunk is written");
    let head = raw.read_head(Duration::from_secs(30));
    assert_eq!(
        head.status, 408,
        "expected 408 for a body the platform waited 10 s for after the code asked for it, got {}",
        head.status
    );
    let id = head.request_id();
    let (entry, _) = wait_for_entry(&vm.api_as(&installer()), APP, &id);
    assert_eq!(entry["status"], 408, "{entry}");
    assert_eq!(
        entry["printed"],
        serde_json::json!([
            { "level": "log", "message": "asked for the body", "loading": false },
            { "level": "log", "message": "done waiting", "loading": false }
        ]),
        "expected the code to go on to its end after its body failed, {entry}"
    );
}

#[test]
fn a_body_nobody_asked_for_is_not_taken_from_the_client() {
    // About 4 seconds. A client sends a 64 MiB body to a route that sleeps 3
    // seconds without touching it. "A body nobody has asked for yet is not
    // read": the client's writes block once the socket buffers (about 10 MB
    // on loopback) are full, so far fewer than 64 MiB are accepted by the
    // time the answer comes. 32 MiB leaves a wide margin for the buffers.
    let vm = vm();
    let total: u64 = 64 * MIB;
    let block = pattern_block(CHUNK);
    let mut raw = Raw::connect(vm.address());
    raw.send_head(
        "POST",
        "/sleep?ms=3000",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(total),
        &[],
    );
    let mut writer = raw.split_writer();
    let accepted = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let (head, at_answer) = thread::scope(|scope| {
        scope.spawn(|| {
            let step = u64::try_from(CHUNK).expect("fits");
            while !stop.load(Ordering::Relaxed) && accepted.load(Ordering::Relaxed) < total {
                if writer.write(&block).is_err() {
                    return;
                }
                accepted.fetch_add(step, Ordering::Relaxed);
            }
        });
        let head = raw.try_read_head(Duration::from_secs(20));
        let at_answer = accepted.load(Ordering::Relaxed);
        stop.store(true, Ordering::Relaxed);
        raw.go_away();
        (head, at_answer)
    });
    let head = head.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(head.status, 200, "got {}", head.status);
    assert!(
        at_answer < total / 2,
        "expected the program not to take a body nobody asked for ({} MiB of 64 were accepted by the time the code answered)",
        at_answer / MIB
    );
}

#[test]
fn an_unread_body_the_client_had_not_finished_sending_closes_the_connection_after_the_answer() {
    // About 1 second. "The unread body is dropped, and the connection is
    // closed after the answer when the client had not finished sending it."
    // (The platform's own close; the code's `connection` header is never
    // honoured, `body_framing.rs`.) The client states a MiB, sends none.
    let vm = vm();
    let mut raw = Raw::connect_keep_alive(vm.address());
    raw.send_head(
        "POST",
        "/never-reads",
        &vm.host_of(APP),
        Some(&bob()),
        Framing::Length(MIB),
        &[],
    );
    let head = raw.read_head(ANSWER_WAIT);
    assert_eq!(head.status, 200, "got {}", head.status);
    let mut body = Vec::new();
    assert_eq!(
        raw.drain_body(Duration::from_secs(5), |b| body.extend_from_slice(b)),
        BodyEnd::Whole
    );
    assert_eq!(body, b"never-reads answered 200");
    assert!(
        raw.wait_closed_without_taking(Duration::from_secs(5))
            .is_some(),
        "expected the connection closed after the answer: the client had not finished sending the body"
    );
}

#[test]
fn the_same_bound_of_a_requests_body_holds_in_the_local_run_under_vibe_dev() {
    // (A request "`vibe dev` passes on" is not told apart here: the tests of
    // the local loop reach `vibe dev` by its HTTP address, which is how a
    // browser or `vibe request` is passed on; `vibe request` itself takes at
    // most 1 MiB of body, so it cannot state a length over the bound.)
    // Starting `vibe dev` builds the project and starts the private
    // Postgres, as the local loop's own tests do: it is the slow part.
    let local = Local::new();
    let project = local_project(&local.machine, "body-dev", &[]);
    let dev = Dev::start(&local, &project.dir, "body-dev");
    let cookie = format!("vm_test_user={OUTSIDER}");
    let at_bound = usize::try_from(REQUEST_BODY_BOUND).expect("fits in usize");
    let over_bound = usize::try_from(REQUEST_BODY_BOUND + 1).expect("fits in usize");

    // Over the bound: 413 before the code, and no `request` line (the line
    // is for a request the code was given).
    let over = dev
        .request("/")
        .method("POST")
        .header("cookie", &cookie)
        .declare_length(over_bound)
        .send();
    over.expect_status(413);

    // Exactly the bound: taken, and answered by the code without the body.
    let exact = dev
        .request("/")
        .method("POST")
        .header("cookie", &cookie)
        .declare_length(at_bound)
        .send();
    exact.expect_status(200);
    assert_eq!(
        exact.json()["user"],
        serde_json::json!(OUTSIDER),
        "expected the code's own answer for a body of exactly the bound"
    );

    // `vibe dev` writes the line of a request when it ends, not before its
    // answer: wait for the line of the second request (up to 10 seconds).
    // Lines come in order, so the first request's line, had there been one,
    // is there by then: the second's is the only one.
    let exact_id = serde_json::json!(exact.request_id());
    let began = Instant::now();
    while !dev
        .events("request")
        .iter()
        .any(|line| line["request_id"] == exact_id)
    {
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "expected a `request` line for the request the code was given within 10 s\n{}",
            dev.stdout()
        );
        thread::sleep(Duration::from_millis(100));
    }
    let lines = dev.events("request");
    assert_eq!(
        lines.len(),
        1,
        "expected a `request` line for the request the code was given and none for the one answered before the code, got {lines:?}"
    );

    dev.stop("INT").expect_code(0);
}
